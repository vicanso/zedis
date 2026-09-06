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

mod about;
mod acl_manager;
mod bitmap_editor;
mod bytes_editor;
mod client_dialogs;
mod clients_manager;
mod command_palette;
mod config_doc;
mod config_editor;
mod connection_diagnostics;
mod content;
mod copy_key_dialog;
mod danger_confirm;
mod editor;
mod export;
mod export_servers_dialog;
mod features_dialog;
mod function_editor;
mod geo_map;
mod hash_editor;
mod hll_editor;
mod hotkeys;
mod jsonpath_completion;
mod key_tag_dialog;
mod key_tree;
mod keyspace_notifications;
mod kv_table;
mod list_editor;
mod lua_script_library;
mod memory_analysis;
mod metrics;
mod migration_window;
mod monitor;
mod multi_search;
mod persistence;
mod probabilistic_editor;
mod proto_editor;
mod pubsub_channels_dialog;
mod pubsub_editor;
mod recent_keys_palette;
mod script_editor;
mod search_manager;
mod secondary_window;
mod sentinel_dialogs;
mod server_info;
mod server_load;
mod servers;
mod set_editor;
mod setting_editor;
mod shortcuts_overlay;
mod sidebar;
mod slowlog_editor;
mod status_bar;
mod stream_editor;
mod terminal;
mod timeseries_editor;
mod title_bar;
mod topology;
mod trash_dialog;
mod unsupported_panel;
mod update_dialog;
mod value_diff;
mod value_search;
mod vector_set_editor;
mod zset_editor;

pub use about::open_about_window;
pub use acl_manager::ZedisAclManager;
pub use bitmap_editor::{BitmapEvent, ZedisBitmapEditor};
pub(crate) use bitmap_editor::{bitmap_eligible, looks_like_bitmap};
pub use bytes_editor::ZedisBytesEditor;
pub use client_dialogs::{KillFilterSupport, ZedisClientKillFilterDialog, ZedisClientPauseDialog};
pub use clients_manager::ZedisClientsManager;
pub use command_palette::ZedisCommandPalette;
pub use config_editor::ZedisConfigEditor;
pub use connection_diagnostics::open_connection_diagnostics;
pub use content::ZedisContent;
pub use copy_key_dialog::ZedisCopyKeyDialog;
pub use danger_confirm::confirm_dangerous_command;
pub use editor::ZedisEditor;
pub(crate) use export::{export_filename, export_to_file, export_to_file_global};
pub use export_servers_dialog::ZedisExportServersDialog;
pub use function_editor::ZedisFunctionEditor;
pub(crate) use geo_map::zset_looks_geo;
pub use geo_map::{GeoMapEvent, ZedisGeoMap};
pub use hash_editor::ZedisHashEditor;
pub use hll_editor::ZedisHllEditor;
pub(crate) use hll_editor::looks_like_hll;
pub use hotkeys::ZedisHotkeys;
pub use key_tag_dialog::{OnTagDialogDone, open_batch_key_tag_dialog, open_key_tag_dialog};
pub use key_tree::ZedisKeyTree;
pub use keyspace_notifications::ZedisKeyspaceNotifications;
pub use kv_table::ZedisKvTable;
pub use list_editor::ZedisListEditor;
pub use lua_script_library::ZedisLuaScriptLibrary;
pub use memory_analysis::ZedisMemoryAnalysis;
pub use sentinel_dialogs::{ZedisSentinelMonitorDialog, ZedisSentinelSetDialog};
// Chart helpers re-exported so other diagnostic panels (e.g.
// memory_analysis) can reuse the metrics view's canvas primitives
// without each one re-implementing axis / tick rendering.
pub use features_dialog::open_features_dialog;
pub use metrics::ZedisMetrics;
pub(crate) use metrics::{ChartParams, format_timestamp_ms, make_bar_canvas, make_line_canvas};
pub(crate) use migration_window::dirs_default_directory;
pub use migration_window::{ExportSource, open_migration_export_window, open_migration_import_window};
pub use monitor::ZedisMonitor;
pub use multi_search::ZedisMultiSearch;
pub use persistence::ZedisPersistence;
pub use probabilistic_editor::ZedisProbabilisticEditor;
pub use proto_editor::ZedisProtoEditor;
pub use pubsub_channels_dialog::{ChannelPick, open_pubsub_channels_dialog};
pub use pubsub_editor::ZedisPubsubEditor;
pub use recent_keys_palette::ZedisRecentKeysPalette;
pub use script_editor::ZedisScriptEditor;
pub use search_manager::ZedisSearchManager;
pub use server_info::ZedisServerInfo;
pub use server_load::ZedisServerLoad;
pub use servers::ZedisServers;
pub use set_editor::ZedisSetEditor;
pub use setting_editor::open_settings_window;
pub use shortcuts_overlay::ZedisShortcutsOverlay;
pub use sidebar::ZedisSidebar;
pub use slowlog_editor::ZedisSlowlogEditor;
pub use status_bar::ZedisStatusBar;
pub use stream_editor::ZedisStreamEditor;
pub use terminal::ZedisTerminal;
pub use timeseries_editor::ZedisTimeSeriesEditor;
pub use title_bar::ZedisTitleBar;
pub use topology::ZedisTopology;
pub use trash_dialog::open_trash_dialog;
pub use unsupported_panel::ZedisUnsupportedPanel;
pub use update_dialog::{DialogCallback, ZedisUpdateDialog};
pub use value_diff::{DiffCloseCallback, ZedisValueDiff};
pub use value_search::ZedisValueSearch;
pub use vector_set_editor::ZedisVectorSetEditor;
pub use zset_editor::ZedisZsetEditor;

use crate::connection::{CommandStatus, ServerCommand};
use crate::states::{ServerView, ZedisGlobalStore, ZedisServerState, i18n_features};
use gpui::{App, Entity, IntoElement, ParentElement, SharedString, Styled};
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, label::Label};
use rust_i18n::t;

/// The per-section "CONFIG SET unavailable — denied for this user (NOPERM)"
/// chip panels show in place of a button or sub-view the server can't back,
/// so a reduced panel explains itself instead of toasting.
pub fn unavailable_chip(cx: &App, command: ServerCommand, status: CommandStatus) -> impl IntoElement {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let reason = i18n_features(cx, status.i18n_key());
    let text: SharedString = t!(
        "features.section_unavailable",
        command = command.label(),
        reason = reason,
        locale = &locale
    )
    .to_string()
    .into();
    let warning = cx.theme().warning;
    h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(warning)
        .child(Icon::new(IconName::TriangleAlert).text_xs().text_color(warning))
        .child(Label::new(text).text_xs().text_color(warning))
}

/// Shared "jump to key" used by observability views (Monitor, Keyspace
/// notifications, Memory Analyzer, Value Search): select the key on the
/// active connection and switch to the editor view.
pub fn open_key_in_editor(server_state: &Entity<ZedisServerState>, key: SharedString, cx: &mut App) {
    server_state.update(cx, |state, cx| state.select_key(key, cx));
    cx.global::<ZedisGlobalStore>()
        .clone()
        .update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
}

/// Shared "search in key tree": start a keyword scan (contains-match) on the
/// active connection and switch to the editor view. The key tree mirrors the
/// keyword into its search box via the `KeyScanStarted` event.
pub fn search_keys_in_tree(server_state: &Entity<ZedisServerState>, keyword: SharedString, cx: &mut App) {
    server_state.update(cx, |state, cx| state.scan(keyword, cx));
    cx.global::<ZedisGlobalStore>()
        .clone()
        .update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
}
