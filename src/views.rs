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
mod bytes_editor;
mod clients_manager;
mod command_palette;
mod config_editor;
mod content;
mod danger_confirm;
mod editor;
mod function_editor;
mod hash_editor;
mod key_tree;
mod kv_table;
mod list_editor;
mod lua_script_library;
mod memory_analysis;
mod metrics;
mod migration_window;
mod monitor;
mod proto_editor;
mod pubsub_editor;
mod script_editor;
mod search_manager;
mod secondary_window;
mod servers;
mod set_editor;
mod setting_editor;
mod sidebar;
mod slowlog_editor;
mod status_bar;
mod stream_editor;
mod terminal;
mod title_bar;
mod zset_editor;

pub use about::open_about_window;
pub use acl_manager::ZedisAclManager;
pub use bytes_editor::ZedisBytesEditor;
pub use clients_manager::ZedisClientsManager;
pub use command_palette::ZedisCommandPalette;
pub use config_editor::ZedisConfigEditor;
pub use content::ZedisContent;
pub use danger_confirm::confirm_dangerous_command;
pub use editor::ZedisEditor;
pub use function_editor::ZedisFunctionEditor;
pub use hash_editor::ZedisHashEditor;
pub use key_tree::ZedisKeyTree;
pub use kv_table::ZedisKvTable;
pub use list_editor::ZedisListEditor;
pub use lua_script_library::ZedisLuaScriptLibrary;
pub use memory_analysis::ZedisMemoryAnalysis;
// Chart helpers re-exported so other diagnostic panels (e.g.
// memory_analysis) can reuse the metrics view's canvas primitives
// without each one re-implementing axis / tick rendering.
pub use metrics::ZedisMetrics;
pub(crate) use metrics::{ChartParams, format_timestamp_ms, make_line_canvas};
pub use migration_window::{open_migration_export_window, open_migration_import_window};
pub use monitor::ZedisMonitor;
pub use proto_editor::ZedisProtoEditor;
pub use pubsub_editor::ZedisPubsubEditor;
pub use script_editor::ZedisScriptEditor;
pub use search_manager::ZedisSearchManager;
pub use servers::ZedisServers;
pub use set_editor::ZedisSetEditor;
pub use setting_editor::open_settings_window;
pub use sidebar::ZedisSidebar;
pub use slowlog_editor::ZedisSlowlogEditor;
pub use status_bar::ZedisStatusBar;
pub use stream_editor::ZedisStreamEditor;
pub use terminal::ZedisTerminal;
pub use title_bar::ZedisTitleBar;
pub use zset_editor::ZedisZsetEditor;
