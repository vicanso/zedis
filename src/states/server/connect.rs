// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Connecting `ZedisServerState` to a Redis server, loading initial topology,
//! and reconnecting after link failures.

use super::*;

impl ZedisServerState {
    /// Select and connect to a Redis server
    ///
    /// This initiates a connection and loads server metadata:
    /// - Database size (DBSIZE)
    /// - Server version
    /// - Latency measurement (PING)
    /// - Cluster node counts
    ///
    /// If query_mode is QueryMode::All, automatically starts scanning all keys.
    ///
    /// # Arguments
    /// * `server_id` - Server id to connect to
    /// * `db` - Database to connect to
    /// * `cx` - Context for spawning async tasks and state updates
    pub fn select(&mut self, server_id: SharedString, db: usize, cx: &mut Context<Self>) {
        // Reload when switching to a different server, OR when re-selecting
        // the *same* server whose previous load failed. The latter matters
        // because going Home clears only the global selection (the empty
        // `ServerSelected` is ignored by content.rs), so `self.server_id`
        // stays stale; without the `Failed` check, re-clicking the server
        // after a network error would be a silent no-op and never retry.
        let same_target = self.server_id == server_id && self.db == db;
        let retry_failed = same_target && matches!(self.link.server_status, RedisServerStatus::Failed);
        if !same_target || retry_failed {
            self.link.start_load();
            // The metrics history is the server's, whichever database is
            // open: a database switch or a retry keeps it (it used to empty
            // the charts), another server drops the last one's.
            if self.server_id != server_id {
                get_metrics_cache().remove_server(self.server_id.as_str());
            }
            // A retry of the same target is a reconnect: an open write
            // window is still the user's. `reset` closes windows because one
            // belongs to the server it was opened on, which this still is —
            // closing it made every reconnect re-lock the entry.
            let open_window = retry_failed.then(|| self.lock.clone());
            // And what the link failed with stays on screen while it is
            // tried again.
            let link_failure = if retry_failed {
                self.link.link_failure.take()
            } else {
                None
            };
            // And what this session's panels found is still this server's.
            let findings = retry_failed.then(|| std::mem::take(&mut self.report_findings));
            self.reset(cx);
            if let Some(findings) = findings {
                self.report_findings = findings;
            }
            if let Some(earlier) = open_window {
                self.lock.keep_window_from(&earlier);
            }
            self.link.link_failure = link_failure;
            self.server_id = server_id.clone();
            self.db = db;
            // Cached matrix from an earlier connect to this server (or the
            // optimistic default); the background probe refines it below.
            self.facts.set_features(get_server_features(server_id.as_str()));
            // Resolve key-tree / scan prefs: per-server override, else Settings.
            let settings = cx.global::<ZedisGlobalStore>().read(cx);
            self.prefs
                .resolve_tree(get_server(server_id.as_str()).ok().as_ref(), settings);

            let (query_mode, soft_wrap) = get_session_option(&server_id)
                .map(|option| {
                    let mode = option
                        .query_mode
                        .as_deref()
                        .and_then(|s| QueryMode::from_str(s).ok())
                        .unwrap_or_default();

                    let wrap = option.soft_wrap.unwrap_or(true);

                    // 返回一个元组，包含所有需要更新的值
                    (mode, wrap)
                })
                .unwrap_or((QueryMode::All, true));
            self.scan.query_mode = query_mode;
            self.prefs.set_soft_wrap(soft_wrap);

            debug!(server_id = self.server_id.as_str(), "Selecting server");
            // In the browser, the tags and notes other accounts set here
            // since the last round, before the tree is read (ADR 17).
            ask_for_fresh_page_data();
            let search_history_manager = get_search_history_manager();
            if let Ok(history) = search_history_manager.records(server_id.as_str()) {
                self.scan.search_history = history.into_iter().map(Into::into).collect();
            }
            cx.emit(ServerEvent::ServerSelected(server_id));

            // Set loading state
            self.link.server_status = RedisServerStatus::Loading;
            self.scan.scanning = true;
            cx.notify();

            let server_id_clone = self.server_id.clone();
            let counting_server_id = server_id_clone.clone();
            let db = self.db;

            self.spawn(
                ServerTask::SelectServer,
                move || async move {
                    // Connects (or reuses the pooled client) and gathers the
                    // server's metadata.
                    Ok(server_summary(&ServerDb::new(server_id_clone.as_str(), db)).await?)
                },
                move |this, result, cx| {
                    // Ignore if user switched to a different server while loading
                    if this.server_id != counting_server_id {
                        return;
                    }

                    match result {
                        Ok(ServerSummary {
                            dbsize,
                            nodes,
                            description: nodes_description,
                            version,
                            databases,
                            access_mode,
                            supports_rejson,
                            supports_search,
                            supports_timeseries,
                            supports_bf,
                        }) => {
                            this.link.load_retries = 0;
                            this.link.link_failure = None;
                            this.readings.set_dbsize(dbsize);
                            this.facts.set_topology(nodes, nodes_description);
                            this.after_topology_loaded(cx);
                            this.facts.note_connect(
                                version.into(),
                                databases,
                                facts::ModuleFlags {
                                    rejson: supports_rejson,
                                    search: supports_search,
                                    timeseries: supports_timeseries,
                                    bf: supports_bf,
                                },
                            );
                            // Stamp the connection for the server list's
                            // "last used" order. Once per successful connect,
                            // and the write itself is a background task.
                            let sid = this.server_id.to_string();
                            if let Ok(mut option) = get_session_option(&sid) {
                                option.last_connected_at = Some(unix_ts());
                                save_session_option(&sid, option, cx);
                            }
                            // A locked entry starts in SafeMode — unless this
                            // connect is a reconnect inside an open window.
                            let access_mode = if access_mode == AccessMode::ReadWrite
                                && this.write_locked()
                                && this.unlock_remaining().is_none()
                            {
                                AccessMode::SafeMode
                            } else {
                                access_mode
                            };
                            this.lock.set_access_mode(access_mode);
                            this.probe_features(false, cx);

                            let server_id = this.server_id.clone();
                            this.link.server_status = RedisServerStatus::Idle;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            let is_exact_mode = this.scan.query_mode == QueryMode::Exact;
                            if is_exact_mode {
                                this.scan.scanning = false;
                            }
                            cx.notify();
                            // One-time onboarding toast on the very first
                            // successful connection: point at the two global
                            // entry points (palette, cross-db search) a new
                            // user won't discover from the key tree alone.
                            if !cx
                                .global::<ZedisGlobalStore>()
                                .read(cx)
                                .hint_dismissed(HINT_FIRST_CONNECT)
                            {
                                update_app_state_and_save_quiet(cx, "dismiss_hint_first_connect", |state, _| {
                                    state.dismiss_hint(HINT_FIRST_CONNECT)
                                });
                                let message = first_connect_hint(cx);
                                this.emit_info_notification(message, cx);
                            }
                            // Auto-scan keys if not exact mode — and not on a
                            // server already known to lack SCAN (the key tree
                            // shows its open-by-name banner instead).
                            if !is_exact_mode && this.command_block(ServerCommand::Scan).is_none() {
                                this.scan_keys(server_id, SharedString::default(), cx);
                            }
                        }
                        Err(error) => {
                            this.note_link_failure(&error);
                            this.link.note_load_failure(error.connection_kind());
                            // Connection / metadata load failed (e.g. network
                            // down). Mark Failed so a later re-select retries
                            // instead of being swallowed by select()'s
                            // same-server guard. The error toast was already
                            // emitted in spawn_with_arg; skip the key scan
                            // since it would just fail again with a 2nd toast.
                            this.link.server_status = RedisServerStatus::Failed;
                            this.scan.scanning = false;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            cx.notify();
                        }
                    }
                },
                cx,
            );
        }
    }

