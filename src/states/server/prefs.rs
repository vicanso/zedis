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

//! How this connection's key tree and editor are drawn: the entry's own
//! settings where it has them, else the app's, resolved when it is selected.

use super::*;
use crate::connection::RedisServer;
use crate::states::ZedisAppState;

#[derive(Debug, Clone, Default)]
pub(super) struct ConnectionPrefs {
    /// Key-tree separator for this connection (from server config, else `:`).
    /// Set on `select`.
    key_separator: String,

    /// Resolved SCAN page size for this connection.
    key_scan_count: usize,

    /// Resolved max key-tree depth for this connection.
    max_key_tree_depth: usize,

    /// Resolved auto-expand threshold for this connection.
    auto_expand_threshold: usize,

    /// Resolved "show TTL in key tree" for this connection.
    show_key_tree_ttl: bool,

    /// Whether to soft wrap the editor
    soft_wrap: bool,
}

impl ConnectionPrefs {
    /// The tree's settings for `server`: its own where it has them, else
    /// `settings`'. An entry not in the list goes by `settings` alone.
    pub(super) fn resolve_tree(&mut self, server: Option<&RedisServer>, settings: &ZedisAppState) {
        let g_scan = settings.key_scan_count_setting();
        let g_depth = settings.max_key_tree_depth();
        let g_expand = settings.auto_expand_threshold();
        let g_ttl = settings.show_key_tree_ttl();
        if let Some(server) = server {
            self.key_separator = server.resolve_key_separator();
            self.key_scan_count = server.resolve_key_scan_count(g_scan);
            self.max_key_tree_depth = server.resolve_max_key_tree_depth(g_depth);
            self.auto_expand_threshold = server.resolve_auto_expand_threshold(g_expand);
            self.show_key_tree_ttl = server.resolve_show_key_tree_ttl(g_ttl);
        } else {
            self.key_separator = ":".to_string();
            self.key_scan_count = g_scan
                .unwrap_or(DEFAULT_KEY_SCAN_COUNT)
                .clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT);
            self.max_key_tree_depth = g_depth.max(1);
            self.auto_expand_threshold = g_expand;
            self.show_key_tree_ttl = g_ttl;
        }
    }

    /// Never empty — defaults to `":"`.
    pub(super) fn key_separator(&self) -> &str {
        if self.key_separator.is_empty() {
            ":"
        } else {
            &self.key_separator
        }
    }

    /// Never below the floor.
    pub(super) fn key_scan_count(&self) -> usize {
        self.key_scan_count.clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT)
    }

    pub(super) fn max_key_tree_depth(&self) -> usize {
        if self.max_key_tree_depth == 0 {
            5
        } else {
            self.max_key_tree_depth
        }
    }

    pub(super) fn auto_expand_threshold(&self) -> usize {
        self.auto_expand_threshold
    }

    pub(super) fn show_key_tree_ttl(&self) -> bool {
        self.show_key_tree_ttl
    }

    pub(super) fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    pub(super) fn set_soft_wrap(&mut self, soft_wrap: bool) {
        self.soft_wrap = soft_wrap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry's own tree settings win over the app's; what it leaves
    /// unset is the app's.
    #[test]
    fn an_entry_s_own_tree_settings_win_over_the_apps() {
        let mut settings = ZedisAppState::default();
        settings.set_key_scan_count(2_500);
        settings.set_max_key_tree_depth(7);
        settings.set_auto_expand_threshold(40);
        settings.set_show_key_tree_ttl(false);
        let server = RedisServer {
            key_separator: Some("/".to_string()),
            max_key_tree_depth: Some(3),
            show_key_tree_ttl: Some(true),
            ..Default::default()
        };
        let mut prefs = ConnectionPrefs::default();
        prefs.resolve_tree(Some(&server), &settings);
        assert_eq!(prefs.key_separator(), "/");
        assert_eq!(prefs.max_key_tree_depth(), 3);
        assert!(prefs.show_key_tree_ttl());
        assert_eq!(prefs.key_scan_count(), 2_500);
        assert_eq!(prefs.auto_expand_threshold(), 40);
    }
}
