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

//! Per-connection MRU list of recently opened Redis keys.
//!
//! Scoped by `(server_id, db)` so switching databases does not mix keys.
//! Cap is intentionally small (10) — the key-tree dropdown shows the
//! whole list without scrolling friction.

use super::RECENT_KEYS_TABLE;
use super::history_manager::HistoryManager;
use std::sync::LazyLock;

/// Max keys kept per connection. Matches the product "5–10 chips" budget.
const RECENT_KEYS_CAP: usize = 10;

static RECENT_KEYS_MANAGER: LazyLock<HistoryManager> =
    LazyLock::new(|| HistoryManager::new(RECENT_KEYS_TABLE).set_max_history_size(RECENT_KEYS_CAP));

pub fn get_recent_keys_manager() -> &'static HistoryManager {
    &RECENT_KEYS_MANAGER
}

/// Storage key for a connection's MRU list (`server_id` + db index).
pub fn recent_keys_scope(server_id: &str, db: usize) -> String {
    format!("{server_id}/{db}")
}