    /// Run the load again that the link failed, now that the heartbeat has
    /// an answer from a server with its dataset in memory. Called on every
    /// such beat; does nothing unless a load is owed.
    pub(super) fn retry_failed_load(&mut self, cx: &mut Context<Self>) {
        if !self.link.take_load_retry() {
            return;
        }
        info!(
            server_id = self.server_id.as_str(),
            attempt = self.link.load_retries,
            "the server answers: loading what the failed connect could not"
        );
        let server_id = self.server_id.clone();
        let db = self.db;
        self.select(server_id, db, cx);
    }

    /// Force a reconnect of the currently-selected server/db.
    ///
    /// The heartbeat marks a live connection `Offline` after repeated PING
    /// failures but leaves `server_status` as `Idle` (the initial load
    /// succeeded), so [`select`](Self::select)'s same-target guard would treat
    /// a re-select as a silent no-op. Flipping the status to `Failed` first
    /// makes the existing `retry_failed` path fire, reusing the full reload
    /// logic (reset, metadata fetch, key scan) instead of duplicating it.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        self.link.server_status = RedisServerStatus::Failed;
        self.select(server_id, db, cx);
    }

    #[cfg(not(target_family = "wasm"))]
    fn after_topology_loaded(&mut self, cx: &mut Context<Self>) {
        self.note_sentinel_master_choice(cx);
        if self.nodes_description().server_type == "Sentinel" {
            self.start_sentinel_watch(cx);
        }
    }

    #[cfg(target_family = "wasm")]
    fn after_topology_loaded(&mut self, _cx: &mut Context<Self>) {}
}
