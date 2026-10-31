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

//! The key tree's half of a connection: what is asked (the keyword, the
//! query mode, the filters), what the scan has brought back (the keys, their
//! types and TTLs) and how far it got (the cursors, the folders loaded).

use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct KeyScan {
    /// Search keyword for filtering keys
    pub(super) keyword: SharedString,

    /// SCAN cursors for cluster nodes (one per node)
    pub(super) cursors: Option<Vec<u64>>,

    /// Whether a scan operation is in progress
    pub(super) scanning: bool,

    /// Whether the current scan has completed
    pub(super) scan_completed: bool,

    /// Number of scan iterations performed
    pub(super) scan_times: usize,

    /// Monotonic scan-session counter, bumped by [`Self::reset_scan`]. Every
    /// in-flight prefix scan captures the epoch at launch and bails on
    /// completion if it no longer matches — so a lazy folder scan / "Load
    /// more" still streaming when the user re-runs the search can't write its
    /// stale keys and `incomplete_prefixes`/`loaded_prefixes` entries back
    /// onto the freshly-reset tree. A keyword check alone can't catch this:
    /// an empty-keyword browse re-searched with an empty keyword keeps the
    /// same keyword, so only a generation bump distinguishes the sessions.
    pub(super) scan_epoch: u64,

    /// Unique ID for current key tree (changes when keys are reloaded)
    pub(super) key_tree_id: SharedString,

    /// Set of prefixes that have been scanned (for lazy loading folders)
    pub(super) loaded_prefixes: AHashSet<SharedString>,

    /// Lazy-loaded folder prefixes saved by [`Self::reset`] when
    /// `keep_rows` is `true` (`begin_refresh_scan`), so expanded lazy folders
    /// can be re-scanned once the top-level refresh round finishes.
    pub(super) refresh_prefixes: AHashSet<SharedString>,

    /// Prefixes whose lazy folder-scan (`scan_prefix`) is currently in
    /// flight, stored in the same `"{folder_id}:"` form `scan_prefix`
    /// receives. Drives the inline spinner on the matching folder row;
    /// entries are removed as each prefix scan finishes and the whole set
    /// is cleared on reset / server switch.
    pub(super) scanning_prefixes: AHashSet<SharedString>,

    /// Folders re-scanned by `refresh_prefix` while the whole keyspace was
    /// loaded (`scan_completed`). The refresh has to clear that flag — its
    /// keys are gone until the re-scan lands — and it comes back once every
    /// such re-scan has run to its end, where it used to stay off for good
    /// and every later folder open went back to the server.
    pub(super) complete_before_refresh: AHashSet<SharedString>,

    /// Prefixes whose lazy scan stopped at the per-load page cap before
    /// completing, mapped to the SCAN cursors to resume from. Drives the
    /// inline "Load more" row under the folder; an entry is removed once
    /// its prefix finishes (or is refreshed) and the whole map is cleared
    /// on reset / server switch.
    pub(super) incomplete_prefixes: AHashMap<SharedString, Vec<u64>>,

    /// Every loaded key and its type ([`LoadedKeys`]).
    pub(super) keys: LoadedKeys,

    /// How many keys the tree lists, where that is not all of `keys`: a
    /// filter the tree applies itself (a tag, a TTL range, a regex) narrowed
    /// them. Written by the tree after each build — `set_listed_keys`.
    pub(super) listed_keys: Option<usize>,

    /// Parallel map of per-key TTL in seconds, populated alongside `keys`
    /// during SCAN. `-1` = no expiry, `-2` = missing (key vanished between
    /// SCAN and the TTL pipeline).
    ///
    /// Behind an `Arc` so the key tree's background build snapshots it with
    /// an Arc clone instead of a structural copy (with 500k keys that copy
    /// was ~20MB resident, twice). Writers go through `Arc::make_mut`: the
    /// map is copied only when a write lands while a build holds it. Keep it
    /// that way — a reader that keeps the `Arc` between builds turns every
    /// write into a copy of the whole map (the key tree once did).
    pub(super) key_ttls: Arc<AHashMap<SharedString, i64>>,

    /// Set by [`Self::begin_refresh_scan`]: the rows currently on screen are
    /// the *previous* answer to the question the new scan is asking, kept so
    /// the tree never blanks, and owed to whichever batch lands first —
    /// `extend_keys` takes the flag and empties them before inserting.
    /// `false` everywhere else, including after a plain [`Self::reset_scan`],
    /// where the rows are gone already.
    pub(super) keys_superseded_by_next_batch: bool,

    /// Optional filter to show only keys of one native type (`SCAN ... TYPE`).
    pub(super) type_filter: Option<KeyType>,

    /// The key tree reads its keyword box as a regex. `SCAN` has no regex,
    /// so this turns the server-side `MATCH` off and every page of names is
    /// narrowed on this side (`name_filter`) — a full walk of the keyspace,
    /// which is the price of a regex and is why it is off by default. It
    /// also takes the query mode's place: see `keyword_query_mode`.
    pub(super) regex_keyword: bool,

    /// Query mode (All/Prefix/Exact) for key filtering
    pub(super) query_mode: QueryMode,

    /// Search history
    pub(super) search_history: Vec<SharedString>,
}

impl KeyScan {
    /// A new tree for the view: the rows it builds from changed.
    pub(super) fn renew_tree_id(&mut self) {
        self.key_tree_id = Uuid::now_v7().to_string().into();
    }

