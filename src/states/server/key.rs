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

use super::{
    ServerEvent, ServerTask, ZedisServerState,
    hash::first_load_hash_value,
    json::get_redis_json_value,
    list::first_load_list_value,
    set::first_load_set_value,
    stream::first_load_stream_value,
    string::get_redis_bytes_value,
    value::{KeyType, RedisValue, RedisValueData, RedisValueStatus, SortOrder},
    zset::first_load_zset_value,
};
use crate::states::{QueryMode, ZedisGlobalStore};
use crate::{
    connection::get_connection_manager,
    error::Error,
    helpers::{parse_duration, unix_ts},
};
use ahash::AHashSet;
use futures::stream::{self, StreamExt};
use gpui::{SharedString, prelude::*};
use redis::{cmd, pipe};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;
use uuid::Uuid;

/// Hard ceiling on the per-page target after the per-master multiplier, so a
/// large cluster can't try to pull (and client-side tree-build) an unbounded
/// batch in a single load. "Load more" still fetches keys beyond this.
const SCAN_RESULT_MAX_CAP: usize = 100_000;

/// Max SCAN pages a single prefix-scan batch runs before pausing for a
/// "Load more" click. Keeps a sparse prefix (few matches per page) from
/// scanning the whole keyspace in one go.
const SCAN_PREFIX_MAX_PAGES: usize = 5;

/// A prefix-scan batch stops early once it has matched at least this percent
/// of `key_scan_count` keys — enough to fill the view — instead of running the
/// full page budget.
const SCAN_PREFIX_FILL_PERCENT: usize = 80;

