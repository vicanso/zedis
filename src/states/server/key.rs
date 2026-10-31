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
    LoadedKeys, ServerEvent, ServerTask, ZedisServerState,
    hash::first_load_hash_value,
    json::get_redis_json_value,
    list::first_load_list_value,
    set::first_load_set_value,
    stream::first_load_stream_value,
    string::{get_redis_bytes_preview, get_redis_bytes_value},
    value::{
        KeyType, MAX_INLINE_VALUE_SIZE, MAX_MODULE_DUMP_BYTES, ModuleTypeId, RedisBytesValue, RedisValue,
        RedisValueData, RedisValueStatus, SortOrder, VALUE_PREVIEW_BYTES, ViewMode,
    },
    zset::first_load_zset_value,
};
use crate::connection::{
    ExpireCondition, HeatMetric, KeyOp, KeyOpOutcome, ServerCommand, create_key, delete_key, delete_keys,
    delete_keys_matching, dump_key, expire_key, expire_key_at, floors, flush_all, flush_db, get_server_features,
    get_server_heat_probe, key_memory_usage, key_object_meta, key_type_and_ttl, key_types, persist_key, publish,
    rename_key, run_key_op, scan_page, server_supports, set_keys_ttl, set_ttl_matching, snapshot_key,
};
use crate::db::{
    TRASH_MAX_BULK_KEYS, TRASH_MAX_PAYLOAD, TRASH_MAX_VALUE_MEMORY, TrashEntry, get_favorites_manager,
    get_key_metadata_manager, get_recent_keys_manager, recent_keys_scope,
};
use crate::states::{QueryMode, ZedisGlobalStore, i18n_key_tree, i18n_status_bar, trash_store};
use crate::{
    connection::{Capability, ServerDb},
    error::Error,
    helpers::{escape_glob, folder_prefixes, format_ttl_chip, parse_duration, unix_ts, unix_ts_millis},
};
use ahash::{AHashMap, AHashSet};
use bytes::Bytes;
use futures::future::join;
use gpui::{SharedString, prelude::*};
use regex::Regex;
use rust_i18n::t;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};
use zedis_core::change_log::ChangeEntry;

/// What a value's `expire_at` reads after `EXPIRE` (`Some` seconds from
/// `now`) or `PERSIST` (`None`): the instant, or the load path's `-1` for no
/// expiry.
fn expire_at_after(ttl_secs: Option<u64>, now: i64) -> i64 {
    match ttl_secs {
        Some(secs) => now.saturating_add(i64::try_from(secs).unwrap_or(i64::MAX)),
        None => -1,
    }
}

/// The records this app keeps under a key's *name* follow the key to its
/// new one: the recent-keys entry, the favorite, the tag and note. Only the
/// edit history did, so a renamed key was still listed under a name that
/// opened nothing, and its star and colour stayed behind with it (#188).
/// What is moved is what the old name had — a key with no favorite or tag
/// takes none, and leaves the new name's alone. Never an error: these are
/// conveniences, and the rename itself has already happened.
fn move_local_records(server_id: &str, db: usize, old: &str, new: &str) {
    let scope = recent_keys_scope(server_id, db);
    if let Err(e) = get_recent_keys_manager().rename_record(&scope, old, new) {
        warn!(error = %e, "recent keys did not follow the rename");
    }
    if let Err(e) = get_favorites_manager().rename_record(server_id, old, new) {
        warn!(error = %e, "the favorite did not follow the rename");
    }
    if let Err(e) = get_key_metadata_manager().rename(server_id, old, new) {
        warn!(error = %e, "the tag and note did not follow the rename");
    }
}

/// A key the recycle bin put back: where, and with what expiry.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredKey {
    pub key: SharedString,
    pub db: usize,
    /// The `PTTL` the key was deleted with, which is what `RESTORE` gave it
    /// back: milliseconds, or not positive for a key that does not expire.
    pub pttl_ms: i64,
}

impl RestoredKey {
    /// The expiry as the tree's TTL chip counts it: whole seconds, rounded
    /// up so a key with half a second left is not drawn as expired, and -1
    /// for none.
    fn ttl_secs(&self) -> i64 {
        if self.pttl_ms > 0 {
            (self.pttl_ms + 999) / 1000
        } else {
            -1
        }
    }
}

/// What one auto-refresh round changes: keys to add, keys to take out, and
/// new TTLs for keys already loaded.
struct AutoRefreshPlan {
    add: Vec<(SharedString, SharedString, i64)>,
    remove: Vec<SharedString>,
    ttls: Vec<(SharedString, i64)>,
}

/// Per-page cursor/position state threaded through the recursive
/// [`ZedisServerState::scan_prefix_page`] loop (grouped so the function
/// stays under clippy's argument-count limit).
struct ScanPrefixPage {
    /// SCAN cursors to resume from; `None` on the first page.
    cursors: Option<Vec<u64>>,
    /// Zero-based page index within this batch (caps at `SCAN_PREFIX_MAX_PAGES`).
    iteration: usize,
    /// Matched keys accumulated so far in this batch.
    loaded: usize,
    /// Scan-session epoch captured at launch. A mismatch on completion means
    /// the tree was reset (re-search / filter change) and the page is stale.
    epoch: u64,
}

/// Best-effort stash of a key into the recycle bin before deletion: its
/// `DUMP` and `PTTL`, kept for 24h by `trash_store` — the local database,
/// or the bridge. Every failure path only warns and falls through to a
/// permanent delete — the bin must never turn a working delete into an
/// error.
/// Oversized payloads (> [`TRASH_MAX_PAYLOAD`]) are skipped: the bin is a
/// safety net for fat-finger deletes, not a big-key backup store.
async fn stash_key_to_trash(at: &ServerDb, key: &str) {
    let Some(snapshot) = snapshot_key(at, key, TRASH_MAX_VALUE_MEMORY, TRASH_MAX_PAYLOAD).await else {
        return;
    };
    let entry = TrashEntry {
        key: key.to_string(),
        db: at.db(),
        pttl_ms: snapshot.pttl_ms,
        deleted_at_ms: unix_ts_millis(),
        payload: snapshot.payload,
    };
    if let Err(e) = trash_store::stash(at.server_id(), entry).await {
        warn!(key, error = %e, "trash: stash failed, deleting permanently");
    }
}

/// Whether deleting `count` keys at once puts them in the recycle bin: the
/// bin is on (`soft_delete`, never in the browser) and the selection is not
/// a sweep ([`TRASH_MAX_BULK_KEYS`]). The confirmation and the delete both
/// ask this, so what the dialog says is what the delete does. A delete of a
/// multi-selection used to skip the bin whatever its size, under a one-key
/// delete that promised a restore (#195).
pub fn recycles_deleted_keys(soft_delete: bool, count: usize) -> bool {
    soft_delete && count <= TRASH_MAX_BULK_KEYS
}

/// The keys a delete removed: named one by one (a key, a multi-selection),
/// or everything under a folder's prefix.
enum DeletedKeys {
    Named(AHashSet<SharedString>),
    Prefix(String),
}

