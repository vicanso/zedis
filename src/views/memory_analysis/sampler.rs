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

//! Online keyspace sampling (`SCAN` + `MEMORY USAGE` + `TTL` + `OBJECT`) and
//! offline RDB dump streaming for [`ZedisMemoryAnalysis`].

use super::*;

/// Where an offline analysis reads its dump from: the reader, and its length
/// for the progress bar. A path streamed from disk on the desktop, an upload
/// held in memory in a page.
type RdbSource = Box<dyn FnOnce() -> Result<(Box<dyn std::io::Read + Send>, u64), String> + Send>;

/// The largest dump a page reads: it is held whole in the tab's memory.
#[cfg(target_family = "wasm")]
const MAX_WEB_RDB_BYTES: u64 = 256 * 1024 * 1024;

impl ZedisMemoryAnalysis {
    /// Refresh the `INFO keysizes` card — one tiny INFO, exact counts, no
    /// sampling. Servers without the section (pre-8 Redis, Valkey) are not
    /// asked at all, and failures (restricted INFO) stay silent: the card
    /// simply doesn't render without data.
    pub(super) fn fetch_keysizes(&mut self, cx: &mut gpui::Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = state.db();
        self.keysizes_task = Some(cx.spawn(async move |handle, cx| {
            let result = cx
                .background_spawn(async move {
                    Ok::<Vec<KeysizesDist>, Error>(key_size_distributions(&ServerDb::new(&*server_id, db)).await?)
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.keysizes_task = None;
                if let Ok(dists) = result {
                    this.keysizes_selected = this.keysizes_selected.min(dists.len().saturating_sub(1));
                    this.keysizes = dists;
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn stop_analysis(&mut self, cx: &mut gpui::Context<Self>) {
        self.analysis_task.take();
        self.status = if self.prefix_count > 0 || self.single_count > 0 || !self.schema.is_empty() {
            AnalysisStatus::Finished
        } else {
            AnalysisStatus::Idle
        };
        if self.rdb_file.is_none() && self.status == AnalysisStatus::Finished {
            self.spawn_schema_probe(cx);
        }
        cx.notify();
    }

    pub(super) fn spawn_schema_probe(&mut self, cx: &mut gpui::Context<Self>) {
        let probe_targets = self.schema.top_probe_targets(25);
        if probe_targets.is_empty() {
            return;
        }
        let state = self.server_state.read(cx);
        let probe_server_id = state.server_id().to_string();
        if probe_server_id.is_empty() {
            return;
        }
        let db = state.db();
        self.schema_probe_task = Some(cx.spawn(async move |handle, cx| {
            let probed = cx
                .background_spawn(async move {
                    probe_pattern_fields(&ServerDb::new(&*probe_server_id, db), probe_targets).await
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.schema_probe_task = None;
                for (pattern, fields) in probed {
                    this.schema.record_structure_fields(&pattern, &fields);
                }
                cx.notify();
            });
        }));
    }

    /// Shared reset for both analysis sources (online SCAN / RDB file):
    /// clears every accumulator, table, and prior advice, and flips the
    /// tables' offline flag to match the new source.
    pub(super) fn reset_for_run(&mut self, offline: bool, cx: &mut gpui::Context<Self>) {
        // A fresh run invalidates any previous AI advice.
        self.clear_ai_result(cx);
        self.status = AnalysisStatus::Running;
        self.scan_error = None;
        self.progress = "0%".into();
        self.progress_value = 0;
        self.prefix_count = 0;
        self.single_count = 0;
        self.single_groups = SingleKeyTopGroups::new(TOP_N);
        // Reset histogram so a re-run starts from zero. The TTL tab
        // re-renders each round via the snapshot below, so leaving
        // stale data here would briefly show the old bars on top of
        // a partial new run.
        self.ttl_histogram = TtlHistogram::default();
        // Stale advice from a previous run is worse than none — clear it so
        // the panel hides until the fresh run completes and recomputes.
        self.recommendations.clear();

        self.offline.set(offline);
        // A run in flight has no map to drill through yet.
        self.drillable.set(false);
        self.prefix_map.clear();
        self.prefix_root = SharedString::default();
        self.type_map.clear();
        self.type_rows.clear();
        self.prefix_rows.clear();
        self.single_rows.clear();
        self.schema = SchemaAccumulator::new();
        self.schema_probe_task.take();
        self.prefix_table.update(cx, |s, _| s.delegate_mut().clear());
        self.single_table.update(cx, |s, _| s.delegate_mut().clear());
    }

    /// The live sampler's hard dependencies (`SCAN` + `MEMORY USAGE`) — the
    /// first one this server lacks, if any. Offline RDB analysis doesn't care.
    pub(crate) fn live_scan_block(&self, cx: &gpui::App) -> Option<(ServerCommand, CommandStatus)> {
        self.server_state
            .read(cx)
            .features()
            .first_unusable(&[ServerCommand::Scan, ServerCommand::MemoryUsage])
    }

    pub(super) fn start_analysis(&mut self, cx: &mut gpui::Context<Self>) {
        self.reset_for_run(false, cx);
        self.rdb_file = None;
        self.fetch_keysizes(cx);

        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        self.analyzed = Some((server_id.clone(), db));
        // One extra O(1) command per sampled key, skipped entirely on a
        // server the probe found unable to run it.
        let with_encoding = server_state.features().is_usable(ServerCommand::ObjectEncoding);
        let prefix_table = self.prefix_table.clone();
        let single_table = self.single_table.clone();
        let key_separator = self.server_state.read(cx).key_separator().to_string();
        let ratio = self.ratio;
        self.run_ratio = Some(ratio);
        let dbsize = self.dbsize.unwrap_or(0);
        let scan_count = self.scan_count;

        self.analysis_task = Some(cx.spawn(async move |handle, cx| {
            debug!(dbsize, ratio, "Memory analysis: using sample ratio");

            // Detect maxmemory-policy once at the start of the run. If the
            // server changes policy mid-scan we'd see mixed FREQ/IDLETIME
            // results — accept that as a one-frame visual glitch rather than
            // re-fetching every round.
            let (policy, heat) = match cx
                .background_spawn({
                    let server_id = server_id.clone();
                    async move {
                        let p = maxmemory_policy(&ServerDb::new(&*server_id, db)).await?;
                        Ok::<String, Error>(p)
                    }
                })
                .await
            {
                Ok(p) => {
                    let h = HeatProbe::from_policy(&p);
                    (p, h)
                }
                Err(_) => (String::new(), HeatProbe::None),
            };
            let policy_shared: SharedString = policy.clone().into();
            let _ = handle.update(cx, |this, _| {
                this.policy = policy_shared.clone();
                this.heat = heat;
                // Hot/Cold modes only meaningful with a heat probe — fall back
                // to Size if the user previously selected a hot/cold view on
                // a server that doesn't expose either metric.
                if heat == HeatProbe::None && this.sort_mode != SortMode::Size {
                    this.sort_mode = SortMode::Size;
                }
                // The probe decides whether Hot/Cold are offered at all, so the
                // dropdown's items follow it.
                this.should_rebuild_sort_items = Some(true);
            });

            let mut acc = AnalysisAccumulators::new();
            let mut cursors: Option<Vec<u64>> = None;
            let mut analysis_count: u64 = 0;
            // Set when a SCAN round errors; carried out of the loop so the
            // final update surfaces the failure instead of a fake "100%".
            let mut scan_error: Option<SharedString> = None;
            let redis_process_ratio = 0.5;
            let min_sleep = Duration::from_micros(500);
            let max_sleep = Duration::from_millis(20);
            // GPUI's timer rather than smol's: the background block cannot
            // reach `cx`, and smol has no browser backend (ADR 9).
            let executor = cx.background_executor().clone();

            loop {
                let scan_task = cx.background_spawn({
                    let server_id = server_id.clone();
                    let cursors_clone = cursors.clone();
                    let executor = executor.clone();
                    async move {
                        let start = Instant::now();
                        let at = ServerDb::new(&*server_id, db);
                        let (count, new_cursors, keys_memory_usage) =
                            sample_memory_usage(&at, ratio, scan_count, cursors_clone, heat, with_encoding).await?;
                        let base_sleep = start.elapsed().mul_f64(redis_process_ratio);
                        let sleep_duration = base_sleep.clamp(min_sleep, max_sleep);
                        executor.timer(sleep_duration).await;
                        Ok::<(u64, Vec<u64>, Vec<KeyMemoryUsage>), Error>((count, new_cursors, keys_memory_usage))
                    }
                });

                let (count, new_cursors, keys_memory_usage) = match scan_task.await {
                    Ok(result) => result,
                    Err(e) => {
                        error!(error = %e, "Failed to sample scan for memory analysis");
                        scan_error = Some(e.to_string().into());
                        break;
                    }
                };
                analysis_count += count;

                if keys_memory_usage.is_empty() && new_cursors.iter().all(|c| *c == 0) {
                    break;
                }

                // Classify, accumulate and snapshot on the background pool:
                // the prefix rows are rebuilt from the whole map each round
                // (up to 100k prefixes), which on the UI thread stalled every
                // round's repaint.
                let (next_acc, prefix_rows, groups_snapshot, ttl_snapshot, type_snapshot, schema_snapshot) = cx
                    .background_spawn({
                        let key_separator = key_separator.clone();
                        let mut acc = acc;
                        async move {
                            for item in &keys_memory_usage {
                                acc.add(
                                    KeySample {
                                        key: &item.key,
                                        memory_bytes: item.memory_usage,
                                        ttl: item.ttl,
                                        key_type: &item.key_type,
                                        encoding: &item.encoding,
                                        heat: item.heat,
                                    },
                                    heat,
                                    &key_separator,
                                );
                            }
                            // A run always shows the top level: the drill-down
                            // needs the finished map, which only the final
                            // update hands over.
                            let prefix_rows = build_prefix_rows(&acc.prefix_map, ratio, &key_separator, "");
                            let groups = acc.single_groups.clone();
                            let ttl = acc.ttl_histogram.clone();
                            let types = acc.type_map.clone();
                            let schema = acc.schema.clone();
                            (acc, prefix_rows, groups, ttl, types, schema)
                        }
                    })
                    .await;
                acc = next_acc;

                // Update progress
                let pct = if analysis_count > 0 && dbsize > 0 {
                    ((analysis_count as f32 / dbsize as f32) * 100.0).min(99.0) as u32
                } else {
                    99
                };
                let progress_text: SharedString = format!("{}%", pct).into();
                let pc = prefix_rows.len();
                let _ = handle.update(cx, |this, cx| {
                    this.progress = progress_text;
                    this.progress_value = pct;
                    this.prefix_count = pc;
                    this.single_groups = groups_snapshot;
                    this.ttl_histogram = ttl_snapshot;
                    this.type_map = type_snapshot;
                    this.schema = schema_snapshot;
                    this.rebuild_type_rows(cx);
                    let mode = this.sort_mode;
                    let single_rows = this.single_groups.rows_for(mode);
                    this.single_count = single_rows.len();
                    prefix_table.update(cx, |s, _| {
                        s.delegate_mut()
                            .set_rows(prefix_rows.iter().map(PrefixRow::cells).collect());
                    });
                    single_table.update(cx, |s, _| {
                        s.delegate_mut()
                            .set_rows(single_rows.iter().map(SingleKeyRow::cells).collect());
                    });
                    this.prefix_rows = prefix_rows;
                    this.single_rows = single_rows;
                    cx.notify();
                });

                if new_cursors.iter().all(|c| *c == 0) {
                    break;
                }

                cursors = Some(new_cursors);
            }

            // Final update
            let prefix_rows = build_prefix_rows(&acc.prefix_map, ratio, &key_separator, "");
            let prefix_memory = top_level_prefix_memory(&acc.prefix_map, ratio, &key_separator);
            let pc = prefix_rows.len();
            let final_groups = acc.single_groups;
            let final_histogram = acc.ttl_histogram;
            let final_prefix_map = acc.prefix_map;
            let final_type_map = acc.type_map;
            let final_schema = acc.schema;
            let _ = handle.update(cx, |this, cx| {
                if let Some(err) = scan_error {
                    // Surface the failure instead of a fake "100% / Finished".
                    // `progress` keeps the last percentage the scan reached,
                    // and any rows accumulated before the error still render.
                    this.status = AnalysisStatus::Error;
                    this.scan_error = Some(err);
                    this.recommendations.clear();
                } else {
                    this.status = AnalysisStatus::Finished;
                    this.progress = "100%".into();
                    this.progress_value = 100;
                    // Offline rule engine — free + instant, runs on the
                    // just-computed aggregates before they're moved into
                    // `this` below. Big-key detection uses `by_size` (the
                    // global biggest keys), independent of the table's sort.
                    let frag = latest_fragmentation(this.server_state.read(cx).server_id());
                    this.recommendations = build_recommendations(
                        &this.policy,
                        &prefix_rows,
                        &final_groups.by_size.items,
                        &final_histogram,
                        frag,
                        Some(prefix_memory),
                    );
                }
                this.prefix_count = pc;
                this.single_groups = final_groups;
                this.ttl_histogram = final_histogram;
                this.prefix_map = final_prefix_map;
                this.type_map = final_type_map;
                this.schema = final_schema;
                this.spawn_schema_probe(cx);
                this.rebuild_type_rows(cx);
                // The map is on the view now, so rows can be drilled into.
                this.drillable.set(true);
                let mode = this.sort_mode;
                let single_rows = this.single_groups.rows_for(mode);
                this.single_count = single_rows.len();
                prefix_table.update(cx, |s, _| {
                    s.delegate_mut()
                        .set_rows(prefix_rows.iter().map(PrefixRow::cells).collect());
                });
                single_table.update(cx, |s, _| {
                    s.delegate_mut()
                        .set_rows(single_rows.iter().map(SingleKeyRow::cells).collect());
                });
                this.prefix_rows = prefix_rows;
                this.single_rows = single_rows;
                this.publish_report_finding(cx);
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// Toolbar entry point for the offline analysis: pick a local RDB
    /// dump, then parse it. Extension is not filterable in the native
    /// dialog — a non-RDB pick fails fast on the `REDIS` magic check.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn handle_pick_rdb(&mut self, cx: &mut gpui::Context<Self>) {
        if self.status == AnalysisStatus::Running {
            return;
        }
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |handle, cx| {
            let result = receiver.await;
            let _ = handle.update(cx, |this, cx| {
                let paths = match crate::views::export::dialog_answer(result) {
                    crate::views::export::DialogAnswer::Chosen(paths) => paths,
                    crate::views::export::DialogAnswer::Cancelled => return,
                    crate::views::export::DialogAnswer::Failed(detail) => {
                        crate::views::export::note_file_dialog_failure(cx, &detail);
                        return;
                    }
                };
                if let Some(path) = paths.into_iter().next() {
                    let file_name: SharedString = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                        .into();
                    // Streamed from disk, so a multi-GB dump is never in
                    // memory whole.
                    let open: RdbSource = Box::new(move || {
                        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                        let file_len = file.metadata().map_err(|e| e.to_string())?.len();
                        Ok((Box::new(std::io::BufReader::new(file)), file_len))
                    });
                    this.start_rdb_analysis(file_name, open, cx);
                }
            });
        })
        .detach();
    }

    /// [`Self::handle_pick_rdb`] in a page: the browser's file picker, the
    /// dump read whole into the tab — so up to [`MAX_WEB_RDB_BYTES`], with
    /// the desktop app named for anything larger.
    #[cfg(target_family = "wasm")]
    pub(super) fn handle_pick_rdb(&mut self, cx: &mut gpui::Context<Self>) {
        if self.status == AnalysisStatus::Running {
            return;
        }
        let picked = open_file(cx, ".rdb", MAX_WEB_RDB_BYTES);
        cx.spawn(async move |handle, cx| {
            let Some(picked) = picked.await else {
                return;
            };
            let _ = handle.update(cx, |this, cx| match picked {
                Ok(file) => {
                    let len = file.bytes.len() as u64;
                    let bytes = file.bytes;
                    let open: RdbSource = Box::new(move || Ok((Box::new(std::io::Cursor::new(bytes)), len)));
                    this.start_rdb_analysis(file.name.into(), open, cx);
                }
                Err(failure) => {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    let message = match failure {
                        PickFailure::TooLarge { size } => rust_i18n::t!(
                            "memory_analysis.rdb_too_large_web",
                            size = format_memory(size),
                            limit = format_memory(MAX_WEB_RDB_BYTES),
                            locale = &locale
                        )
                        .to_string(),
                        PickFailure::Unreadable(e) => e,
                    };
                    this.status = AnalysisStatus::Error;
                    this.scan_error = Some(message.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Offline analysis: stream-parse a local RDB dump into the same
    /// accumulators the online SCAN feeds. No Redis round-trips — sizes
    /// are serialized bytes from the file (relative magnitudes are
    /// faithful; live `MEMORY USAGE` overhead is not included), there is
    /// no heat metric, no sampling (ratio 1.0), and per-key TTLs become
    /// "remaining vs now".
    fn start_rdb_analysis(&mut self, file_name: SharedString, open: RdbSource, cx: &mut gpui::Context<Self>) {
        self.reset_for_run(true, cx);
        self.rdb_file = Some(file_name);
        // Offline source has no policy / heat probe; force the rank
        // dropdown back to Size-only.
        self.policy = SharedString::default();
        self.heat = HeatProbe::None;
        if self.sort_mode != SortMode::Size {
            self.sort_mode = SortMode::Size;
        }
        self.should_rebuild_sort_items = Some(true);

        let prefix_table = self.prefix_table.clone();
        let single_table = self.single_table.clone();
        let key_separator = self.server_state.read(cx).key_separator().to_string();

        // Entries parsed per UI round: large enough that a multi-GB dump
        // isn't throttled by foreground updates, small enough that
        // progress stays lively.
        const RDB_CHUNK: usize = 20_000;

        self.analysis_task = Some(cx.spawn(async move |handle, cx| {
            let opened = cx
                .background_spawn(async move {
                    let (reader, file_len) = open()?;
                    let parser = RdbParser::new(reader).map_err(|e| e.to_string())?;
                    Ok::<_, String>((parser, file_len))
                })
                .await;
            let (mut parser, file_len) = match opened {
                Ok(v) => v,
                Err(e) => {
                    error!(error = %e, "failed to open RDB file");
                    let _ = handle.update(cx, |this, cx| {
                        this.status = AnalysisStatus::Error;
                        this.scan_error = Some(e.into());
                        cx.notify();
                    });
                    return;
                }
            };

            let mut acc = AnalysisAccumulators::new();
            let now_ms = unix_ts_millis();
            let mut parse_error: Option<SharedString> = None;

            loop {
                // Parse one chunk on the background pool; the parser moves
                // into the round and comes back with it so the foreground
                // task keeps ownership between rounds.
                let round = cx.background_spawn(async move {
                    let mut entries = Vec::with_capacity(RDB_CHUNK);
                    let mut done = false;
                    loop {
                        match parser.next_entry() {
                            Ok(Some(entry)) => {
                                entries.push(entry);
                                if entries.len() >= RDB_CHUNK {
                                    break;
                                }
                            }
                            Ok(None) => {
                                done = true;
                                break;
                            }
                            Err(e) => return Err(e.to_string()),
                        }
                    }
                    let offset = parser.bytes_read();
                    Ok::<_, String>((parser, entries, offset, done))
                });
                let (returned_parser, entries, offset, done) = match round.await {
                    Ok(v) => v,
                    Err(e) => {
                        error!(error = %e, "RDB parse failed");
                        parse_error = Some(e.into());
                        break;
                    }
                };
                parser = returned_parser;

                for entry in &entries {
                    let ttl = match entry.expire_at_ms {
                        // A dump can hold keys that expired after it was
                        // written — clamp to 0 ("expiring now").
                        Some(at) => ((at - now_ms) / 1000).max(0),
                        None => -1,
                    };
                    acc.add(
                        KeySample {
                            key: &entry.key,
                            memory_bytes: entry.serialized_bytes,
                            ttl,
                            key_type: entry.key_type.as_ref(),
                            encoding: entry.encoding,
                            heat: HeatMetric::None,
                        },
                        HeatProbe::None,
                        &key_separator,
                    );
                }

                let pct = if file_len > 0 {
                    ((offset as f64 / file_len as f64) * 100.0).min(99.0) as u32
                } else {
                    99
                };
                let progress_text: SharedString = format!("{}%", pct).into();
                let prefix_rows = build_prefix_rows(&acc.prefix_map, 1.0, &key_separator, "");
                let pc = prefix_rows.len();
                let groups_snapshot = acc.single_groups.clone();
                let ttl_snapshot = acc.ttl_histogram.clone();
                let type_snapshot = acc.type_map.clone();
                let schema_snapshot = acc.schema.clone();
                let updated = handle.update(cx, |this, cx| {
                    this.progress = progress_text;
                    this.progress_value = pct;
                    this.prefix_count = pc;
                    this.type_map = type_snapshot;
                    this.schema = schema_snapshot;
                    this.rebuild_type_rows(cx);
                    this.single_groups = groups_snapshot;
                    this.ttl_histogram = ttl_snapshot;
                    let mode = this.sort_mode;
                    let single_rows = this.single_groups.rows_for(mode);
                    this.single_count = single_rows.len();
                    prefix_table.update(cx, |s, _| {
                        s.delegate_mut()
                            .set_rows(prefix_rows.iter().map(PrefixRow::cells).collect());
                    });
                    single_table.update(cx, |s, _| {
                        s.delegate_mut()
                            .set_rows(single_rows.iter().map(SingleKeyRow::cells).collect());
                    });
                    this.prefix_rows = prefix_rows;
                    this.single_rows = single_rows;
                    cx.notify();
                });
                // View dropped (route change) — stop parsing.
                if updated.is_err() || done {
                    break;
                }
            }

            let prefix_rows = build_prefix_rows(&acc.prefix_map, 1.0, &key_separator, "");
            let prefix_memory = top_level_prefix_memory(&acc.prefix_map, 1.0, &key_separator);
            let pc = prefix_rows.len();
            let final_groups = acc.single_groups;
            let final_histogram = acc.ttl_histogram;
            let final_prefix_map = acc.prefix_map;
            let final_type_map = acc.type_map;
            let final_schema = acc.schema;
            let _ = handle.update(cx, |this, cx| {
                if let Some(err) = parse_error {
                    // Keep whatever parsed before the corruption point.
                    this.status = AnalysisStatus::Error;
                    this.scan_error = Some(err);
                    this.recommendations.clear();
                } else {
                    this.status = AnalysisStatus::Finished;
                    this.progress = "100%".into();
                    this.progress_value = 100;
                    // Offline rules only: no policy, no fragmentation data.
                    this.recommendations = build_recommendations(
                        "",
                        &prefix_rows,
                        &final_groups.by_size.items,
                        &final_histogram,
                        None,
                        Some(prefix_memory),
                    );
                }
                this.prefix_count = pc;
                this.single_groups = final_groups;
                this.ttl_histogram = final_histogram;
                this.prefix_map = final_prefix_map;
                this.type_map = final_type_map;
                this.schema = final_schema;
                this.rebuild_type_rows(cx);
                this.drillable.set(true);
                let mode = this.sort_mode;
                let single_rows = this.single_groups.rows_for(mode);
                this.single_count = single_rows.len();
                prefix_table.update(cx, |s, _| {
                    s.delegate_mut()
                        .set_rows(prefix_rows.iter().map(PrefixRow::cells).collect());
                });
                single_table.update(cx, |s, _| {
                    s.delegate_mut()
                        .set_rows(single_rows.iter().map(SingleKeyRow::cells).collect());
                });
                this.prefix_rows = prefix_rows;
                this.single_rows = single_rows;
                cx.notify();
            });
        }));

        cx.notify();
    }
}