impl ZedisServerState {
    /// Fills the type of keys that are currently loaded but have an unknown type.
    ///
    /// This is typically used when expanding a directory in the key tree view.
    /// It filters keys based on the prefix and ensures we only query keys at the current level.
    fn fill_key_types(&mut self, prefix: Option<SharedString>, cx: &mut Context<Self>) {
        // Filter keys that need type resolution
        let binding = prefix.unwrap_or_default();
        let prefix = binding.as_str();
        let count = self.keys.len();
        let binding = cx.global::<ZedisGlobalStore>().value(cx);
        let separator = binding.key_separator();
        let mut keys = self
            .keys
            .iter()
            .filter_map(|(key, value)| {
                if *value != KeyType::Unknown {
                    return None;
                }
                if prefix.is_empty() {
                    // if no prefix, only fill keys that are not in a subdirectory
                    // or if the count is less than 1000
                    if count < 1000 || !key.contains(separator) {
                        return Some(key.clone());
                    }
                    return None;
                };
                let suffix = key.strip_prefix(prefix)?;
                // Skip if the key is in a deeper subdirectory (contains delimiter)
                if suffix.contains(separator) {
                    return None;
                }
                Some(key.clone())
            })
            .take(2000)
            .collect::<Vec<SharedString>>();
        debug!(prefix, size = keys.len(), "fill key types");
        if keys.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        keys.sort_unstable();
        // Spawn a background task to fetch types
        self.spawn(
            ServerTask::FillKeyTypes,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut types = Vec::with_capacity(keys.len());
                if client.is_cluster() {
                    // Cluster mode: keys may be on different nodes, use concurrent requests
                    let conn = client.connection().clone();
                    let results: Vec<(SharedString, String)> = stream::iter(keys.iter().cloned())
                        .map(|key| {
                            let mut conn_clone = conn.clone();
                            async move {
                                let t: String = cmd("TYPE")
                                    .arg(key.as_str())
                                    .query_async(&mut conn_clone)
                                    .await
                                    .unwrap_or_default();
                                (key, t)
                            }
                        })
                        .buffer_unordered(100)
                        .collect()
                        .await;
                    types = results;
                } else {
                    // Non-cluster: use pipeline to batch TYPE commands, reducing RTT
                    let mut conn = client.connection().clone();
                    for chunk in keys.chunks(500) {
                        let mut pipeline = pipe();
                        for key in chunk {
                            pipeline.cmd("TYPE").arg(key.as_str());
                        }
                        let results: Vec<String> = pipeline.query_async(&mut conn).await?;
                        for (key, t) in chunk.iter().zip(results) {
                            types.push((key.clone(), t));
                        }
                    }
                }
                Ok(types)
            },
            move |this, result, cx| {
                if let Ok(types) = result {
                    // Update local state with fetched types
                    for (key, value) in types {
                        if let Some(k) = this.keys.get_mut(&key) {
                            *k = KeyType::from(value.as_str());
                        }
                    }
                    // Trigger UI update by changing the tree ID
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.notify();
            },
            cx,
        );
    }
    /// Internal function to scan keys from Redis.
    ///
    /// It handles pagination via cursors and recursive calls to fetch more data
    /// if the result set is too small.
    pub(crate) fn scan_keys(&mut self, server_id: SharedString, keyword: SharedString, cx: &mut Context<Self>) {
        // Guard clause: ignore if the context has changed (e.g., switched server)
        if self.server_id != server_id || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        let processing_server = server_id.clone();
        let processing_keyword = keyword.clone();
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let key_scan_count = store.key_scan_count().max(1);
        let with_ttl = store.show_key_tree_ttl();
        // First-page load follows the user's "Per Scan" setting: one batch of
        // `key_scan_count` keys per cluster master (each master returns ~that
        // many in a single SCAN round), capped so a large cluster can't pull an
        // unbounded batch at once. `scan_times` grows the target by one batch
        // on each "load more". Standalone => masters = 1.
        let masters = self.nodes.0.max(1);
        let per_page = key_scan_count.saturating_mul(masters).min(SCAN_RESULT_MAX_CAP);
        let max = (self.scan_times + 1) * per_page;
        let db = self.db;
        // Describe this scan round in the task log: per-round COUNT and how many
        // keys are already loaded (the effective offset into the overall scan),
        // plus the match pattern when a keyword filter is active.
        let offset = self.keys.len();
        let scan_arg = if keyword.is_empty() {
            format!("count={key_scan_count} offset={offset}")
        } else {
            format!("match=*{keyword}* count={key_scan_count} offset={offset}")
        };
        self.spawn_with_arg(
            ServerTask::ScanKeys,
            scan_arg,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let pattern = if keyword.is_empty() {
                    "*".to_string()
                } else {
                    format!("*{}*", keyword)
                };
                // COUNT hint = the user's "Per Scan" setting for both browse
                // and keyword search; the accumulation target (`max`) stops the
                // auto-paging loop after roughly one batch per master.
                let count = key_scan_count as u64;
                if let Some(cursors) = cursors {
                    client.scan(Some(cursors), &pattern, count, with_ttl).await
                } else {
                    client.first_scan(&pattern, count, with_ttl).await
                }
            },
            move |this, result, cx| {
                // Abandon a page whose keyword filter no longer matches the
                // active scan — a stale page would inject the previous
                // query's keys into the current tree. (Server/db switches are
                // already filtered out by spawn_with_arg's stale guard.)
                if this.keyword != processing_keyword {
                    return;
                }
                let mut should_select_processing_key = false;
                match result {
                    Ok((cursors, keys)) => {
                        should_select_processing_key = keys.iter().any(|(k, _, _)| k == &processing_keyword);
                        debug!("cursors: {cursors:?}, keys count: {}", keys.len());
                        // Check if scan is complete (all cursors returned to 0)
                        if cursors.iter().sum::<u64>() == 0 {
                            this.scan_completed = true;
                            cx.emit(ServerEvent::KeyScanFinished);
                            this.cursors = None;
                        } else {
                            this.cursors = Some(cursors);
                        }
                        this.extend_keys(keys);
                    }
                    Err(_) => {
                        this.cursors = None;
                    }
                };
                if this.cursors.is_some() {
                    cx.emit(ServerEvent::KeyScanPaged);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                if should_select_processing_key {
                    this.select_key(processing_keyword.clone(), cx);
                }
                // Automatically load more if we haven't reached the limit and scan isn't done
                if this.cursors.is_some() && this.keys.len() < max {
                    // run again
                    this.scan_keys(processing_server, processing_keyword, cx);
                    return cx.notify();
                }
                this.scanning = false;
                cx.notify();
                if this.keys.len() == 1
                    && let Some(key) = this.keys.keys().next()
                    && *key != this.key.clone().unwrap_or_default()
                {
                    this.select_key(key.clone(), cx);
                }
            },
            cx,
        );
    }
    pub fn handle_auto_refresh(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        if self.query_mode == QueryMode::Exact {
            self.select_key(keyword, cx);
            return;
        }
        let pattern = match self.query_mode {
            QueryMode::Exact => {
                self.select_key(keyword, cx);
                return;
            }
            QueryMode::Prefix => format!("{keyword}*"),
            _ => format!("*{keyword}*"),
        };
        let server_id = self.server_id.clone();
        let db = self.db;
        // Refresh roughly the keys currently shown, spread across cluster
        // masters (first_scan sends COUNT=count to *each* master), so
        // auto-refresh keeps the loaded view fresh instead of pulling a fixed
        // 10k-per-master batch — which ignored "Per Scan" and ballooned the
        // tree to 10000×masters on a multi-master cluster. Floored at one
        // "Per Scan" batch so a tiny view still re-scans sensibly.
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let key_scan_count = store.key_scan_count().max(1);
        let with_ttl = store.show_key_tree_ttl();
        let masters = self.nodes.0.max(1);
        let count = (self.keys.len().max(key_scan_count) / masters).max(1);
        self.spawn_with_arg(
            ServerTask::AutoRefresh,
            pattern.clone(),
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;

                client.first_scan(&pattern, count as u64, with_ttl).await
            },
            move |this, result, cx| {
                // This refresh diffs against the live key set and *removes*
                // keys missing from its result. If the active filter changed
                // while it was in flight, its result is for the old pattern —
                // abandon it so it can't delete keys that match the new one.
                // (`keyword` tracks `self.keyword`, set together in
                // handle_filter; server/db switches are handled upstream.)
                if this.keyword != keyword {
                    return;
                }
                if let Ok((_, keys)) = result {
                    let new_keys_set: AHashSet<SharedString> = keys.iter().map(|(k, _, _)| k.clone()).collect();

                    let keys_to_remove: Vec<SharedString> = this
                        .keys
                        .keys()
                        .filter(|k| !new_keys_set.contains(*k))
                        .cloned()
                        .collect();

                    let keys_to_add: Vec<(SharedString, SharedString, i64)> = keys
                        .into_iter()
                        .filter(|(k, _, _)| !this.keys.contains_key(k))
                        .collect();

                    let has_changes = !keys_to_remove.is_empty() || !keys_to_add.is_empty();
                    debug!(
                        keys_to_remove = keys_to_remove.len(),
                        keys_to_add = keys_to_add.len(),
                        has_changes,
                        "auto refresh",
                    );

                    if has_changes {
                        // Remove old keys
                        for key in keys_to_remove {
                            this.keys.remove(&key);
                        }

                        // Add new keys
                        if keys_to_add.is_empty() {
                            this.key_tree_id = Uuid::now_v7().to_string().into();
                        } else {
                            this.extend_keys(keys_to_add);
                        }
                        cx.notify();
                    }
                }
            },
            cx,
        );
    }
    pub fn handle_filter(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan(cx);
        match self.query_mode {
            QueryMode::Prefix => self.scan_prefix(keyword, cx),
            QueryMode::Exact => self.select_key(keyword, cx),
            _ => self.scan(keyword, cx),
        }
    }
    /// Collapse all keys
    pub fn collapse_all_keys(&mut self, cx: &mut Context<Self>) {
        cx.emit(ServerEvent::KeyCollapseAll);
        cx.emit(ServerEvent::KeyTreeUpdated);
    }
    /// Initiates a new scan for keys matching the keyword.
    pub fn scan(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan(cx);
        self.scanning = true;
        self.keyword = keyword.clone();
        cx.emit(ServerEvent::KeyScanStarted);
        cx.notify();
        self.scan_keys(self.server_id.clone(), keyword, cx);
    }
    /// Loads the next batch of keys (pagination).
    pub fn scan_next(&mut self, cx: &mut Context<Self>) {
        if self.scan_completed {
            return;
        }
        self.scan_times += 1;
        self.scan_keys(self.server_id.clone(), self.keyword.clone(), cx);
        cx.notify();
    }
    /// Scans keys matching a specific prefix.
    ///
    /// Optimized for populating directory-like structures in the key view.
    pub fn scan_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Avoid reloading if already loaded
        let mut key_type_full_loaded = false;
        let mut key_full_loaded = false;
        for key in self.loaded_prefixes.iter() {
            if prefix.as_str() == key.as_str() {
                key_type_full_loaded = true;
                break;
            }
            if prefix.as_str().starts_with(key.as_str()) {
                key_full_loaded = true;
            }
        }
        if key_type_full_loaded {
            return;
        }
        if key_full_loaded {
            self.loaded_prefixes.insert(prefix.clone());
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        // If global scan is complete, we might just need to resolve types
        if self.scan_completed {
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        cx.emit(ServerEvent::KeyScanStarted);
        // Mark this prefix as in-flight so the matching folder row shows an
        // inline spinner until the (up to 5-round) scan finishes.
        self.scanning_prefixes.insert(prefix.clone());
        self.scan_prefix_page(self.server_id.clone(), prefix, None, 0, 0, cx);
    }

    /// Performs a single SCAN page for `scan_prefix`, inserting the matched keys
    /// and emitting `KeyTreeUpdated` before recursing for the next page.
    ///
    /// Running one page per background task (instead of looping up to 5 times
    /// inside a single task and returning everything at once) lets keys stream
    /// into the tree incrementally as each SCAN round returns, rather than
    /// appearing all together when the whole loop finishes.
    ///
    /// `loaded` is the number of matched keys accumulated so far in this batch;
    /// the batch stops once it reaches ~`SCAN_PREFIX_FILL_PERCENT`% of
    /// `key_scan_count` or hits `SCAN_PREFIX_MAX_PAGES` pages.
    fn scan_prefix_page(
        &mut self,
        server_id: SharedString,
        prefix: SharedString,
        cursors: Option<Vec<u64>>,
        iteration: usize,
        loaded: usize,
        cx: &mut Context<Self>,
    ) {
        // Bail if the active server changed while paging.
        if self.server_id != server_id {
            return;
        }
        let db = self.db;
        let pattern = format!("{}*", prefix);
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let key_scan_count = store.key_scan_count() as u64;
        let with_ttl = store.show_key_tree_ttl();
        // Stop this batch once accumulated matches reach ~80% of key_scan_count.
        let threshold = key_scan_count as usize * SCAN_PREFIX_FILL_PERCENT / 100;
        let task_server_id = server_id.clone();
        self.spawn_with_arg(
            ServerTask::ScanPrefix,
            prefix.clone(),
            move || async move {
                let client = get_connection_manager().get_client(&task_server_id, db).await?;
                let (new_cursor, keys) = if let Some(cursors) = cursors {
                    client.scan(Some(cursors), &pattern, key_scan_count, with_ttl).await?
                } else {
                    client.first_scan(&pattern, key_scan_count, with_ttl).await?
                };
                let done = new_cursor.iter().sum::<u64>() == 0;
                Ok((keys, new_cursor, done))
            },
            move |this, result, cx| {
                let mut finished = true;
                if let Ok((keys, new_cursor, done)) = result {
                    let batch_loaded = loaded + keys.len();
                    debug!(
                        prefix = prefix.as_str(),
                        count = keys.len(),
                        batch_loaded,
                        threshold,
                        done,
                        iteration,
                        "scan prefix page"
                    );
                    this.extend_keys(keys);
                    cx.emit(ServerEvent::KeyTreeUpdated);
                    if done {
                        this.loaded_prefixes.insert(prefix.clone());
                        this.incomplete_prefixes.remove(&prefix);
                    } else if batch_loaded < threshold && iteration + 1 < SCAN_PREFIX_MAX_PAGES {
                        // Haven't matched ~80% of key_scan_count yet and still
                        // under the page budget — keep scanning so results keep
                        // streaming in. Clone `prefix` so the original survives
                        // for the spinner cleanup in the `finished` branch below.
                        this.scan_prefix_page(
                            server_id,
                            prefix.clone(),
                            Some(new_cursor),
                            iteration + 1,
                            batch_loaded,
                            cx,
                        );
                        finished = false;
                    } else {
                        // Filled the view (~80% of key_scan_count) or hit the
                        // page budget without finishing — remember the cursor so
                        // the inline "Load more" row can resume the scan here.
                        this.incomplete_prefixes.insert(prefix.clone(), new_cursor);
                    }
                }
                if finished {
                    // Prefix scan is over (completed, hit the page cap, or
                    // errored) — drop the in-flight marker so the folder
                    // spinner clears on the rebuild emitted just below.
                    this.scanning_prefixes.remove(&prefix);
                    cx.emit(ServerEvent::KeyScanFinished);
                    cx.emit(ServerEvent::KeyTreeUpdated);
                    if this.keys.len() == 1
                        && let Some(key) = this.keys.keys().next()
                    {
                        this.select_key(key.clone(), cx);
                    }
                }
            },
            cx,
        );
    }

    /// Resumes a folder scan that previously stopped at the page cap,
    /// continuing from the saved cursor for another batch of pages. Drives
    /// the inline "Load more" row in the key tree. No-op if the prefix has
    /// no saved resume state (already finished or never paused).
    pub fn load_more_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        let Some(cursors) = self.incomplete_prefixes.remove(&prefix) else {
            return;
        };
        cx.emit(ServerEvent::KeyScanStarted);
        // Re-mark in-flight (spinner) and resume from the saved cursor with a
        // fresh page budget.
        self.scanning_prefixes.insert(prefix.clone());
        self.scan_prefix_page(self.server_id.clone(), prefix, Some(cursors), 0, 0, cx);
    }

    /// Force-refreshes keys under a prefix, bypassing all load caches.
    ///
    /// Unlike `scan_prefix`, this always re-scans from Redis regardless of whether the
    /// prefix was previously loaded or the global scan is complete.  Existing keys under
    /// the prefix are removed first so the result is a clean, up-to-date snapshot.
    pub fn refresh_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Drop any cached state for this prefix (and sub-prefixes)
        self.loaded_prefixes
            .retain(|p| !p.as_str().starts_with(prefix.as_str()));
        // Drop any saved "load more" resume state for this prefix subtree.
        self.incomplete_prefixes
            .retain(|p, _| !p.as_str().starts_with(prefix.as_str()));
        // Remove stale keys so the tree shows only what Redis returns
        self.keys.retain(|key, _| !key.starts_with(prefix.as_str()));
        // Clear scan_completed so scan_prefix performs a full Redis re-scan rather than
        // short-circuiting to fill_key_types.  scan_prefix will restore it via the
        // KeyScanFinished path if the prefix scan itself completes fully.
        self.scan_completed = false;
        self.scan_prefix(prefix, cx);
    }

    fn get_value(&mut self, key: SharedString, task: ServerTask, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let current_key = key.clone();
        let max_truncate_length = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();

        self.spawn(
            task,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut conn = client.connection().clone();
                let (t, ttl): (String, i64) = pipe()
                    .cmd("TYPE")
                    .arg(key.as_str())
                    .cmd("TTL")
                    .arg(key.as_str())
                    .query_async(&mut conn)
                    .await?;
                if ttl == -2 {
                    return Ok(RedisValue {
                        expire_at: Some(-2),
                        ..Default::default()
                    });
                }
                let expire_at = match ttl {
                    -1 => Some(-1),
                    t if t >= 0 => Some(unix_ts() + t),
                    _ => None,
                };

                let key_type = KeyType::from(t.as_str());
                let mut redis_value = match key_type {
                    KeyType::String => {
                        let mut data = get_redis_bytes_value(&mut conn, &key).await?;
                        data.detect_and_update(server_id.as_str(), key.as_str(), max_truncate_length);
                        Ok(RedisValue {
                            key_type: KeyType::String,
                            data: Some(RedisValueData::Bytes(Arc::new(data))),
                            ..Default::default()
                        })
                    }
                    KeyType::List => first_load_list_value(&mut conn, &key).await,
                    KeyType::Set => first_load_set_value(&mut conn, &key).await,
                    KeyType::Zset => first_load_zset_value(&mut conn, &key, SortOrder::Asc).await,
                    KeyType::Hash => first_load_hash_value(&mut conn, &key, client.is_at_least_version("7.4.0")).await,
                    KeyType::Stream => first_load_stream_value(&mut conn, &key, true).await,
                    KeyType::Json => get_redis_json_value(&mut conn, &key).await,
                    // The chart + metadata are loaded lazily by
                    // ZedisTimeSeriesEditor (it drives its own TS.INFO /
                    // TS.RANGE with range controls), so here we only need
                    // to classify the key so the editor dispatch routes
                    // to that viewer.
                    KeyType::TimeSeries => Ok(RedisValue {
                        key_type: KeyType::TimeSeries,
                        ..Default::default()
                    }),
                    // RedisBloom structures (Bloom / Cuckoo / CMS / Top-K
                    // / t-digest) — classify only; ZedisProbabilisticEditor
                    // fetches its own *.INFO + extras. `key_type` keeps the
                    // ProbKind so the dispatch knows which one.
                    KeyType::Probabilistic(_) => Ok(RedisValue {
                        key_type,
                        ..Default::default()
                    }),
                    // Redis 8 Vector Set — classify only; the viewer drives
                    // its own VINFO / VCARD / VDIM / VRANDMEMBER / VSIM.
                    KeyType::Vectorset => Ok(RedisValue {
                        key_type: KeyType::Vectorset,
                        ..Default::default()
                    }),
                    _ => Err(Error::Invalid {
                        message: "unsupported key type".to_string(),
                    }),
                }?;
                if let Ok(memory_usage) = client.memory_usage(key.as_str(), key_type.as_str()).await {
                    redis_value.size = memory_usage;
                }
                redis_value.expire_at = expire_at;
                Ok(redis_value)
            },
            move |this, result, cx| {
                if this.key.as_ref() != Some(&current_key) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if this.value.as_ref() == Some(&value) {
                            return;
                        }
                        if !value.is_expired() {
                            let need_refresh = if let Some(k) = this.keys.get_mut(&current_key) {
                                if *k != value.key_type {
                                    *k = value.key_type();
                                    true
                                } else {
                                    false
                                }
                            } else {
                                this.keys.insert(current_key, value.key_type());
                                true
                            };
                            if need_refresh {
                                this.key_tree_id = Uuid::now_v7().to_string().into();
                                cx.emit(ServerEvent::KeyTreeUpdated);
                            }
                        }
                        this.value = Some(value);
                    }
                    Err(e) => {
                        // Keep the key selected and surface the failure in the
                        // value panel (Failed status) instead of silently
                        // deselecting — a transient toast was otherwise the
                        // only signal, and a blank panel looks identical to
                        // "no key selected". The editor renders this inline
                        // with a retry button.
                        let message: SharedString = e.to_string().into();
                        match this.value.as_mut() {
                            Some(value) => {
                                value.status = RedisValueStatus::Failed(message);
                                value.data = None;
                            }
                            None => {
                                this.value = Some(RedisValue {
                                    status: RedisValueStatus::Failed(message),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                };
                cx.emit(ServerEvent::ValueLoaded);
                cx.notify();
            },
            cx,
        );
    }

    /// Reloads the value for a selected key.
    pub fn reload_value(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.get_value(key, ServerTask::ReloadValue, cx);
    }
    pub fn is_channel_mode(&self) -> bool {
        self.value.as_ref().is_some_and(|v| v.key_type == KeyType::Channel)
    }
    /// Sets the channel mode for current server.
    pub fn change_channel_mode(&mut self, cx: &mut Context<Self>) {
        self.value = Some(RedisValue {
            key_type: KeyType::Channel,
            ..Default::default()
        });
        self.key = None;
        cx.notify();
    }

    /// Publishes a message to a Redis channel.
    pub fn publish_message(&mut self, channel: SharedString, message: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        self.spawn_with_arg(
            ServerTask::PublishMessage,
            channel.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: u64 = cmd("PUBLISH")
                    .arg(channel.as_str())
                    .arg(message.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |_this, _result, cx| {
                cx.emit(ServerEvent::PubsubMessagePublished);
                cx.notify();
            },
            cx,
        );
    }

    /// Selects a key and fetches its details (Type, TTL, Value).
    pub fn select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.key = Some(key.clone());
        if key.is_empty() {
            return;
        }
        self.terminal = false;
        // only set loading status if the value exists for better performance
        // prevent editor flickering
        if let Some(value) = self.value.as_mut() {
            value.status = RedisValueStatus::Loading;
        } else {
            self.value = Some(RedisValue {
                status: RedisValueStatus::Loading,
                ..Default::default()
            });
        }
        if !self.keys.contains_key(&key) {
            self.keys.insert(key.clone(), KeyType::Unknown);
        }
        cx.emit(ServerEvent::KeySelected(key.clone()));
        cx.notify();

        self.get_value(key, ServerTask::Selectkey, cx);
    }
    pub fn delete_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let remove_key = key.clone();
        self.spawn_with_arg(
            ServerTask::DeleteKey,
            key.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("DEL").arg(key.as_str()).query_async(&mut conn).await?;
                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.remove(&remove_key);
                    this.clear_value_history_for(&remove_key);
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                    // Deselect if the deleted key was selected
                    if this.key == Some(remove_key) {
                        this.key = None;
                        this.value = None;
                    }
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    pub fn delete_folder(&mut self, folder: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let separator = cx.global::<ZedisGlobalStore>().value(cx).key_separator().to_string();
        let prefix = format!("{folder}{separator}");
        let pattern = format!("{prefix}*");
        self.spawn_with_arg(
            ServerTask::DeleteKeys,
            prefix.clone(),
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let count = 10_000;
                let mut cursors: Option<Vec<u64>> = None;
                for _ in 0..20 {
                    let (new_cursors, keys_per_node) = client.scan_nodes(cursors, &pattern, count).await?;
                    client.unlike_keys(keys_per_node).await?;

                    if new_cursors.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursors);
                }

                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.retain(|key, _| !key.starts_with(prefix.as_str()));
                    this.value_history.retain(|key, _| !key.starts_with(prefix.as_str()));
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    pub fn unlink_keys(&mut self, keys: Vec<SharedString>, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let remove_keys = keys.clone();
        self.spawn_with_arg(
            ServerTask::DeleteKeys,
            format!("{} keys", remove_keys.len()),
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                client.unlike_keys_scattered(keys).await
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.retain(|key, _| !remove_keys.contains(key));
                    this.value_history.retain(|key, _| !remove_keys.contains(key));
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }
    /// Deletes a specified key.
    pub fn delete_select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        cx.notify();
        self.delete_key(key, cx);
    }
    /// Renames a key. With `overwrite == false` it issues `RENAMENX` and,
    /// if the destination already exists, leaves the key untouched and
    /// emits [`ServerEvent::RenameTargetExists`] so the editor can confirm
    /// a clobber. With `overwrite == true` it issues a plain `RENAME`. On
    /// success the keys map, local write history and the selected key are
    /// all re-pointed from `old` to `new` (RENAME carries value + TTL
    /// server-side, so nothing else needs reloading beyond the value view).
    pub fn rename_key(&mut self, old: SharedString, new: SharedString, overwrite: bool, cx: &mut Context<Self>) {
        if new.is_empty() || new == old {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let key_type = self.keys.get(&old).copied();
        let old_done = old.clone();
        let new_done = new.clone();
        self.spawn_with_arg(
            ServerTask::RenameKey,
            new.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                if overwrite {
                    let _: () = cmd("RENAME")
                        .arg(old.as_str())
                        .arg(new.as_str())
                        .query_async(&mut conn)
                        .await?;
                    Ok(true)
                } else {
                    let renamed: i64 = cmd("RENAMENX")
                        .arg(old.as_str())
                        .arg(new.as_str())
                        .query_async(&mut conn)
                        .await?;
                    Ok(renamed == 1)
                }
            },
            move |this, result, cx| {
                match result {
                    Ok(true) => {
                        if let Some(kt) = key_type {
                            this.keys.insert(new_done.clone(), kt);
                        }
                        this.keys.remove(&old_done);
                        // The key tree renders each key's TTL state from this
                        // cache (no entry → the TTL icon goes missing), so move
                        // it across too. RENAME preserves the TTL server-side.
                        if let Some(ttl) = this.key_ttls.remove(&old_done) {
                            this.key_ttls.insert(new_done.clone(), ttl);
                        }
                        if let Some(history) = this.value_history.remove(&old_done) {
                            this.value_history.insert(new_done.clone(), history);
                        }
                        this.key_tree_id = Uuid::now_v7().to_string().into();
                        // Re-select via the normal path so the key tree expands
                        // to reveal the renamed node — `KeySelected` drives the
                        // tree's `update_expand`; a bare rebuild would leave the
                        // key hidden under a collapsed folder. Also reloads the
                        // value into the editor.
                        if this.key.as_ref() == Some(&old_done) {
                            this.select_key(new_done.clone(), cx);
                        }
                        cx.emit(ServerEvent::KeyTreeUpdated);
                    }
                    Ok(false) => {
                        // RENAMENX refused: destination exists. Ask the UI
                        // to confirm an overwrite (clobbering RENAME).
                        cx.emit(ServerEvent::RenameTargetExists(old_done.clone(), new_done.clone()));
                    }
                    Err(_) => {
                        // Failure already surfaced via spawn_with_arg's error path.
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
    /// Updates the TTL (expiration) for a key.
    pub fn update_key_ttl(&mut self, key: SharedString, ttl: SharedString, cx: &mut Context<Self>) {
        if ttl.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;

        let mut new_ttl = Duration::ZERO;
        let mut parse_fail_error = "".to_string();
        match parse_duration(&ttl) {
            Ok(ttl) => new_ttl = ttl,
            Err(err) => {
                parse_fail_error = err.to_string();
            }
        }

        if !new_ttl.is_zero() {
            value.expire_at = Some(unix_ts() + new_ttl.as_secs() as i64);
        }
        cx.notify();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            key.clone(),
            move || async move {
                if !parse_fail_error.is_empty() {
                    return Err(Error::Invalid {
                        message: parse_fail_error,
                    });
                }
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("EXPIRE")
                    .arg(key.as_str())
                    .arg(new_ttl.as_secs())
                    .query_async(&mut conn)
                    .await?;
                Ok(ttl)
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    if result.is_err() {
                        value.expire_at = original_ttl;
                    }
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Batch-apply a TTL to an explicit key list (multi-select). `ttl_secs =
    /// Some` issues `EXPIRE`, `None` issues `PERSIST`; cluster-safe via
    /// `set_ttl_keys_scattered`. Updates the local TTL cache on success.
    pub fn batch_set_ttl_keys(&mut self, keys: Vec<SharedString>, ttl_secs: Option<u64>, cx: &mut Context<Self>) {
        if keys.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let affected = keys.clone();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            format!("{} keys", keys.len()),
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                client.set_ttl_keys_scattered(keys, ttl_secs).await
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let new_ttl = ttl_secs.map(|s| s as i64).unwrap_or(-1);
                    for key in &affected {
                        this.key_ttls.insert(key.clone(), new_ttl);
                    }
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    /// Batch-apply a TTL to every key under a folder prefix. Scans `prefix*`
    /// across masters (like `delete_folder`) and applies in pages.
    pub fn batch_set_ttl_folder(&mut self, folder: SharedString, ttl_secs: Option<u64>, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let separator = cx.global::<ZedisGlobalStore>().value(cx).key_separator().to_string();
        let prefix = format!("{folder}{separator}");
        let pattern = format!("{prefix}*");
        let prefix_done = prefix.clone();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            prefix,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let count = 10_000;
                let mut cursors: Option<Vec<u64>> = None;
                for _ in 0..20 {
                    let (new_cursors, keys_per_node) = client.scan_nodes(cursors, &pattern, count).await?;
                    let flat: Vec<SharedString> = keys_per_node.into_iter().flatten().collect();
                    client.set_ttl_keys_scattered(flat, ttl_secs).await?;
                    if new_cursors.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursors);
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let new_ttl = ttl_secs.map(|s| s as i64).unwrap_or(-1);
                    for (k, v) in this.key_ttls.iter_mut() {
                        if k.starts_with(prefix_done.as_str()) {
                            *v = new_ttl;
                        }
                    }
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    pub fn add_key(
        &mut self,
        category: SharedString,
        key: SharedString,
        ttl: SharedString,
        args: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let key: SharedString = key.trim().to_string().into();
        if key.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let key_type = KeyType::from(category.to_lowercase().as_str());
        let key_clone = key.clone();
        // Remaining TTL in seconds for the optimistic local cache (-1 = none),
        // so the new row carries the right TTL chip immediately.
        let ttl_secs: i64 = if ttl.trim().is_empty() {
            -1
        } else if let Ok(secs) = ttl.trim().parse::<u64>() {
            secs as i64
        } else {
            humantime::parse_duration(ttl.trim())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(-1)
        };
        self.spawn_with_arg(
            ServerTask::AddKey,
            key.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let exists: bool = cmd("EXISTS").arg(key.as_str()).query_async(&mut conn).await?;
                let ttl_duration = if ttl.is_empty() {
                    None
                } else if let Ok(secs) = ttl.parse::<u64>() {
                    Some(Duration::from_secs(secs))
                } else {
                    let ttl = humantime::parse_duration(&ttl).map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Some(ttl)
                };

                if exists {
                    return Err(Error::Invalid {
                        message: "Key already exists".to_string(),
                    });
                }

                let command = key_type.create_command();
                if command.is_empty() {
                    return Err(Error::Invalid {
                        message: "Invalid key type".to_string(),
                    });
                }

                let mut c = cmd(command);
                c.arg(key.as_str());
                for a in &args {
                    c.arg(a.as_str());
                }
                let _: () = c.query_async(&mut conn).await?;

                if let Some(ttl_duration) = ttl_duration {
                    let _: () = cmd("EXPIRE")
                        .arg(key.as_str())
                        .arg(ttl_duration.as_secs())
                        .query_async(&mut conn)
                        .await?;
                }

                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.keys.insert(key_clone.clone(), key_type);
                    this.key_ttls.insert(key_clone.clone(), ttl_secs);
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                    this.select_key(key_clone, cx);
                    // Rebuild the tree from `keys` so the new row actually
                    // appears (without this it stays hidden until a refresh).
                    cx.emit(ServerEvent::KeyTreeUpdated);
                }
                cx.notify();
            },
            cx,
        );
    }
}