impl DeletedKeys {
    fn contains(&self, key: &str) -> bool {
        match self {
            Self::Named(keys) => keys.contains(key),
            Self::Prefix(prefix) => key.starts_with(prefix.as_str()),
        }
    }
}

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
        let count = self.scan.keys.len();
        let separator = self.key_separator();
        let mut keys = self
            .scan
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
        let at = self.at();
        keys.sort_unstable();
        // Spawn a background task to fetch types
        self.spawn(
            ServerTask::FillKeyTypes,
            move || async move {
                let names: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
                let types = key_types(&at, names).await?;
                Ok(keys.into_iter().zip(types).collect::<Vec<(SharedString, String)>>())
            },
            move |this, result, cx| {
                if let Ok(types) = result {
                    // Update local state with fetched types
                    for (key, value) in types {
                        if let Some(k) = this.scan.keys.get_mut(&key) {
                            *k = KeyType::from(value.as_str());
                        }
                    }
                    // Trigger UI update by changing the tree ID
                    this.scan.renew_tree_id();
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
        let at = self.at();
        // Guard clause: ignore if the context has changed (e.g., switched server)
        if self.server_id != server_id || self.scan.keyword != keyword {
            return;
        }
        let cursors = self.scan.cursors.clone();
        let processing_server = server_id.clone();
        let processing_keyword = keyword.clone();
        let key_scan_count = self.key_scan_count();
        let with_ttl = self.show_key_tree_ttl();
        // First-page load follows the user's "Per Scan" setting: one batch of
        // `key_scan_count` keys per cluster master (each master returns ~that
        // many in a single SCAN round), capped so a large cluster can't pull an
        // unbounded batch at once. `scan_times` grows the target by one batch
        // on each "load more". Standalone => masters = 1.
        let masters = self.facts.nodes().0.max(1);
        let per_page = key_scan_count.saturating_mul(masters).min(SCAN_RESULT_MAX_CAP);
        let max = (self.scan.scan_times + 1) * per_page;
        // Describe this scan round in the task log: per-round COUNT and how many
        // keys are already loaded (the effective offset into the overall scan),
        // plus the match pattern when a keyword filter is active.
        let offset = self.scan.scanned_len();
        // A regex keyword has no glob to send: the scan asks for every name
        // and `name_filter` narrows each page as it arrives.
        let name_filter = self.name_filter();
        let keyword = if self.regex_keyword() {
            SharedString::default()
        } else {
            keyword
        };
        let scan_arg = if keyword.is_empty() {
            format!("count={key_scan_count} offset={offset}")
        } else {
            format!("match=*{keyword}* count={key_scan_count} offset={offset}")
        };
        let type_arg = self.scan.type_filter.and_then(|t| t.scan_type_name());
        // The scan session this page belongs to: a type filter, a refresh or
        // a new search starts another (`reset_scan` bumps it) without
        // necessarily changing the keyword.
        let epoch = self.scan.scan_epoch;
        self.spawn_with_arg(
            ServerTask::ScanKeys,
            scan_arg,
            move || async move {
                let pattern = Self::keyword_scan_pattern(&keyword, false, false);
                // COUNT hint = the user's "Per Scan" setting for both browse
                // and keyword search; the accumulation target (`max`) stops the
                // auto-paging loop after roughly one batch per master.
                let count = key_scan_count as u64;
                Ok(scan_page(&at, cursors, &pattern, count, with_ttl, type_arg, name_filter.as_ref()).await?)
            },
            move |this, result, cx| {
                // Abandon a page from another scan session — a stale page
                // would inject the previous query's keys into the current
                // tree, write its cursors over the new scan's (so the old
                // chain's end marked the new scan complete) and carry on
                // paging beside it. The keyword alone misses a type filter
                // or a refresh, which keep it; the epoch does not.
                // (Server/db switches are already filtered out by
                // spawn_with_arg's stale guard.)
                if this.scan.keyword != processing_keyword || this.scan.scan_epoch != epoch {
                    return;
                }
                let mut should_select_processing_key = false;
                let mut finished = false;
                match result {
                    Ok((cursors, keys)) => {
                        should_select_processing_key = keys.iter().any(|(k, _, _)| k == &processing_keyword);
                        debug!("cursors: {cursors:?}, keys count: {}", keys.len());
                        // Check if scan is complete (all cursors returned to 0)
                        if cursors.iter().sum::<u64>() == 0 {
                            this.scan.scan_completed = true;
                            finished = true;
                            this.scan.cursors = None;
                        } else {
                            this.scan.cursors = Some(cursors);
                        }
                        this.extend_keys(
                            keys.into_iter()
                                .map(|(k, t, ttl)| (SharedString::from(k), SharedString::from(t), ttl))
                                .collect(),
                        );
                    }
                    Err(_) => {
                        this.scan.cursors = None;
                        // A refresh keeps the rows until its first batch
                        // lands; with no batch coming they stay. Left set,
                        // the flag silenced every auto-refresh and let the
                        // next folder expand replace the tree with that
                        // folder alone.
                        this.scan.keys_superseded_by_next_batch = false;
                        this.scan.refresh_prefixes.clear();
                    }
                };
                // Keep paging until this round's target is loaded or the scan
                // is done. Decided — and `scanning` put back — before the
                // events go out, so whoever redraws on them (the status bar's
                // Load more, the tree's loading pill) sees the round's end in
                // the same frame; and `KeyScanFinished` goes out after the
                // last page is merged, so its handlers read the final key set.
                let more = this.scan.cursors.is_some() && this.scan.scanned_len() < max;
                if !more {
                    this.scan.scanning = false;
                }
                if finished {
                    cx.emit(ServerEvent::KeyScanFinished);
                }
                if this.scan.cursors.is_some() {
                    cx.emit(ServerEvent::KeyScanPaged);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                if should_select_processing_key {
                    this.select_key(processing_keyword.clone(), cx);
                }
                if more {
                    this.scan_keys(processing_server, processing_keyword, cx);
                    return cx.notify();
                }
                cx.notify();
                if this.scan.keys.len() == 1
                    && let Some(key) = this.scan.keys.keys().next()
                    && *key != this.open.key.clone().unwrap_or_default()
                {
                    this.select_key(key.clone(), cx);
                }
            },
            cx,
        );
    }
    /// The `MATCH` a keyword scans with.
    ///
    /// The keyword is text a key contains (or starts with, in prefix mode):
    /// the tree filters what comes back by plain substring, so the glob is
    /// escaped to mean the same. Unescaped, `cache[v2]` asked the server for
    /// a character class, and `user:*:profile` for a wildcard whose matches
    /// the substring filter then threw away — an empty tree either way. A
    /// regex keyword has no glob: it scans everything and
    /// [`Self::compile_name_filter`] narrows what comes back.
    fn keyword_scan_pattern(keyword: &str, prefix: bool, regex: bool) -> String {
        if keyword.is_empty() || regex {
            return "*".to_string();
        }
        let escaped = escape_glob(keyword);
        if prefix {
            format!("{escaped}*")
        } else {
            format!("*{escaped}*")
        }
    }

    /// The mode a keyword is searched in.
    ///
    /// A regex has one way to the server — scan every name and narrow them
    /// here — whichever of the three modes the box was left in. Sent as a
    /// prefix it is a glob (`MATCH ^user:\d+*`) that matches nothing, and
    /// looked up exactly it is a key name nobody has: the tree said "No keys
    /// found" for keys that were there, under a box that showed the regex
    /// icon and no trace of the mode still deciding (#167).
    fn resolve_query_mode(mode: QueryMode, regex: bool) -> QueryMode {
        if regex { QueryMode::All } else { mode }
    }

    /// [`Self::resolve_query_mode`] for the box as it is now — what the
    /// tree's own wording has to follow, too.
    pub fn keyword_query_mode(&self) -> QueryMode {
        Self::resolve_query_mode(self.scan.query_mode, self.regex_keyword())
    }

    /// The regex a scan narrows the listed names by: the keyword, when the
    /// box is read as one.
    ///
    /// Narrowed as the pages arrive rather than once they are loaded, so
    /// that "enough keys loaded" counts matches. Counting every name the
    /// server listed stopped the scan after one page of the keyspace, and a
    /// match anywhere past it was "No keys found" under a menu item that
    /// says the regex scans everything. A pattern that does not compile
    /// narrows nothing — the tree reports it and keeps showing the keys.
    fn compile_name_filter(keyword: &str, regex: bool) -> Option<Regex> {
        if !regex || keyword.is_empty() {
            return None;
        }
        Regex::new(keyword).ok()
    }

    fn name_filter(&self) -> Option<Regex> {
        Self::compile_name_filter(&self.scan.keyword, self.regex_keyword())
    }

    /// What an auto-refresh round changes in the loaded key set.
    ///
    /// Additions are always safe: a key the server just listed exists.
    /// Removals are only safe when `complete` — the round's cursors all came
    /// back at 0, so the server has listed *everything* matching and a key
    /// absent from the result really is gone. One `SCAN` round is a hint,
    /// not an enumeration: `COUNT` is advisory and `MATCH` filters *after*
    /// the round is drawn, so on any keyspace bigger than a round the result
    /// is a sample. Diffing against a sample deleted live keys from the tree,
    /// and it did so precisely on the large databases where auto-refresh is
    /// most used.
    ///
    /// `ttls` is the loaded TTL map when the tree shows TTLs (`None` when
    /// the round asked for none): a key already loaded gets its new TTL when
    /// its chip would read differently, since the chip does not count down
    /// by itself and otherwise kept the first scan's number for good.
    fn plan_auto_refresh(
        complete: bool,
        scanned: Vec<(String, String, i64)>,
        loaded: &LoadedKeys,
        ttls: Option<&AHashMap<SharedString, i64>>,
    ) -> AutoRefreshPlan {
        let seen: AHashSet<&str> = scanned.iter().map(|(k, _, _)| k.as_str()).collect();
        let remove = if complete {
            loaded.keys().filter(|k| !seen.contains(k.as_ref())).cloned().collect()
        } else {
            Vec::new()
        };
        let mut add = Vec::new();
        let mut new_ttls = Vec::new();
        for (key, key_type, ttl) in scanned {
            if !loaded.contains_key(key.as_str()) {
                add.push((SharedString::from(key), SharedString::from(key_type), ttl));
            } else if let Some(ttls) = ttls
                && ttls
                    .get(key.as_str())
                    .is_none_or(|old| format_ttl_chip(*old) != format_ttl_chip(ttl))
            {
                new_ttls.push((SharedString::from(key), ttl));
            }
        }
        AutoRefreshPlan {
            add,
            remove,
            ttls: new_ttls,
        }
    }

    /// Everything a delete that went through leaves behind, however its keys
    /// were named: out of the loaded keys, the edit history and the recent
    /// keys, the tree rebuilt, and the open key closed when it was one of
    /// them — left open, it showed a value that was gone, and a field edited
    /// there wrote the key back (#197). The one-key, multi-selection and
    /// folder deletes each did their own share of this, and each missed a
    /// different part.
    fn forget_deleted_keys(&mut self, deleted: DeletedKeys, cx: &mut Context<Self>) {
        match &deleted {
            DeletedKeys::Named(keys) => keys.iter().for_each(|key| self.scan.forget_key(key)),
            DeletedKeys::Prefix(_) => self.scan.forget_keys_where(|key| deleted.contains(key)),
        }
        self.open.histories.retain_keys(|key| !deleted.contains(key));
        if self.open.key.as_ref().is_some_and(|key| deleted.contains(key)) {
            self.open.close();
        }
        // Force refresh of the key tree view
        self.scan.renew_tree_id();
        self.refresh_dbsize_now(cx);
        // So the recent-keys list does not offer a key that is gone.
        let scope = recent_keys_scope(self.server_id.as_str(), self.db);
        cx.background_spawn(async move {
            let manager = get_recent_keys_manager();
            let Ok(records) = manager.records(&scope) else {
                return;
            };
            for key in records.iter().filter(|key| deleted.contains(key)) {
                let _ = manager.remove_record(&scope, key);
            }
        })
        .detach();
    }

    pub fn handle_auto_refresh(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        // Nobody is looking: a background tab, or an app left unattended
        // (`pacing::unattended`). The heartbeat and the sampling panels stop
        // for that already (ADR 5); a key tree refresh is a SCAN plus a TYPE
        // and a TTL per key, on a server that may bill every one of them.
        if self.is_background() {
            return;
        }
        let query_mode = self.keyword_query_mode();
        if query_mode == QueryMode::Exact {
            self.select_key(keyword, cx);
            return;
        }
        // The same MATCH the scan that filled the tree sent. Anything else
        // diffs the tree against another question: a regex keyword spelled
        // as a glob matched nothing, so a round that walked the keyspace
        // emptied the tree; an unescaped prefix put `user1:…` in for
        // `user[1]:` and took the real `user[1]:…` keys out.
        let pattern = Self::keyword_scan_pattern(&keyword, query_mode == QueryMode::Prefix, self.regex_keyword());
        let name_filter = Self::compile_name_filter(&keyword, self.regex_keyword());
        let at = self.at();
        // Refresh roughly the keys currently shown, spread across cluster
        // masters (first_scan sends COUNT=count to *each* master), so
        // auto-refresh keeps the loaded view fresh instead of pulling a fixed
        // 10k-per-master batch — which ignored "Per Scan" and ballooned the
        // tree to 10000×masters on a multi-master cluster. Floored at one
        // "Per Scan" batch so a tiny view still re-scans sensibly, and capped
        // at one per master: a view grown by "Load more" to 500k keys asked
        // each master for `SCAN … COUNT 500000` — one command walking half a
        // million slots on the server's main thread — then a TYPE and a TTL
        // for each key it returned, every interval. Past that size a round
        // is a sample: new keys still appear, and removals wait for a round
        // that covers the keyspace (`plan_auto_refresh`).
        let key_scan_count = self.key_scan_count();
        let with_ttl = self.show_key_tree_ttl();
        let masters = self.facts.nodes().0.max(1);
        let count = (self.scan.keys.len().max(key_scan_count) / masters).clamp(1, key_scan_count.max(1));
        let type_arg = self.scan.type_filter.and_then(|t| t.scan_type_name());
        self.spawn_with_arg(
            ServerTask::AutoRefresh,
            pattern.clone(),
            move || async move {
                Ok(scan_page(
                    &at,
                    None,
                    &pattern,
                    count as u64,
                    with_ttl,
                    type_arg,
                    name_filter.as_ref(),
                )
                .await?)
            },
            move |this, result, cx| {
                // This refresh diffs against the live key set and *removes*
                // keys missing from its result. If the active filter changed
                // while it was in flight, its result is for the old pattern —
                // abandon it so it can't delete keys that match the new one.
                // (`keyword` tracks `self.keyword`, set together in
                // handle_filter; server/db switches are handled upstream.)
                if this.scan.keyword != keyword {
                    return;
                }
                // A manual refresh is mid-flight and the rows on screen are
                // the ones it is about to replace wholesale. This result is a
                // *diff* against them, so feeding it through `extend_keys`
                // would spend the supersede on a handful of keys and empty
                // the tree down to them. The re-scan re-reads everything
                // anyway — drop this round.
                if this.scan.keys_superseded_by_next_batch {
                    return;
                }
                if let Ok((cursors, keys)) = result {
                    // Whether this round walked the whole keyspace: every
                    // master's cursor back at 0, the same test `scan_keys`
                    // uses. Only then is "absent from the result" the same
                    // thing as "gone from the server".
                    let complete = cursors.iter().sum::<u64>() == 0;
                    let AutoRefreshPlan {
                        add: keys_to_add,
                        remove: keys_to_remove,
                        ttls: new_ttls,
                    } = Self::plan_auto_refresh(
                        complete,
                        keys,
                        &this.scan.keys,
                        with_ttl.then_some(&*this.scan.key_ttls),
                    );

                    let has_changes = !keys_to_remove.is_empty() || !keys_to_add.is_empty() || !new_ttls.is_empty();
                    debug!(
                        keys_to_remove = keys_to_remove.len(),
                        keys_to_add = keys_to_add.len(),
                        new_ttls = new_ttls.len(),
                        has_changes,
                        "auto refresh",
                    );

                    if has_changes {
                        // Remove old keys
                        for key in &keys_to_remove {
                            this.scan.forget_key(key);
                        }
                        if !new_ttls.is_empty() {
                            Arc::make_mut(&mut this.scan.key_ttls).extend(new_ttls);
                        }

                        // Add new keys
                        if keys_to_add.is_empty() {
                            this.scan.renew_tree_id();
                        } else {
                            this.extend_keys(keys_to_add);
                        }
                        // The key tree rebuilds on this event and on nothing
                        // else: no view observes this entity, so `notify`
                        // alone only repaints rows that were already built.
                        // Without it an auto-refresh kept the key set current
                        // and the panel kept showing the previous one, until
                        // something unrelated (a scan page, opening a key)
                        // happened to rebuild — which is to say the feature
                        // did nothing most of the time.
                        cx.emit(ServerEvent::KeyTreeUpdated);
                        cx.notify();
                    }
                }
            },
            cx,
        );
    }
    /// Run the key tree's query: the keyword the user typed, under the
    /// current mode — [`Self::keyword_query_mode`], which a regex keyword
    /// overrides.
    ///
    /// `refresh` says this asks the *same* question the tree is already
    /// showing (⌘R, the ⋯ menu's Reload), so the rows stay up while the scan
    /// runs and the first batch replaces them — see
    /// [`ZedisServerState::begin_refresh_scan`]. An exact lookup is never
    /// refreshed that way: `select_key` inserts its one key directly and
    /// never reaches `extend_keys`, so nothing would ever take the old rows
    /// out.
    pub fn handle_filter(&mut self, keyword: SharedString, refresh: bool, cx: &mut Context<Self>) {
        match self.keyword_query_mode() {
            QueryMode::Prefix => {
                self.begin_or_reset_scan(refresh, cx);
                // Record what the tree is now showing. `scan_prefix` cannot do
                // this itself — a folder expand calls it with a folder's
                // prefix, which is not what the user asked for — so only the
                // whole-tree query sets it. Without it `keyword` stayed empty
                // through a prefix search and three things read that emptiness
                // as "no filter": the `KeyScanStarted` subscriber wiped the
                // search box to match, auto-refresh re-scanned `*` and poured
                // unrelated keys into the tree every interval, and the status
                // bar's Load more scanned unfiltered.
                self.scan.keyword = keyword.clone();
                // Same reason for the tree-wide scanning flag: a folder expand
                // drives the per-prefix spinner, which is the right scope for
                // it, but a whole-tree query has no folder row to spin and the
                // toolbar reads this one. Safe to set here and nowhere else in
                // `scan_prefix`: the reset just above empties `loaded_prefixes`
                // and clears `scan_completed`, so none of that method's early
                // returns can fire and `scan_prefix_page` always runs to a
                // `finished` round that puts it back.
                self.scan.scanning = true;
                self.scan_prefix(keyword, cx);
            }
            QueryMode::Exact => {
                self.reset_scan(cx);
                self.select_key(keyword, cx);
            }
            _ => {
                self.begin_or_reset_scan(refresh, cx);
                self.start_scan(keyword, cx);
            }
        }
    }

    fn begin_or_reset_scan(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if refresh {
            self.begin_refresh_scan(cx);
        } else {
            self.reset_scan(cx);
        }
    }

    /// Set the key-type filter and re-run the current filter so the tree shows
    /// only keys of that type (`None` clears it).
    pub fn set_type_filter(&mut self, type_filter: Option<KeyType>, cx: &mut Context<Self>) {
        self.scan.type_filter = type_filter;
        let keyword = self.scan.keyword.clone();
        // A different type filter is a different question: the rows on screen
        // are the wrong ones and go at once.
        self.handle_filter(keyword, false, cx);
    }
    /// Collapse all keys
    pub fn collapse_all_keys(&mut self, cx: &mut Context<Self>) {
        cx.emit(ServerEvent::KeyCollapseAll);
        cx.emit(ServerEvent::KeyTreeUpdated);
    }
    /// Initiates a new scan for keys matching the keyword, dropping whatever
    /// the tree holds first.
    pub fn scan(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan(cx);
        self.start_scan(keyword, cx);
    }

    /// The scan itself, with the reset already done by the caller (which
    /// chose whether the old rows stay up meanwhile).
    fn start_scan(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.scan.scanning = true;
        self.scan.keyword = keyword.clone();
        cx.emit(ServerEvent::KeyScanStarted);
        cx.notify();
        self.scan_keys(self.server_id.clone(), keyword, cx);
    }
    /// Loads the next batch of keys (pagination).
    pub fn scan_next(&mut self, cx: &mut Context<Self>) {
        // A round already in flight would be a second paging chain over the
        // same cursors, and each press grew the target a batch more.
        if self.scan.scan_completed || self.scan.scanning {
            return;
        }
        // Refused here rather than inside the spawn: nothing would come back
        // to put `scanning` down again, and the tree's search waits on it.
        if self.refuse_while_offline(cx) {
            return;
        }
        self.scan.scanning = true;
        self.scan.scan_times += 1;
        self.scan_keys(self.server_id.clone(), self.scan.keyword.clone(), cx);
        cx.notify();
    }
    /// Scans keys matching a specific prefix.
    ///
    /// Optimized for populating directory-like structures in the key view.
    pub fn scan_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Avoid reloading if already loaded
        let mut key_type_full_loaded = false;
        let mut key_full_loaded = false;
        for key in self.scan.loaded_prefixes.iter() {
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
            self.scan.loaded_prefixes.insert(prefix.clone());
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        // If global scan is complete, we might just need to resolve types
        if self.scan.scan_completed {
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        cx.emit(ServerEvent::KeyScanStarted);
        // Mark this prefix as in-flight so the matching folder row shows an
        // inline spinner until the (up to 5-round) scan finishes.
        self.scan.scanning_prefixes.insert(prefix.clone());
        let page = ScanPrefixPage {
            cursors: None,
            iteration: 0,
            loaded: 0,
            epoch: self.scan.scan_epoch,
        };
        self.scan_prefix_page(self.server_id.clone(), prefix, page, cx);
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
        page: ScanPrefixPage,
        cx: &mut Context<Self>,
    ) {
        let ScanPrefixPage {
            cursors,
            iteration,
            loaded,
            epoch,
        } = page;
        // Bail if the active server changed while paging.
        if self.server_id != server_id {
            return;
        }
        // "Starts with this text": a `[` or `*` in a folder name, or typed
        // into the prefix box, is a character of the key, not a wildcard.
        let pattern = format!("{}*", escape_glob(&prefix));
        let key_scan_count = self.key_scan_count() as u64;
        let with_ttl = self.show_key_tree_ttl();
        // Stop this batch once accumulated matches reach ~80% of key_scan_count.
        let threshold = key_scan_count as usize * SCAN_PREFIX_FILL_PERCENT / 100;
        let task_at = self.at();
        let type_arg = self.scan.type_filter.and_then(|t| t.scan_type_name());
        // A folder opened under a regex keyword lists what the tree is
        // showing: the names under it that the regex matches.
        let name_filter = self.name_filter();
        self.spawn_with_arg(
            ServerTask::ScanPrefix,
            prefix.clone(),
            move || async move {
                let (new_cursor, keys) = scan_page(
                    &task_at,
                    cursors,
                    &pattern,
                    key_scan_count,
                    with_ttl,
                    type_arg,
                    name_filter.as_ref(),
                )
                .await?;
                let done = new_cursor.iter().sum::<u64>() == 0;
                Ok((keys, new_cursor, done))
            },
            move |this, result, cx| {
                // Stale page from a previous scan session: the user re-ran the
                // search (or changed the type filter) while this folder scan was
                // still streaming. `reset_scan` already cleared the keys,
                // `scanning_prefixes` and `incomplete_prefixes` and bumped the
                // epoch — applying this result would re-inject the old prefix's
                // keys and resurrect its `incomplete_prefixes`/`loaded_prefixes`
                // entry (making the folder's "Load more" flip-flop). Drop it.
                // A keyword check can't catch an empty→empty re-search; the epoch
                // can. (`spawn_with_arg` already guards server/db switches.)
                if this.scan.scan_epoch != epoch {
                    return;
                }
                let mut finished = true;
                if let Ok((keys, new_cursor, done)) = result {
                    let batch_loaded = loaded + keys.len();
                    let will_continue = !done && batch_loaded < threshold && iteration + 1 < SCAN_PREFIX_MAX_PAGES;
                    debug!(
                        prefix = prefix.as_str(),
                        count = keys.len(),
                        batch_loaded,
                        threshold,
                        done,
                        iteration,
                        "scan prefix page"
                    );
                    if !keys.is_empty() || !will_continue {
                        this.extend_keys(
                            keys.into_iter()
                                .map(|(k, t, ttl)| (SharedString::from(k), SharedString::from(t), ttl))
                                .collect(),
                        );
                        cx.emit(ServerEvent::KeyTreeUpdated);
                    }
                    if done {
                        this.scan.loaded_prefixes.insert(prefix.clone());
                        this.scan.incomplete_prefixes.remove(&prefix);
                        // A refreshed folder of a fully loaded keyspace is
                        // whole again: so is the keyspace, once no other
                        // refresh is still on its way.
                        if this.scan.complete_before_refresh.remove(&prefix)
                            && this.scan.complete_before_refresh.is_empty()
                        {
                            this.scan.scan_completed = true;
                        }
                    } else if will_continue {
                        // Haven't matched ~80% of key_scan_count yet and still
                        // under the page budget — keep scanning so results keep
                        // streaming in. Clone `prefix` so the original survives
                        // for the spinner cleanup in the `finished` branch below.
                        this.scan_prefix_page(
                            server_id,
                            prefix.clone(),
                            ScanPrefixPage {
                                cursors: Some(new_cursor),
                                iteration: iteration + 1,
                                loaded: batch_loaded,
                                epoch,
                            },
                            cx,
                        );
                        finished = false;
                    } else {
                        // Filled the view (~80% of key_scan_count) or hit the
                        // page budget without finishing — remember the cursor so
                        // the inline "Load more" row can resume the scan here.
                        this.scan.incomplete_prefixes.insert(prefix.clone(), new_cursor);
                    }
                } else {
                    this.scan.keys_superseded_by_next_batch = false;
                    this.scan.refresh_prefixes.clear();
                }
                if finished {
                    // Prefix scan is over (completed, hit the page cap, or
                    // errored) — drop the in-flight marker so the folder
                    // spinner clears on the rebuild emitted just below, and
                    // the tree-wide one a whole-tree prefix query set.
                    this.scan.scanning_prefixes.remove(&prefix);
                    this.scan.scanning = false;
                    cx.emit(ServerEvent::KeyScanFinished);
                    cx.emit(ServerEvent::KeyTreeUpdated);
                    if this.scan.keys.len() == 1
                        && let Some(key) = this.scan.keys.keys().next()
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
        let Some(cursors) = self.scan.incomplete_prefixes.remove(&prefix) else {
            return;
        };
        cx.emit(ServerEvent::KeyScanStarted);
        // Re-mark in-flight (spinner) and resume from the saved cursor with a
        // fresh page budget.
        self.scan.scanning_prefixes.insert(prefix.clone());
        let page = ScanPrefixPage {
            cursors: Some(cursors),
            iteration: 0,
            loaded: 0,
            epoch: self.scan.scan_epoch,
        };
        self.scan_prefix_page(self.server_id.clone(), prefix, page, cx);
    }

    /// Force-refreshes keys under a prefix, bypassing all load caches.
    ///
    /// Unlike `scan_prefix`, this always re-scans from Redis regardless of whether the
    /// prefix was previously loaded or the global scan is complete.  Existing keys under
    /// the prefix are removed first so the result is a clean, up-to-date snapshot.
    pub fn refresh_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Drop any cached state for this prefix, its sub-prefixes — and the
        // folders it sits in: a loaded parent answered `scan_prefix` from
        // the keys at hand, which the removal below just took this folder's
        // out of, so the refresh came back empty.
        self.scan
            .loaded_prefixes
            .retain(|p| !p.as_str().starts_with(prefix.as_str()) && !prefix.as_str().starts_with(p.as_str()));
        // Drop any saved "load more" resume state for this prefix subtree.
        self.scan
            .incomplete_prefixes
            .retain(|p, _| !p.as_str().starts_with(prefix.as_str()));
        // Remove stale keys so the tree shows only what Redis returns
        self.scan.forget_keys_where(|key| key.starts_with(prefix.as_str()));
        // Clear scan_completed so scan_prefix performs a full Redis re-scan
        // rather than short-circuiting to fill_key_types; the re-scan puts it
        // back when it runs to its end (see `complete_before_refresh`).
        // (A refresh started while another is on its way joins it: the flag
        // was already taken, but the keyspace was whole before either.)
        if std::mem::take(&mut self.scan.scan_completed) || !self.scan.complete_before_refresh.is_empty() {
            self.scan.complete_before_refresh.insert(prefix.clone());
        }
        self.scan_prefix(prefix, cx);
    }

    /// Re-scans lazy-loaded folder prefixes saved before a refresh (`⌘R`),
    /// keeping only folders that are still expanded on screen (along with all
    /// of their ancestor folders) while the keyspace scan is still partial.
    pub fn reload_expanded_prefixes(&mut self, expanded_folders: &AHashSet<SharedString>, cx: &mut Context<Self>) {
        let prefixes = std::mem::take(&mut self.scan.refresh_prefixes);
        if prefixes.is_empty() || self.scan.scan_completed || self.keyword_query_mode() == QueryMode::Prefix {
            return;
        }
        let separator = self.key_separator().to_string();
        let max_depth = self.max_key_tree_depth();
        let mut to_reload: Vec<SharedString> = prefixes
            .into_iter()
            .filter(|prefix| {
                let Some(folder) = prefix.as_str().strip_suffix(separator.as_str()) else {
                    return false;
                };
                expanded_folders.contains(folder)
                    && folder_prefixes(folder, &separator, max_depth)
                        .iter()
                        .all(|ancestor| expanded_folders.contains(ancestor.as_str()))
            })
            .collect();
        to_reload.sort_unstable();
        for prefix in to_reload {
            self.scan_prefix(prefix, cx);
        }
    }

    fn get_value(&mut self, key: SharedString, task: ServerTask, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        let at = self.at();
        let server_id = self.server_id.clone();
        let current_key = key.clone();
        let max_truncate_length = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();
        let bypass_size_gate = self.open.size_gate_bypassed.as_ref() == Some(&key);
        let preview = self.open.size_gate_preview.as_ref() == Some(&key);
        // Header decorations, both resolved from what the connect-time probe
        // already learned: whether `OBJECT ENCODING` answers here at all, and
        // which of FREQ / IDLETIME this server's eviction policy makes
        // meaningful. A server that has neither simply gets no chip.
        let with_encoding = self.facts.feature_matrix().is_usable(ServerCommand::ObjectEncoding);
        let heat_probe = get_server_heat_probe(&server_id);

        self.spawn(
            task,
            move || async move {
                let (t, ttl) = key_type_and_ttl(&at, key.as_str()).await?;
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
                // Size gate: String / RedisJSON pull the whole payload in one
                // command, so probe the size first (STRLEN / MEMORY USAGE —
                // both cheap) and stop before fetching something that would
                // stall the UI and spike memory. Collections are exempt —
                // their first loads are already paginated. On a failed probe
                // the gate is skipped rather than blocking the load.
                if !bypass_size_gate
                    && matches!(key_type, KeyType::String | KeyType::Json)
                    && let Ok(size) = key_memory_usage(&at, key.as_str(), key_type.as_str()).await
                    && size > MAX_INLINE_VALUE_SIZE
                {
                    // A String can be read in part (`GETRANGE`), so its
                    // preview is its first bytes; RedisJSON has no such read.
                    if preview && key_type == KeyType::String {
                        let (head, (encoding, heat)) = join(
                            get_redis_bytes_preview(&at, &key, VALUE_PREVIEW_BYTES),
                            key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                        )
                        .await;
                        let (head, total) = head?;
                        return Ok(RedisValue {
                            key_type,
                            data: Some(RedisValueData::Bytes(Arc::new(head))),
                            size,
                            expire_at,
                            encoding: encoding.into(),
                            heat,
                            preview_of: Some(total),
                            ..Default::default()
                        });
                    }
                    return Ok(RedisValue {
                        key_type,
                        status: RedisValueStatus::TooLarge(size),
                        size,
                        expire_at,
                        ..Default::default()
                    });
                }
                // Boxed: this match inlines every type's first-load future
                // into one state machine, and each of those awaits a
                // connection of its own. Unboxed in a debug build the frame
                // outgrew a background thread's 512 KB stack and selecting a
                // key crashed with a stack-guard fault.
                let mut redis_value = Box::pin(async {
                    match key_type {
                        KeyType::String => {
                            let mut data = get_redis_bytes_value(&at, &key).await?;
                            data.detect_and_update(server_id.as_str(), key.as_str(), max_truncate_length);
                            Ok(RedisValue {
                                key_type: KeyType::String,
                                data: Some(RedisValueData::Bytes(Arc::new(data))),
                                ..Default::default()
                            })
                        }
                        KeyType::List => first_load_list_value(&at, &key).await,
                        KeyType::Set => first_load_set_value(&at, &key).await,
                        KeyType::Zset => first_load_zset_value(&at, &key, SortOrder::Asc).await,
                        KeyType::Hash => {
                            let field_ttl = server_supports(&at, floors::HASH_FIELD_TTL).await?;
                            first_load_hash_value(&at, &key, field_ttl).await
                        }
                        KeyType::Stream => first_load_stream_value(&at, &key, true).await,
                        KeyType::Json => get_redis_json_value(&at, &key).await,
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
                        // A module type without a viewer: its DUMP bytes,
                        // behind the size gates — see `load_module_value`.
                        KeyType::Module(_) => {
                            let size = key_memory_usage(&at, key.as_str(), key_type.as_str()).await.ok();
                            load_module_value(&at, server_id.as_str(), key.as_str(), key_type, size, bypass_size_gate)
                                .await
                        }
                        KeyType::Unknown | KeyType::Channel => Err(Error::Invalid {
                            message: format!("unsupported key type: {}", key_type.as_str()),
                        }),
                    }
                })
                .await?;
                // Size and the OBJECT header decorations are both O(1) tail
                // calls on the same multiplexed connection, so they go out
                // together — the chip costs no round trip of its own.
                let (memory_usage, (encoding, heat)) = join(
                    key_memory_usage(&at, key.as_str(), key_type.as_str()),
                    key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                )
                .await;
                if let Ok(memory_usage) = memory_usage {
                    redis_value.size = memory_usage;
                }
                redis_value.encoding = encoding.into();
                redis_value.heat = heat;
                redis_value.expire_at = expire_at;
                Ok(redis_value)
            },
            move |this, result, cx| {
                if this.open.key.as_ref() != Some(&current_key) {
                    return;
                }
                // A "load anyway" the user cancelled: the gate is back, and
                // this reply is not wanted any more.
                if bypass_size_gate && this.open.size_gate_bypassed.as_ref() != Some(&current_key) {
                    return;
                }
                this.open.size_gate_pending = None;
                match result {
                    Ok(value) => {
                        if this.open.value.as_ref() == Some(&value) {
                            return;
                        }
                        let mut tree_changed = false;
                        if !value.is_expired() {
                            tree_changed = if let Some(k) = this.scan.keys.get_mut(&current_key) {
                                if *k != value.key_type {
                                    *k = value.key_type();
                                    true
                                } else {
                                    false
                                }
                            } else {
                                this.scan.keys.insert(current_key.clone(), value.key_type());
                                true
                            };
                        }
                        if let Some(ttl_secs) = value.ttl_secs() {
                            tree_changed |= this.sync_tree_ttl(&current_key, ttl_secs);
                        }
                        if tree_changed {
                            this.scan.renew_tree_id();
                            cx.emit(ServerEvent::KeyTreeUpdated);
                        }
                        this.open.next_epoch();
                        this.open.value = Some(value);
                    }
                    Err(e) => {
                        // Keep the key selected and surface the failure in the
                        // value panel (Failed status) instead of silently
                        // deselecting — a transient toast was otherwise the
                        // only signal, and a blank panel looks identical to
                        // "no key selected". The editor renders this inline
                        // with a retry button.
                        let message: SharedString = e.to_string().into();
                        this.open.next_epoch();
                        match this.open.value.as_mut() {
                            Some(value) => {
                                value.status = RedisValueStatus::Failed(message);
                                value.data = None;
                            }
                            None => {
                                this.open.value = Some(RedisValue {
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

    /// Re-read what the key bar says about the selected key after a write
    /// to it: the size, and the `OBJECT` decorations beside it — the
    /// encoding, which a write can change (`int` to `embstr`, `listpack` to
    /// `quicklist`), and the heat, which the write itself resets. All three
    /// are taken at load, and after a save they went on describing the value
    /// as it was: a string set from `12` to `ABC` still said `int`. Member
    /// counts already move locally. Does not flip `Updating` — the write
    /// already finished.
    pub(crate) fn refresh_value_header(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.open.key.clone() else {
            return;
        };
        if key.is_empty() {
            return;
        }
        let key_type = self
            .open
            .value
            .as_ref()
            .map(|value| value.key_type().as_str().to_string())
            .unwrap_or_default();
        let epoch = self.open.value_epoch;
        let at = self.at();
        // The same two questions the load asks, answered the same way.
        let with_encoding = self.facts.feature_matrix().is_usable(ServerCommand::ObjectEncoding);
        let heat_probe = get_server_heat_probe(self.server_id.as_str());
        self.spawn(
            ServerTask::RefreshValueHeader,
            move || async move {
                let (size, (encoding, heat)) = join(
                    key_memory_usage(&at, key.as_str(), &key_type),
                    key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                )
                .await;
                Ok((size.ok(), encoding, heat))
            },
            move |this, result, cx| {
                if this.open.value_epoch != epoch {
                    return;
                }
                let Ok((size, encoding, heat)) = result else {
                    return;
                };
                let Some(value) = this.open.value.as_mut() else {
                    return;
                };
                if Self::apply_refreshed_header(value, size, encoding.into(), heat) {
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// Put a re-read header on `value`; true when the key bar would change.
    /// A size the server would not give leaves the one shown.
    fn apply_refreshed_header(
        value: &mut RedisValue,
        size: Option<u64>,
        encoding: SharedString,
        heat: HeatMetric,
    ) -> bool {
        let mut changed = size.is_some_and(|size| Self::apply_refreshed_memory_size(value, size));
        if value.encoding != encoding {
            value.encoding = encoding;
            changed = true;
        }
        if value.heat != heat {
            value.heat = heat;
            changed = true;
        }
        changed
    }

    /// True when the header size chip would change.
    fn apply_refreshed_memory_size(value: &mut RedisValue, size: u64) -> bool {
        if value.size == size {
            return false;
        }
        value.size = size;
        true
    }

    /// Runs one type-native operation against the selected key, then
    /// reloads the value.
    ///
    /// A full reload rather than an optimistic edit: `LTRIM` and the pops
    /// change an unbounded number of elements, and a half-applied local
    /// guess is worse than the round trip. The outcome becomes one toast —
    /// the new counter, or what was taken out — because these are the
    /// operations whose *result* is the point.
    pub fn run_key_operation(&mut self, key: SharedString, op: KeyOp, cx: &mut Context<Self>) {
        let at = self.at();
        let key_arg = key.clone();
        // Taken before `op` moves into the task. `None` for the String
        // operations, whose history is before-and-after snapshots instead.
        let description = op.describe();
        // That snapshot: the value this operation is about to change, kept
        // like a save keeps the value it overwrites, so a path write to a
        // document can be diffed and restored.
        // Not a preview's: its bytes are the value's beginning only, and
        // restoring them would cut the key down to that.
        let snapshot = self
            .open
            .value
            .as_ref()
            .filter(|value| value.preview_of().is_none())
            .and_then(|value| value.bytes_value())
            .map(|bytes| bytes.bytes.clone())
            .filter(|bytes| !bytes.is_empty());
        if let Some(bytes) = snapshot {
            self.push_value_history(key.clone(), bytes);
        }
        self.spawn_with_arg(
            ServerTask::RunKeyOperation,
            key.clone(),
            move || async move { Ok(run_key_op(&at, key_arg.as_str(), op).await?) },
            move |this, result, cx| {
                let Ok(outcome) = result else {
                    // The error already surfaced through `spawn_with_arg`.
                    return;
                };
                // Logged as an operation: its effect on individual elements
                // was not read back, so it stays out of the net diff.
                if let Some(description) = description {
                    this.record_changes(key.clone(), vec![ChangeEntry::operation(unix_ts(), description)]);
                }
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let message: SharedString = match outcome {
                    KeyOpOutcome::Number(value) => t!("key_ops.result_number", value = value, locale = locale)
                        .to_string()
                        .into(),
                    KeyOpOutcome::Removed(items) if items.is_empty() => {
                        t!("key_ops.result_nothing", locale = locale).to_string().into()
                    }
                    KeyOpOutcome::Removed(items) => t!(
                        "key_ops.result_removed",
                        count = items.len(),
                        items = items.join(", "),
                        locale = locale
                    )
                    .to_string()
                    .into(),
                    KeyOpOutcome::Count(count) => t!("key_ops.result_count", count = count, locale = locale)
                        .to_string()
                        .into(),
                    KeyOpOutcome::Done => t!("key_ops.result_done", locale = locale).to_string().into(),
                };
                this.emit_info_notification(message, cx);
                this.reload_value(key.clone(), cx);
            },
            cx,
        );
    }

    /// Reloads the value for a selected key.
    pub fn reload_value(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.get_value(key, ServerTask::ReloadValue, cx);
    }

    /// The module types among the loaded keys, for the type-filter menu.
    pub fn module_types_seen(&self) -> Vec<ModuleTypeId> {
        let mut ids: Vec<ModuleTypeId> = self
            .scan
            .keys
            .values()
            .filter_map(|t| match t {
                KeyType::Module(id) => Some(*id),
                _ => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Reloads the value for a key with the oversized-value gate disabled
    /// ("Load anyway" on the too-large panel, "Load whole value" on a
    /// preview). The bypass is remembered for the key so a later refresh
    /// doesn't bounce back to the gate panel.
    pub fn load_value_ignore_size_limit(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.open.size_gate_bypassed = Some(key.clone());
        self.open.size_gate_preview = None;
        // Busy while the (large) payload downloads, and carrying the size
        // the probe found: the editor says "Loading 22.5 MB…" with a Cancel
        // instead of going blank, and the header keeps the real size.
        let replaced = self.open.value.take();
        let size = replaced.as_ref().map_or(0, |value| value.size);
        self.open.size_gate_pending = replaced.map(|value| (key.clone(), value));
        self.open.next_epoch();
        self.open.value = Some(RedisValue {
            status: RedisValueStatus::Loading,
            size,
            ..Default::default()
        });
        self.get_value(key, ServerTask::ReloadValue, cx);
    }
    /// Gives up a "load anyway" that has not landed: what it replaced — the
    /// too-large panel, or the preview — comes back as it was, the key is
    /// gated again, and the reply still on its way is dropped when it
    /// arrives (`get_value`). The download itself is not interrupted: the
    /// reply is already on the shared connection.
    pub fn cancel_large_load(&mut self, cx: &mut Context<Self>) {
        let Some((key, replaced)) = self.open.size_gate_pending.take() else {
            return;
        };
        if self.open.key.as_ref() != Some(&key) {
            return;
        }
        self.open.size_gate_bypassed = None;
        if replaced.preview_of().is_some() {
            self.open.size_gate_preview = Some(key);
        }
        self.open.next_epoch();
        self.open.value = Some(replaced);
        cx.emit(ServerEvent::ValueLoaded);
        cx.notify();
    }
    /// Loads the first [`VALUE_PREVIEW_BYTES`] of a String too large to load
    /// whole ("Preview" on the too-large panel), as a read-only value. The
    /// choice is remembered for the key like the bypass, so a refresh keeps
    /// showing the preview rather than the panel.
    pub fn load_value_preview(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.open.size_gate_preview = Some(key.clone());
        self.open.next_epoch();
        self.open.value = Some(RedisValue {
            status: RedisValueStatus::Loading,
            ..Default::default()
        });
        self.get_value(key, ServerTask::ReloadValue, cx);
    }
    pub fn is_channel_mode(&self) -> bool {
        self.open.value.as_ref().is_some_and(|v| v.key_type == KeyType::Channel)
    }
    /// Sets the channel mode for current server.
    ///
    /// The Pub/Sub panel is the editor's, and the terminal is drawn in the
    /// editor's place: opened from a menu while the terminal was up, the
    /// mode changed underneath and the menu item appeared to do nothing
    /// (#191). Like selecting a key, it takes the terminal off.
    pub fn change_channel_mode(&mut self, cx: &mut Context<Self>) {
        self.open.next_epoch();
        self.open.value = Some(RedisValue {
            key_type: KeyType::Channel,
            ..Default::default()
        });
        self.open.key = None;
        if self.terminal {
            self.terminal = false;
            cx.emit(ServerEvent::TerminalToggled(false));
        }
        cx.notify();
    }

    /// Publishes a message to a Redis channel. `sharded` switches to
    /// `SPUBLISH` (Redis 7+) — on clusters the message is then routed by
    /// the channel's hash slot instead of being broadcast to every node.
    pub fn publish_message(
        &mut self,
        channel: SharedString,
        message: SharedString,
        sharded: bool,
        cx: &mut Context<Self>,
    ) {
        let at = self.at();
        self.spawn_with_arg(
            ServerTask::PublishMessage,
            channel.clone(),
            move || async move {
                publish(&at, channel.as_str(), message.as_str(), sharded).await?;
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
        let same_key = self.open.key.as_ref() == Some(&key);
        self.open.next_epoch();
        self.open.key = Some(key.clone());
        if key.is_empty() {
            return;
        }
        // Opening a key counts as using its history: the budget evicts the
        // key used least recently, and one you keep returning to should stay.
        self.open.histories.touch(&key, unix_ts());
        self.terminal = false;
        // A different key drops the previous payload (type + rows) so the
        // editor cannot paint them under this name. The tree already knows
        // the type. Re-selecting the same key keeps its rows.
        let tree_type = self.scan.keys.get(&key).copied().unwrap_or(KeyType::Unknown);
        self.open.value = Some(RedisValue::for_select(same_key, self.open.value.take(), tree_type));
        if !self.scan.keys.contains_key(&key) {
            self.scan.keys.insert(key.clone(), KeyType::Unknown);
        }
        // MRU: most-recent first, capped per (server, db). Best-effort —
        // a full disk/db error must not block opening the key.
        let scope = recent_keys_scope(self.server_id.as_str(), self.db);
        let key_for_mru = key.to_string();
        cx.background_spawn(async move {
            let _ = get_recent_keys_manager().add_record(&scope, &key_for_mru);
        })
        .detach();
        cx.emit(ServerEvent::KeySelected(key.clone()));
        cx.notify();

        self.get_value(key, ServerTask::Selectkey, cx);
    }
    pub fn delete_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let at = self.at();
        let soft_delete = cx.global::<ZedisGlobalStore>().read(cx).soft_delete();
        let remove_key = key.clone();
        self.spawn_with_arg(
            ServerTask::DeleteKey,
            key.clone(),
            move || async move {
                if soft_delete {
                    stash_key_to_trash(&at, key.as_str()).await;
                }
                Ok(delete_key(&at, key.as_str()).await?)
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.forget_deleted_keys(DeletedKeys::Named(AHashSet::from_iter([remove_key])), cx);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    /// Everything under `folder`, as a key prefix: the folder and the
    /// configured separator.
    pub fn folder_prefix(&self, folder: &str) -> String {
        format!("{folder}{}", self.key_separator())
    }

    /// Everything under `folder`, as a `SCAN MATCH` pattern: the prefix with
    /// its glob characters escaped, so a folder named `user[1]` means the
    /// keys under `user[1]:` and not a character class. The delete, the
    /// count that precedes it and the batch TTL all ask this one function,
    /// so what the confirmation counted is what the click removes.
    pub fn folder_pattern(&self, folder: &str) -> String {
        format!("{}*", escape_glob(&self.folder_prefix(folder)))
    }

    pub fn delete_folder(&mut self, folder: SharedString, cx: &mut Context<Self>) {
        let at = self.at_confirmed();
        let prefix = self.folder_prefix(&folder);
        let pattern = self.folder_pattern(&folder);
        self.spawn_with_arg(
            ServerTask::DeleteKeys,
            prefix.clone(),
            move || async move { Ok(delete_keys_matching(&at, &pattern).await?) },
            move |this, result, cx| {
                match result {
                    Ok(true) => {
                        this.forget_deleted_keys(DeletedKeys::Prefix(prefix.clone()), cx);
                    }
                    // The walk stopped at its round limit: some keys may
                    // still be there. Scan the folder again rather than take
                    // every loaded key under it out of the tree as deleted,
                    // and load the open key again if it was under it, so the
                    // editor shows whether it is still there.
                    Ok(false) => {
                        this.open.histories.retain_keys(|key| !key.starts_with(prefix.as_str()));
                        this.refresh_prefix(prefix.clone().into(), cx);
                        this.refresh_dbsize_now(cx);
                        if let Some(key) = this.open.key.clone().filter(|key| key.starts_with(prefix.as_str())) {
                            this.reload_value(key, cx);
                        }
                    }
                    Err(_) => {}
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    /// `FLUSHDB` / `FLUSHALL` — wipe the current database, or every
    /// database on the instance when `all` is set.
    ///
    /// The caller (status bar) already routed the click through
    /// [`confirm_dangerous_command`](crate::views::confirm_dangerous_command);
    /// the capability check here is the second layer of defence for the
    /// keyboard / command-palette path, mirroring [`Self::bgsave`].
    ///
    /// Nothing is stashed into the local recycle bin: the trash frames one
    /// `DUMP` per key, which is meaningless at whole-database scale — the
    /// same call the folder delete makes.
    pub fn flush_database(&mut self, all: bool, cx: &mut Context<Self>) {
        if !self.can(Capability::FlushDatabase) {
            self.emit_warning_notification(i18n_status_bar(cx, "flush_readonly_blocked"), cx);
            return;
        }
        let at = self.at_confirmed();
        let task = if all { ServerTask::FlushAll } else { ServerTask::FlushDb };
        self.spawn(
            task,
            move || async move {
                if all { flush_all(&at).await } else { flush_db(&at).await }?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.scan.keys.clear();
                    this.scan.key_ttls = Arc::new(AHashMap::new());
                    this.open.histories.clear();
                    this.open.close();
                    // The open database is empty now; and the next beat
                    // re-reads DBSIZE rather than wait out its minute, so the
                    // status bar and the database list stop showing the old
                    // counts.
                    this.readings.flushed();
                    // Force refresh of the key tree view
                    this.scan.renew_tree_id();
                    let message = if all {
                        i18n_status_bar(cx, "flush_all_done_message")
                    } else {
                        i18n_status_bar(cx, "flush_db_done_message")
                    };
                    this.emit_success_notification(message, i18n_status_bar(cx, "flush_done_title"), cx);
                    // DBSIZE / INFO keyspace drive the status bar counters and
                    // the per-db key counts in the dropdown — both are stale
                    // the moment the flush lands.
                    this.refresh_redis_info(cx);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    pub fn unlink_keys(&mut self, keys: Vec<SharedString>, cx: &mut Context<Self>) {
        let at = self.at_confirmed();
        let stash = recycles_deleted_keys(cx.global::<ZedisGlobalStore>().read(cx).soft_delete(), keys.len());
        // A set, not the Vec: the removal below asks it once per loaded key.
        let remove_keys: AHashSet<SharedString> = keys.iter().cloned().collect();
        self.spawn_with_arg(
            ServerTask::DeleteKeys,
            format!("{} keys", remove_keys.len()),
            move || async move {
                // Each one as the one-key delete stashes it, before any is gone.
                if stash {
                    for key in &keys {
                        stash_key_to_trash(&at, key.as_str()).await;
                    }
                }
                Ok(delete_keys(&at, keys.into_iter().map(|k| k.to_string()).collect()).await?)
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.forget_deleted_keys(DeletedKeys::Named(remove_keys), cx);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }
    /// Deletes a specified key.
    pub fn delete_select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.open.value.as_mut() else {
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
        let at = self.at();
        let key_type = self.scan.keys.get(&old).copied();
        let old_done = old.clone();
        let new_done = new.clone();
        let (server_id, db) = (self.server_id.clone(), self.db);
        self.spawn_with_arg(
            ServerTask::RenameKey,
            new.clone(),
            move || async move {
                let renamed = rename_key(&at, old.as_str(), new.as_str(), overwrite).await?;
                // Here rather than in the callback: off the UI thread, and
                // done before the tree is rebuilt and the key re-selected,
                // both of which read these records.
                if renamed {
                    move_local_records(server_id.as_str(), db, old.as_str(), new.as_str());
                }
                Ok(renamed)
            },
            move |this, result, cx| {
                match result {
                    Ok(true) => {
                        if let Some(kt) = key_type {
                            this.scan.keys.insert(new_done.clone(), kt);
                        }
                        this.scan.keys.remove(&old_done);
                        // The key tree renders each key's TTL state from this
                        // cache (no entry → the TTL icon goes missing), so move
                        // it across too. RENAME preserves the TTL server-side.
                        let key_ttls = Arc::make_mut(&mut this.scan.key_ttls);
                        if let Some(ttl) = key_ttls.remove(&old_done) {
                            key_ttls.insert(new_done.clone(), ttl);
                        }
                        this.open.histories.rename(&old_done, new_done.clone());
                        this.scan.renew_tree_id();
                        // Re-select via the normal path so the key tree expands
                        // to reveal the renamed node — `KeySelected` drives the
                        // tree's `update_expand`; a bare rebuild would leave the
                        // key hidden under a collapsed folder. Also reloads the
                        // value into the editor.
                        if this.open.key.as_ref() == Some(&old_done) {
                            this.select_key(new_done.clone(), cx);
                        }
                        // A clobbering RENAME leaves the database one key
                        // short — the one that had the name. Ask for the
                        // total now, as a delete does: the status bar read
                        // 25/26 until the heartbeat's next `DBSIZE`.
                        if overwrite {
                            this.refresh_dbsize_now(cx);
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
    /// Write `ttl_secs` into the tree cache when TTL chips are on and the
    /// chip would read differently — the same gate auto-refresh uses, so a
    /// select that still paints `10m` does not rebuild the tree.
    fn sync_tree_ttl(&mut self, key: &SharedString, ttl_secs: i64) -> bool {
        if !self.prefs.show_key_tree_ttl() {
            return false;
        }
        let changed = self
            .scan
            .key_ttls
            .get(key)
            .is_none_or(|old| format_ttl_chip(*old) != format_ttl_chip(ttl_secs));
        if !changed {
            return false;
        }
        Arc::make_mut(&mut self.scan.key_ttls).insert(key.clone(), ttl_secs);
        true
    }

    fn apply_tree_ttl(&mut self, key: &SharedString, ttl_secs: i64, cx: &mut Context<Self>) {
        if self.sync_tree_ttl(key, ttl_secs) {
            self.scan.renew_tree_id();
            cx.emit(ServerEvent::KeyTreeUpdated);
        }
    }

    /// Updates the TTL (expiration) for a key.
    pub fn update_key_ttl(&mut self, key: SharedString, ttl: SharedString, cx: &mut Context<Self>) {
        if ttl.is_empty() {
            return;
        }
        let at = self.at();
        let Some(value) = self.open.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;
        let epoch = self.open.value_epoch;

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
        let tree_key = key.clone();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            key.clone(),
            move || async move {
                if !parse_fail_error.is_empty() {
                    return Err(Error::Invalid {
                        message: parse_fail_error,
                    });
                }
                expire_key(&at, key.as_str(), new_ttl.as_secs()).await?;
                Ok(ttl)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.apply_tree_ttl(&tree_key, new_ttl.as_secs() as i64, cx);
                }
                // The value on screen now is another one's: its expiry and
                // status are not this change's to restore.
                if this.open.value_epoch != epoch {
                    return;
                }
                if let Some(value) = this.open.value.as_mut() {
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

    /// Sets an absolute expiry (`EXPIREAT`) instead of a countdown. The UI
    /// offers both because they answer different questions — "keep this for
    /// an hour" and "drop this at midnight" — and the second one cannot be
    /// expressed as a duration without the user doing the arithmetic that
    /// the clock drift then invalidates.
    ///
    /// A past instant is refused by the caller, not here: `EXPIREAT` would
    /// happily delete the key, which is a destructive act that belongs to
    /// the delete button, not to a typo in a date field.
    pub fn update_key_expire_at(&mut self, key: SharedString, at: i64, cx: &mut Context<Self>) {
        let server_db = self.at();
        let Some(value) = self.open.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;
        let epoch = self.open.value_epoch;
        value.expire_at = Some(at);
        cx.notify();
        let tree_key = key.clone();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            key.clone(),
            move || async move {
                expire_key_at(&server_db, key.as_str(), at).await?;
                Ok(at)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let ttl_secs = if at < 0 {
                        at
                    } else {
                        at.saturating_sub(unix_ts()).max(0)
                    };
                    this.apply_tree_ttl(&tree_key, ttl_secs, cx);
                }
                // The value on screen now is another one's: its expiry and
                // status are not this change's to restore.
                if this.open.value_epoch != epoch {
                    return;
                }
                if let Some(value) = this.open.value.as_mut() {
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

    /// Drops the selected key's expiry (`PERSIST`). The editor TTL dropdown
    /// is the one-key path; folders and multi-select go through
    /// [`Self::batch_set_ttl_keys`] with `ttl_secs = None`.
    pub fn persist_key_ttl(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let at = self.at();
        let Some(value) = self.open.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;
        let epoch = self.open.value_epoch;
        // Same encoding the load path uses for `TTL -1` (no expiry).
        value.expire_at = Some(-1);
        cx.notify();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            key.clone(),
            {
                let key = key.clone();
                move || async move {
                    persist_key(&at, key.as_str()).await?;
                    Ok(())
                }
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.apply_tree_ttl(&key, -1, cx);
                }
                // The value on screen now is another one's: its expiry and
                // status are not this change's to restore.
                if this.open.value_epoch != epoch {
                    return;
                }
                if let Some(value) = this.open.value.as_mut() {
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
    /// Some` issues `EXPIRE`, `None` issues `PERSIST`; `condition` (NX / XX /
    /// GT / LT, Redis 7.0+ — the caller gates on the version) makes the server
    /// skip keys that don't qualify. Cluster-safe via `set_ttl_keys_scattered`.
    /// The local TTL cache is updated only for the keys the server actually
    /// changed, and a conditional run reports its applied / skipped split.
    pub fn batch_set_ttl_keys(
        &mut self,
        keys: Vec<SharedString>,
        ttl_secs: Option<u64>,
        condition: Option<ExpireCondition>,
        cx: &mut Context<Self>,
    ) {
        if keys.is_empty() {
            return;
        }
        let at = self.at();
        let affected = keys.clone();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            format!("{} keys", keys.len()),
            move || async move {
                let keys = keys.into_iter().map(|k| k.to_string()).collect();
                Ok(set_keys_ttl(&at, keys, ttl_secs, condition).await?)
            },
            move |this, result, cx| {
                if let Ok(applied) = result {
                    let new_ttl = ttl_secs.map(|s| s as i64).unwrap_or(-1);
                    let key_ttls = Arc::make_mut(&mut this.scan.key_ttls);
                    let mut changed = 0usize;
                    let mut includes_open_key = false;
                    for (key, done) in affected.iter().zip(&applied) {
                        if *done {
                            key_ttls.insert(key.clone(), new_ttl);
                            changed += 1;
                            includes_open_key |= this.open.key.as_ref() == Some(key);
                        }
                    }
                    if includes_open_key {
                        this.follow_batch_ttl(ttl_secs);
                    }
                    this.scan.renew_tree_id();
                    this.notify_batch_ttl_outcome(condition, changed, affected.len().saturating_sub(changed), cx);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    /// Batch-apply a TTL to every key under a folder prefix. Scans `prefix*`
    /// across masters (like `delete_folder`) and applies in pages; the task
    /// hands back the keys the server changed plus the count it skipped.
    pub fn batch_set_ttl_folder(
        &mut self,
        folder: SharedString,
        ttl_secs: Option<u64>,
        condition: Option<ExpireCondition>,
        cx: &mut Context<Self>,
    ) {
        let at = self.at();
        let prefix = self.folder_prefix(&folder);
        let pattern = self.folder_pattern(&folder);
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            prefix,
            move || async move { Ok(set_ttl_matching(&at, &pattern, ttl_secs, condition).await?) },
            move |this, result, cx| {
                if let Ok((changed, skipped)) = result {
                    let new_ttl = ttl_secs.map(|s| s as i64).unwrap_or(-1);
                    let changed_set: AHashSet<&str> = changed.iter().map(String::as_str).collect();
                    for (k, v) in Arc::make_mut(&mut this.scan.key_ttls).iter_mut() {
                        if changed_set.contains(&**k) {
                            *v = new_ttl;
                        }
                    }
                    if this
                        .open
                        .key
                        .as_ref()
                        .is_some_and(|key| changed_set.contains(key.as_ref()))
                    {
                        this.follow_batch_ttl(ttl_secs);
                    }
                    this.scan.renew_tree_id();
                    this.notify_batch_ttl_outcome(condition, changed.len(), skipped, cx);
                }
                cx.emit(ServerEvent::KeyTreeUpdated);
                cx.notify();
            },
            cx,
        );
    }

    /// The open key's header follows a batch TTL change that included it.
    /// The batch writes the tree's chips; the header reads the loaded
    /// value, which it left alone — so a key set to expire in fifteen
    /// minutes from the tree went on saying "No TTL" on the right until it
    /// was opened again (#185).
    fn follow_batch_ttl(&mut self, ttl_secs: Option<u64>) {
        if let Some(value) = self.open.value.as_mut() {
            value.expire_at = Some(expire_at_after(ttl_secs, unix_ts()));
        }
    }

    /// One toast per batch TTL run that used a condition — the server's
    /// applied / skipped split is the whole point of asking for one.
    fn notify_batch_ttl_outcome(
        &self,
        condition: Option<ExpireCondition>,
        applied: usize,
        skipped: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(condition) = condition else {
            return;
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let message = t!(
            "key_tree.batch_ttl_result",
            condition = condition.word(),
            applied = applied,
            skipped = skipped,
            locale = locale
        );
        self.emit_success_notification(message.to_string().into(), i18n_key_tree(cx, "batch_ttl_title"), cx);
    }

    /// The keys of `restored` that belong in this tree: the ones the recycle
    /// bin put back into the server and database it is showing. The bin
    /// lists a server's deleted keys across all its databases, and a key
    /// restored into another one is not a row here.
    fn restored_here(&self, server_id: &str, restored: &[RestoredKey]) -> Vec<RestoredKey> {
        if self.server_id.as_ref() != server_id {
            return Vec::new();
        }
        restored.iter().filter(|key| key.db == self.db).cloned().collect()
    }

    /// Put keys the recycle bin restored back in the tree, and recount.
    ///
    /// The bin restores from a dialog, over a connection of its own
    /// (`restore_key`), and used to tell nobody: the key was back in Redis,
    /// the toast said so, and the tree went on without it under the old
    /// count until something reloaded it (#173). A bin entry is a `DUMP`
    /// payload and does not record the type, so that is asked here — one
    /// `TYPE` per key — before the rows are drawn with a badge.
    pub fn note_keys_restored(&mut self, server_id: &str, restored: &[RestoredKey], cx: &mut Context<Self>) {
        let restored = self.restored_here(server_id, restored);
        if restored.is_empty() {
            return;
        }
        let at = self.at();
        // -2 is "not fetched", which is what a scan reports while the tree
        // shows no TTL chips; the restored expiry is known either way.
        let with_ttl = self.show_key_tree_ttl();
        self.spawn(
            ServerTask::FillKeyTypes,
            move || async move {
                let names: Vec<String> = restored.iter().map(|key| key.key.to_string()).collect();
                let types = key_types(&at, names).await?;
                Ok(restored
                    .into_iter()
                    .zip(types)
                    // "none": deleted again, or expired, between the restore
                    // and this question. It is not a row.
                    .filter(|(_, key_type)| key_type != "none")
                    .map(|(key, key_type)| {
                        let ttl = if with_ttl { key.ttl_secs() } else { -2 };
                        (key.key, SharedString::from(key_type), ttl)
                    })
                    .collect::<Vec<_>>())
            },
            move |this, result, cx| {
                if let Ok(rows) = result {
                    this.extend_keys(rows);
                    // The tree rebuilds on this event and on nothing else.
                    cx.emit(ServerEvent::KeyTreeUpdated);
                }
                // The keys are back whether or not their types could be
                // asked, so the total has changed either way.
                this.refresh_dbsize_now(cx);
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
        let at = self.at();
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
                let ttl_secs = if ttl.is_empty() {
                    None
                } else if let Ok(secs) = ttl.parse::<u64>() {
                    Some(secs)
                } else {
                    let ttl = humantime::parse_duration(&ttl).map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Some(ttl.as_secs())
                };
                let command = key_type.create_command();
                if command.is_empty() {
                    return Err(Error::Invalid {
                        message: "Invalid key type".to_string(),
                    });
                }
                let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
                if !create_key(&at, key.as_str(), command, &args, ttl_secs).await? {
                    return Err(Error::Invalid {
                        message: "Key already exists".to_string(),
                    });
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.scan.keys.insert(key_clone.clone(), key_type);
                    Arc::make_mut(&mut this.scan.key_ttls).insert(key_clone.clone(), ttl_secs);
                    this.scan.renew_tree_id();
                    this.select_key(key_clone, cx);
                    // Rebuild the tree from `keys` so the new row actually
                    // appears (without this it stays hidden until a refresh).
                    cx.emit(ServerEvent::KeyTreeUpdated);
                    this.refresh_dbsize_now(cx);
                }
                cx.notify();
            },
            cx,
        );
    }
}

/// A key of a type Zedis has no viewer for. The value is shown as its
/// serialized form (`DUMP`), read-only, behind two gates: the auto-load cap
/// the string editor uses (the "load anyway" path lifts it), and a hard cap
/// above which DUMP is never sent — it serializes the whole value on the
/// server's main thread with no way to page, so a huge module value is
/// read from the terminal, not clicked on. `size` is what `MEMORY USAGE`
/// said: a module reports it through its own `mem_usage` callback, and a
/// missing one answers 0 or an error — unknown, which is not "small", and
/// unknown never gets a DUMP. A value the card cannot show still carries
/// its type, size and TTL, so rename / TTL / delete / copy keep working.
async fn load_module_value(
    at: &ServerDb,
    server_id: &str,
    key: &str,
    key_type: KeyType,
    size: Option<u64>,
    bypass_size_gate: bool,
) -> Result<RedisValue, Error> {
    let size = size.filter(|s| *s > 0);
    let mut value = RedisValue {
        key_type,
        size: size.unwrap_or(0),
        ..Default::default()
    };
    let Some(size) = size else {
        return Ok(value);
    };
    if size > MAX_MODULE_DUMP_BYTES {
        return Ok(value);
    }
    if !bypass_size_gate && size > MAX_INLINE_VALUE_SIZE {
        value.status = RedisValueStatus::TooLarge(size);
        return Ok(value);
    }
    if !get_server_features(server_id).is_usable(ServerCommand::Dump) {
        return Ok(value);
    }
    let payload = dump_key(at, key).await?;
    value.data = Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
        bytes: Bytes::from(payload),
        view_mode: ViewMode::Hex,
        ..Default::default()
    })));
    Ok(value)
}

#[cfg(test)]
mod bulk_delete_tests {
    use super::{DeletedKeys, TRASH_MAX_BULK_KEYS, recycles_deleted_keys};
    use crate::states::{KeyType, RedisValue, ZedisServerState};
    use ahash::AHashSet;
    use gpui::{AppContext, SharedString, TestAppContext};

    fn named(keys: &[&str]) -> DeletedKeys {
        DeletedKeys::Named(
            keys.iter()
                .map(|key| SharedString::from(key.to_string()))
                .collect::<AHashSet<_>>(),
        )
    }

    fn open(state: &mut ZedisServerState, key: &str) {
        state.open.key = Some(SharedString::from(key.to_string()));
        state.open.value = Some(RedisValue::default());
    }

    /// A delete closes the open key when it removed it, whichever way the
    /// keys were named — a multi-selection left it open, showing a value that
    /// was gone (#197), and so did a folder — and leaves any other key open.
    #[gpui::test]
    fn a_delete_closes_the_open_key_it_removed(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            for key in ["cart:2001", "cart:2002", "user:1", "user:2", "order:1"] {
                state.scan.keys.insert(SharedString::from(key), KeyType::Hash);
            }

            open(state, "cart:2002");
            state.forget_deleted_keys(named(&["cart:2001"]), cx);
            assert_eq!(state.open.key.as_deref(), Some("cart:2002"), "not one of them");
            state.forget_deleted_keys(named(&["cart:2001", "cart:2002"]), cx);
            assert!(
                state.open.key.is_none() && state.open.value.is_none(),
                "one of a multi-selection"
            );

            open(state, "user:1");
            state.forget_deleted_keys(DeletedKeys::Prefix("order:".into()), cx);
            assert_eq!(state.open.key.as_deref(), Some("user:1"), "another folder");
            state.forget_deleted_keys(DeletedKeys::Prefix("user:".into()), cx);
            assert!(
                state.open.key.is_none() && state.open.value.is_none(),
                "under the folder"
            );

            let left: Vec<&str> = state.scan.keys.keys().map(|key| key.as_ref()).collect();
            assert!(left.is_empty(), "every deleted key leaves the tree: {left:?}");
        });
    }

    /// A selection deleted together goes to the bin like one key does,
    /// up to a sweep's size, and never with the bin switched off (#195).
    #[test]
    fn a_hand_made_selection_goes_to_the_bin() {
        assert!(recycles_deleted_keys(true, 2));
        assert!(recycles_deleted_keys(true, TRASH_MAX_BULK_KEYS));
        assert!(!recycles_deleted_keys(true, TRASH_MAX_BULK_KEYS + 1));
        assert!(!recycles_deleted_keys(false, 2));
    }
}

#[cfg(test)]
mod auto_refresh_tests {

    #[test]
    fn a_keyword_scans_as_the_text_it_is() {
        use crate::states::ZedisServerState as State;
        assert_eq!(State::keyword_scan_pattern("", false, false), "*");
        assert_eq!(State::keyword_scan_pattern("user", false, false), "*user*");
        // Glob characters in a keyword are the key's own characters.
        assert_eq!(State::keyword_scan_pattern("cache[v2]", false, false), r"*cache\[v2\]*");
        assert_eq!(
            State::keyword_scan_pattern("user:*:profile", false, false),
            r"*user:\*:profile*"
        );
        // Prefix mode anchors at the start; a regex keyword scans everything.
        assert_eq!(State::keyword_scan_pattern("user[1]:", true, false), r"user\[1\]:*");
        assert_eq!(State::keyword_scan_pattern("^order:\\d+$", false, true), "*");
    }

    use super::*;

    /// With the regex toggle on there is one way to search, whatever the
    /// box's mode says (#167).
    #[test]
    fn a_regex_keyword_takes_the_place_of_the_query_mode() {
        for mode in [QueryMode::All, QueryMode::Prefix, QueryMode::Exact] {
            assert_eq!(
                ZedisServerState::resolve_query_mode(mode, true),
                QueryMode::All,
                "{mode:?}"
            );
            assert_eq!(ZedisServerState::resolve_query_mode(mode, false), mode);
        }
    }

    /// The pages of a regex scan are narrowed by the keyword itself, so the
    /// scan's "enough loaded" counts matches and not every name listed.
    #[test]
    fn the_name_filter_is_the_keyword_read_as_a_regex() {
        let filter = ZedisServerState::compile_name_filter("^zedis:fn:session", true).expect("it compiles");
        assert!(filter.is_match("zedis:fn:session:1001"));
        assert!(filter.is_match("zedis:fn:session:1003r"));
        assert!(!filter.is_match("other:zedis:fn:session:1001"));
        // A substring keyword is the server's `MATCH`, an empty box is no
        // filter, and a half-typed pattern narrows nothing.
        assert!(ZedisServerState::compile_name_filter("^zedis:fn:session", false).is_none());
        assert!(ZedisServerState::compile_name_filter("", true).is_none());
        assert!(ZedisServerState::compile_name_filter("user:(", true).is_none());
    }

    /// The bin lists one server's deleted keys across all its databases.
    /// Only the ones restored into the server and database on screen are
    /// rows of this tree (#173 — until then none of them were).
    /// Pubsub Mode is the editor's panel: reached from a menu while the
    /// terminal covers the editor, it has to take the terminal off, or the
    /// menu item appears to do nothing (#191).
    #[gpui::test]
    fn pubsub_mode_takes_the_terminal_off_the_editor(cx: &mut gpui::TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.toggle_terminal(cx);
            assert!(state.is_terminal());
            state.change_channel_mode(cx);
            assert!(state.is_channel_mode());
            assert!(!state.is_terminal());
        });
    }

    #[gpui::test]
    fn a_restored_key_is_a_row_only_in_the_tree_it_belongs_to(cx: &mut gpui::TestAppContext) {
        let here = RestoredKey {
            key: "here".into(),
            db: 2,
            pttl_ms: 1500,
        };
        let elsewhere = RestoredKey {
            key: "other-db".into(),
            db: 0,
            pttl_ms: -1,
        };
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, _| {
            state.server_id = "one".into();
            state.db = 2;
            let restored = [here.clone(), elsewhere.clone()];
            assert_eq!(state.restored_here("one", &restored), std::slice::from_ref(&here));
            assert!(state.restored_here("two", &restored).is_empty(), "another server's bin");
        });
        // The chip counts whole seconds, rounded up; no expiry is -1.
        assert_eq!(here.ttl_secs(), 2);
        assert_eq!(elsewhere.ttl_secs(), -1);
    }

    fn loaded(keys: &[&str]) -> LoadedKeys {
        keys.iter()
            .map(|k| (SharedString::from(k.to_string()), KeyType::String))
            .collect()
    }

    fn scanned(keys: &[&str]) -> Vec<(String, String, i64)> {
        keys.iter().map(|k| (k.to_string(), "string".to_string(), -1)).collect()
    }

    fn names(rows: &[(SharedString, SharedString, i64)]) -> Vec<&str> {
        rows.iter().map(|(k, _, _)| k.as_ref()).collect()
    }

    /// The round finished (cursors at 0): a key the server no longer lists
    /// is gone, and a new one arrives.
    #[test]
    fn a_complete_round_removes_what_the_server_no_longer_lists() {
        let plan = ZedisServerState::plan_auto_refresh(true, scanned(&["a", "c"]), &loaded(&["a", "b"]), None);
        assert_eq!(names(&plan.add), ["c"]);
        assert_eq!(plan.remove, vec![SharedString::from("b")]);
    }

    /// The round did *not* finish: its result is a sample, and a key missing
    /// from a sample has not been shown to be missing from the server. This
    /// is the case that used to delete live keys from the tree.
    #[test]
    fn an_incomplete_round_adds_but_never_removes() {
        let plan = ZedisServerState::plan_auto_refresh(false, scanned(&["a", "c"]), &loaded(&["a", "b"]), None);
        assert_eq!(names(&plan.add), ["c"], "what the round did see is still added");
        assert!(plan.remove.is_empty(), "b was simply not in this round's sample");
    }

    #[test]
    fn a_key_in_both_is_neither_added_nor_removed() {
        let plan = ZedisServerState::plan_auto_refresh(true, scanned(&["a"]), &loaded(&["a"]), None);
        assert!(plan.add.is_empty() && plan.remove.is_empty() && plan.ttls.is_empty());
    }

    /// A loaded key's TTL is refreshed when its chip would change, and left
    /// alone when it would not (or when the tree shows no TTLs).
    #[test]
    fn a_loaded_key_gets_its_new_ttl_when_the_chip_would_change() {
        let ttls: AHashMap<SharedString, i64> = [("a", 600_i64), ("b", 600), ("c", -1)]
            .into_iter()
            .map(|(k, ttl)| (SharedString::from(k), ttl))
            .collect();
        let round = vec![
            ("a".to_string(), "string".to_string(), 540),
            ("b".to_string(), "string".to_string(), 630),
            ("c".to_string(), "string".to_string(), 30),
        ];
        let plan = ZedisServerState::plan_auto_refresh(true, round.clone(), &loaded(&["a", "b", "c"]), Some(&ttls));
        let mut changed: Vec<(&str, i64)> = plan.ttls.iter().map(|(k, ttl)| (k.as_ref(), *ttl)).collect();
        changed.sort_unstable();
        // 10m → 9m and ∞ → 30s change the chip; 600 s → 630 s reads 10m both
        // times, so `b` is left as it was.
        assert_eq!(changed, vec![("a", 540), ("c", 30)]);
        let hidden = ZedisServerState::plan_auto_refresh(true, round, &loaded(&["a", "b", "c"]), None);
        assert!(hidden.ttls.is_empty());
    }
}

#[cfg(test)]
mod tree_ttl_tests {
    use super::*;
    use crate::states::ZedisAppState;

    fn key(name: &str) -> SharedString {
        SharedString::from(name)
    }

    /// Selecting a key writes the fresh TTL only when chips are on and the
    /// label would change — 600s and 630s both read `10m`.
    /// What the open key's header is given after a batch TTL change: the
    /// instant an `EXPIRE` lands on, and the load path's "no expiry" after a
    /// `PERSIST`.
    #[test]
    fn a_batch_ttl_change_reads_like_a_fresh_load() {
        assert_eq!(expire_at_after(Some(900), 1_000), 1_900);
        assert_eq!(expire_at_after(None, 1_000), -1);
        assert_eq!(expire_at_after(Some(u64::MAX), 1_000), i64::MAX);
    }

    #[test]
    fn sync_tree_ttl_writes_only_when_the_chip_changes() {
        let mut state = ZedisServerState::default();
        let k = key("session");
        assert!(!state.sync_tree_ttl(&k, 30), "chips default off");
        assert!(state.scan.key_ttls.get(&k).is_none());

        let mut settings = ZedisAppState::default();
        settings.set_show_key_tree_ttl(true);
        state.prefs.resolve_tree(None, &settings);
        assert!(state.sync_tree_ttl(&k, 600), "first sample");
        assert_eq!(state.scan.key_ttls.get(&k), Some(&600));
        assert!(!state.sync_tree_ttl(&k, 630), "same 10m chip");
        assert_eq!(state.scan.key_ttls.get(&k), Some(&600), "stale seconds stay");
        assert!(state.sync_tree_ttl(&k, 540), "10m → 9m");
        assert_eq!(state.scan.key_ttls.get(&k), Some(&540));
        assert!(state.sync_tree_ttl(&k, -1), "timed → persist");
        assert_eq!(state.scan.key_ttls.get(&k), Some(&-1));
    }
}

#[cfg(test)]
mod memory_size_tests {
    use super::*;

    #[test]
    fn apply_refreshed_memory_size_only_when_it_changed() {
        let mut value = RedisValue {
            size: 128,
            ..Default::default()
        };
        assert!(!ZedisServerState::apply_refreshed_memory_size(&mut value, 128));
        assert_eq!(value.size, 128);
        assert!(ZedisServerState::apply_refreshed_memory_size(&mut value, 160));
        assert_eq!(value.size, 160);
    }

    /// After a save the key bar takes the encoding and heat asked again, not
    /// only the size: `12` saved as `ABC` went on saying `int`, idle 2m.
    #[test]
    fn a_refreshed_header_replaces_encoding_and_heat() {
        let mut value = RedisValue {
            size: 48,
            encoding: "int".into(),
            heat: HeatMetric::IdleTime(120),
            ..Default::default()
        };
        let unchanged =
            ZedisServerState::apply_refreshed_header(&mut value, Some(48), "int".into(), HeatMetric::IdleTime(120));
        assert!(!unchanged);
        assert!(ZedisServerState::apply_refreshed_header(
            &mut value,
            Some(48),
            "embstr".into(),
            HeatMetric::IdleTime(0)
        ));
        assert_eq!(value.encoding.as_ref(), "embstr");
        assert_eq!(value.heat, HeatMetric::IdleTime(0));
        // A size the server would not give leaves the one shown.
        ZedisServerState::apply_refreshed_header(&mut value, None, "embstr".into(), HeatMetric::IdleTime(0));
        assert_eq!(value.size, 48);
    }
}
