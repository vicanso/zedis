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

//! The key open in the editor: its name, its value as loaded, the generation
//! that value belongs to, what the size gate let through or put aside, and
//! the session's history of every key's writes.

use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct OpenKey {
    /// Currently selected key name
    pub(super) key: Option<SharedString>,

    /// Value data for the currently selected key
    pub(super) value: Option<RedisValue>,

    /// Generation of the selected value: bumped when a key is selected, a
    /// value is (re)loaded or replaced, or a collection filter starts over
    /// (`next_value_epoch`). A task that reads or writes the loaded value —
    /// a page of a hash / list / set / zset, a write and its follow-up, a
    /// string save, a TTL change — captures it at launch, and its callback
    /// leaves the value alone when it moved on. Without it one key's fields
    /// were appended to another's table, an `HSCAN` went on over the new key
    /// from the old one's cursor, and a failed string save wrote the old
    /// key's bytes into the new key's editor. The key name cannot tell:
    /// re-selecting the same key is a new generation too.
    pub(super) value_epoch: u64,

    /// Key whose oversized value the user chose to load anyway ("Load
    /// anyway" on the too-large panel). While the selected key matches,
    /// `get_value` skips the size gate so a later refresh doesn't bounce
    /// back to the panel. Cleared on server switch.
    pub(super) size_gate_bypassed: Option<SharedString>,

    /// Key whose oversized String the user chose to preview ("Preview" on
    /// the too-large panel): `get_value` then fetches its first
    /// `VALUE_PREVIEW_BYTES` instead of stopping at the gate, so a refresh
    /// keeps the preview. Cleared on server switch.
    pub(super) size_gate_preview: Option<SharedString>,

    /// What a "load anyway" still in flight replaced, with its key: the
    /// gate panel's value, or a preview. Cancel puts it back as it was —
    /// without asking the server, whose connection is the one the large
    /// reply is still arriving on. Cleared when the load lands.
    pub(super) size_gate_pending: Option<(SharedString, RedisValue)>,

    /// Per-key String versions and collection change logs for this
    /// connection, under one memory budget with least-recently-used
    /// eviction (`history::KeyHistories`). In memory only, never persisted.
    pub(super) histories: KeyHistories,
}

impl OpenKey {
    /// Start a new generation of the selected value — see `value_epoch`.
    pub(super) fn next_epoch(&mut self) {
        self.value_epoch = self.value_epoch.wrapping_add(1);
    }

    /// No key open any more: a new generation, so a task still on its way
    /// for the old one leaves the editor alone.
    pub(super) fn close(&mut self) {
        self.key = None;
        self.value = None;
        self.next_epoch();
    }

    /// What a server switch forgets: the open key, what the size gate held,
    /// and the history — scoped to the session, it would restore one
    /// server's bytes into another's key.
    pub(super) fn forget_server(&mut self) {
        self.histories.clear();
        self.close();
        self.size_gate_bypassed = None;
        self.size_gate_preview = None;
        self.size_gate_pending = None;
    }

    /// The open key and its value, to edit — not while a load is on its way.
    pub(super) fn try_get_mut_key_value(&mut self) -> Option<(SharedString, &mut RedisValue)> {
        let key = self.key.as_ref().filter(|k| !k.is_empty())?.clone();
        let value = self.value.as_mut()?;
        if value.is_loading() {
            return None;
        }
        Some((key, value))
    }
}
