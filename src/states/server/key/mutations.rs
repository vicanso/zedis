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

//! Key-level mutations on [`ZedisServerState`]: create, rename, delete, flush,
//! TTL/EXPIREAT/PERSIST updates, batch TTL runs, and recycle-bin restore sync.

use super::*;
use crate::connection::{
    ExpireCondition, create_key, delete_key, delete_keys, delete_keys_matching, expire_key, expire_key_at, flush_all,
    flush_db, key_types, persist_key, rename_key, set_hash_fields_ttl, set_keys_ttl, set_ttl_matching,
};
use crate::db::move_local_records;
use crate::helpers::{format_ttl_chip, parse_duration};
use crate::states::{i18n_key_tree, i18n_status_bar};
use rust_i18n::t;
use std::time::Duration;

impl ZedisServerState {
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
        let prefix = self.folder_prefix(&folder);
        let pattern = self.folder_pattern(&folder);
        self.delete_matching_with_prefix(pattern, Some(prefix), cx);
    }

    /// Delete every key matching an arbitrary `SCAN MATCH` glob `pattern`
    /// across all masters (bounded like `delete_folder`).
    pub fn delete_keys_by_pattern(&mut self, pattern: SharedString, cx: &mut Context<Self>) {
        self.delete_matching_with_prefix(pattern.to_string(), None, cx);
    }

    fn delete_matching_with_prefix(&mut self, pattern: String, prefix: Option<String>, cx: &mut Context<Self>) {
        let at = self.at_confirmed();
        let label = prefix.clone().unwrap_or_else(|| pattern.clone());
        self.spawn_with_arg(
            ServerTask::DeleteKeys,
            label,
            move || async move { Ok(delete_keys_matching(&at, &pattern).await?) },
            move |this, result, cx| {
                match result {
                    Ok(true) => {
                        if let Some(prefix) = prefix.as_ref() {
                            this.forget_deleted_keys(DeletedKeys::Prefix(prefix.clone()), cx);
                        } else {
                            this.open.histories.clear();
                            this.refresh_dbsize_now(cx);
                            this.refresh_prefix(SharedString::default(), cx);
                        }
                    }
                    // The walk stopped at its round limit: some keys may
                    // still be there. Scan again rather than drop every
                    // loaded key as deleted.
                    Ok(false) => {
                        if let Some(prefix) = prefix.as_ref() {
                            this.open.histories.retain_keys(|key| !key.starts_with(prefix.as_str()));
                            this.refresh_prefix(prefix.clone().into(), cx);
                            if let Some(key) = this.open.key.clone().filter(|key| key.starts_with(prefix.as_str())) {
                                this.reload_value(key, cx);
                            }
                        } else {
                            this.refresh_prefix(SharedString::default(), cx);
                            if let Some(key) = this.open.key.clone() {
                                this.reload_value(key, cx);
                            }
                        }
                        this.refresh_dbsize_now(cx);
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
    pub(super) fn sync_tree_ttl(&mut self, key: &SharedString, ttl_secs: i64) -> bool {
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

    pub(super) fn apply_tree_ttl(&mut self, key: &SharedString, ttl_secs: i64, cx: &mut Context<Self>) {
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
        let prefix = self.folder_prefix(&folder);
        let pattern = self.folder_pattern(&folder);
        self.batch_set_ttl_matching_inner(prefix, pattern, ttl_secs, condition, cx);
    }

    /// Batch-apply a TTL (or remove it when `ttl_secs` is `None`) to every
    /// key matching an arbitrary `SCAN MATCH` glob `pattern`.
    pub fn batch_set_ttl_by_pattern(
        &mut self,
        pattern: SharedString,
        ttl_secs: Option<u64>,
        condition: Option<ExpireCondition>,
        cx: &mut Context<Self>,
    ) {
        let pattern = pattern.to_string();
        self.batch_set_ttl_matching_inner(pattern.clone(), pattern, ttl_secs, condition, cx);
    }

    fn batch_set_ttl_matching_inner(
        &mut self,
        label: String,
        pattern: String,
        ttl_secs: Option<u64>,
        condition: Option<ExpireCondition>,
        cx: &mut Context<Self>,
    ) {
        let at = self.at();
        self.spawn_with_arg(
            ServerTask::UpdateKeyTtl,
            label,
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
    pub(super) fn restored_here(&self, server_id: &str, restored: &[RestoredKey]) -> Vec<RestoredKey> {
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
        self.add_key_with_field_ttl(category, key, ttl, args, None, cx);
    }

    pub fn add_key_with_field_ttl(
        &mut self,
        category: SharedString,
        key: SharedString,
        ttl: SharedString,
        args: Vec<SharedString>,
        field_ttl_secs: Option<u64>,
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
                if key_type == KeyType::Hash
                    && let Some(secs) = field_ttl_secs.filter(|s| *s > 0)
                {
                    let field_names: Vec<&[u8]> = args.iter().step_by(2).map(|f| f.as_bytes()).collect();
                    set_hash_fields_ttl(&at, key.as_str(), &field_names, secs).await?;
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
