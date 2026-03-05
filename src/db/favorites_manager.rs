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

use super::FAVORITY_TABLE;
use super::history_manager::HistoryManager;
use std::sync::LazyLock;

static FAVORITES_MANAGER: LazyLock<HistoryManager> = LazyLock::new(|| HistoryManager::new(FAVORITY_TABLE));

pub fn get_favorites_manager() -> &'static HistoryManager {
    &FAVORITES_MANAGER
}
