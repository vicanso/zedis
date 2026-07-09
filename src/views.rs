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
mod clients_manager;
mod command_palette;
mod config_editor;
mod connection_diagnostics;
mod content;
mod copy_key_dialog;
mod danger_confirm;
mod editor;
mod export;
mod export_servers_dialog;
mod function_editor;
mod geo_map;
mod hash_editor;
mod hll_editor;
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
mod persistence;
mod probabilistic_editor;
mod proto_editor;
mod pubsub_editor;
mod recent_keys_palette;
mod script_editor;
mod search_manager;
mod secondary_window;
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
mod value_diff;
mod value_search;
mod vector_set_editor;
mod zset_editor;

pub use about::open_about_window;
pub use acl_manager::ZedisAclManager;
pub use bitmap_editor::{BitmapEvent, ZedisBitmapEditor};
pub(crate) use bitmap_editor::{bitmap_eligible, looks_like_bitmap};
pub use bytes_editor::ZedisBytesEditor;
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
pub use key_tag_dialog::{OnTagDialogDone, open_key_tag_dialog};
pub use key_tree::ZedisKeyTree;
pub use keyspace_notifications::ZedisKeyspaceNotifications;
pub use kv_table::ZedisKvTable;
pub use list_editor::ZedisListEditor;
pub use lua_script_library::ZedisLuaScriptLibrary;
pub use memory_analysis::ZedisMemoryAnalysis;
// Chart helpers re-exported so other diagnostic panels (e.g.
// memory_analysis) can reuse the metrics view's canvas primitives
// without each one re-implementing axis / tick rendering.
pub use metrics::ZedisMetrics;
pub(crate) use metrics::{ChartParams, format_timestamp_ms, make_bar_canvas, make_line_canvas};
pub(crate) use migration_window::dirs_default_directory;
pub use migration_window::{open_migration_export_window, open_migration_import_window};
pub use monitor::ZedisMonitor;
pub use persistence::ZedisPersistence;
pub use probabilistic_editor::ZedisProbabilisticEditor;
pub use proto_editor::ZedisProtoEditor;
pub use pubsub_editor::ZedisPubsubEditor;
pub use recent_keys_palette::ZedisRecentKeysPalette;
pub use script_editor::ZedisScriptEditor;
pub use search_manager::ZedisSearchManager;
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
pub use value_diff::{DiffCloseCallback, ZedisValueDiff};
pub use value_search::ZedisValueSearch;
pub use vector_set_editor::ZedisVectorSetEditor;
pub use zset_editor::ZedisZsetEditor;