    /// What a server switch forgets before its scan starts over: the rows,
    /// the type filter and the regex toggle (the key tree turns it off on
    /// the same event; left on here, the next keyword was scanned as a regex
    /// under a box that said substring). The query mode, the keyword and the
    /// search history are the next `select`'s and `reset`'s to set.
    pub(super) fn forget_server(&mut self) {
        self.keys.clear();
        self.listed_keys = None;
        self.key_ttls = Arc::new(AHashMap::new());
        self.key_tree_id = SharedString::default();
        // Cleared on server switch (but NOT in reset_scan, which a filter
        // change triggers and must preserve the just-set filter).
        self.type_filter = None;
        self.regex_keyword = false;
    }

    /// A new scan session — see [`ZedisServerState::reset_scan`] and
    /// [`ZedisServerState::begin_refresh_scan`] for `keep_rows`.
    pub(super) fn reset(&mut self, keep_rows: bool) {
        self.keyword = SharedString::default();
        // A refresh owes its rows to the first batch back; a new query owns
        // them outright and empties them right here.
        self.keys_superseded_by_next_batch = keep_rows;
        self.cursors = None;
        if !keep_rows {
            self.keys.clear();
            self.listed_keys = None;
            // Fresh Arc instead of `make_mut(..).clear()` — when a build still
            // holds the old snapshot, `make_mut` would copy the whole map just
            // to empty it.
            self.key_ttls = Arc::new(AHashMap::new());
            // The rows changed (to none); a refresh leaves them as they are,
            // so re-identifying the tree would only cost a rebuild of the
            // same rows. `extend` renews it when the new batch lands.
            self.renew_tree_id();
        }
        self.scanning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        // New scan session: bump the epoch so any prefix scan still in flight
        // from the previous session (a folder expand / "Load more" that hadn't
        // finished) is recognised as stale and drops its result instead of
        // re-injecting keys and resurrecting `incomplete_prefixes` entries.
        self.scan_epoch = self.scan_epoch.wrapping_add(1);
        if keep_rows {
            self.refresh_prefixes = std::mem::take(&mut self.loaded_prefixes);
        } else {
            self.loaded_prefixes.clear();
            self.refresh_prefixes.clear();
        }
        self.scanning_prefixes.clear();
        self.complete_before_refresh.clear();
        self.incomplete_prefixes.clear();
    }

    /// How many keys the *current* scan has accumulated: zero while the
    /// displayed rows are still the previous scan's waiting to be superseded
    /// by the first replacement batch.
    pub(super) fn scanned_len(&self) -> usize {
        if self.keys_superseded_by_next_batch {
            0
        } else {
            self.keys.len()
        }
    }

    /// Add new keys with their types to the key map (deduplicating automatically)
    ///
    /// If any new keys were added, generates a new tree ID to trigger UI refresh
    pub(super) fn extend(&mut self, keys: Vec<(SharedString, SharedString, i64)>) {
        // The first batch of a refresh is what the displayed rows were
        // waiting for: they go now, so a key deleted on the server does not
        // survive the refresh that should have removed it. Emptied here
        // rather than before the scan so the panel never blanks.
        //
        // When a filter (keyword, regex, type, or prefix) narrows the scan and
        // an intermediate page matches zero keys while more cursors remain
        // (`cursors.is_some()`), keep the previous rows on screen until a
        // non-empty batch lands or the scan finishes empty.
        let keep_waiting = keys.is_empty() && self.cursors.is_some();
        let superseded = if keep_waiting {
            false
        } else {
            std::mem::take(&mut self.keys_superseded_by_next_batch)
        };
        if superseded {
            self.keys.clear();
            self.key_ttls = Arc::new(AHashMap::new());
        }
        let key_ttls = Arc::make_mut(&mut self.key_ttls);
        key_ttls.reserve(keys.len());
        let mut insert_count = 0;

        for (key, key_type, ttl_secs) in keys {
            let kt = KeyType::from(key_type.as_ref());
            // One walk down the tree, not the two an `entry` takes (it looks
            // the key up, then inserts): half the cost of a scan page. A
            // known type stays as it was — only an `Unknown` is replaced —
            // so the rare page that knows the key less well puts it back.
            match self.keys.insert(key.clone(), kt) {
                None => insert_count += 1,
                Some(existing) if existing != KeyType::Unknown && existing != kt => {
                    self.keys.insert(key.clone(), existing);
                }
                Some(_) => {}
            }
            // Always update the freshest TTL (it counts down) — replace
            // existing rather than insert_with.
            key_ttls.insert(key, ttl_secs);
        }

        // Update tree ID only if new keys were added — or if a refresh just
        // took the old ones out, which a batch of zero would otherwise leave
        // on screen.
        if insert_count != 0 || superseded {
            self.renew_tree_id();
        }
    }

    /// Take one key out of the loaded set, with its TTL — a deleted key
    /// must not stay behind in the TTL map the tree's filters read.
    pub(super) fn forget_key(&mut self, key: &SharedString) {
        self.keys.remove(key);
        if self.key_ttls.contains_key(key) {
            Arc::make_mut(&mut self.key_ttls).remove(key);
        }
    }

    /// [`Self::forget_key`] for every loaded key `gone` accepts.
    pub(super) fn forget_keys_where(&mut self, gone: impl Fn(&SharedString) -> bool) {
        let doomed: Vec<SharedString> = self.keys.keys().filter(|key| gone(key)).cloned().collect();
        for key in &doomed {
            self.keys.remove(key);
        }
        if self.key_ttls.keys().any(&gone) {
            Arc::make_mut(&mut self.key_ttls).retain(|key, _| !gone(key));
        }
    }

    /// How many keys the tree lists — see [`ZedisServerState::listed_count`].
    pub(super) fn listed_count(&self) -> usize {
        self.listed_keys.unwrap_or(self.keys.len())
    }
}
