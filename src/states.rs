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

mod app;
mod i18n;
mod migration;
mod server;
mod session;

pub use crate::error::ConnectionErrorKind;
pub use app::*;
pub use i18n::i18n_acl;
pub use i18n::i18n_bitmap;
pub use i18n::i18n_clients_manager;
pub use i18n::i18n_command_palette;
pub use i18n::i18n_common;
pub use i18n::i18n_config_editor;
pub use i18n::i18n_copy;
pub use i18n::i18n_editor;
pub use i18n::i18n_functions;
pub use i18n::i18n_geo_map;
pub use i18n::i18n_hash_editor;
pub use i18n::i18n_hll;
pub use i18n::i18n_key_tag;
pub use i18n::i18n_key_tree;
pub use i18n::i18n_keyspace_notifications;
pub use i18n::i18n_kv_table;
pub use i18n::i18n_list_editor;
pub use i18n::i18n_lua_scripts;
pub use i18n::i18n_memory_analysis;
pub use i18n::i18n_metrics;
pub use i18n::i18n_migration;
pub use i18n::i18n_monitor;
pub use i18n::i18n_persistence;
pub use i18n::i18n_probabilistic;
pub use i18n::i18n_proto_editor;
pub use i18n::i18n_pubsub_editor;
pub use i18n::i18n_script_editor;
pub use i18n::i18n_search;
pub use i18n::i18n_server_load;
pub use i18n::i18n_servers;
pub use i18n::i18n_set_editor;
pub use i18n::i18n_settings;
pub use i18n::i18n_shortcuts;
pub use i18n::i18n_sidebar;
pub use i18n::i18n_slowlog_editor;
pub use i18n::i18n_status_bar;
pub use i18n::i18n_stream_editor;
pub use i18n::i18n_timeseries;
pub use i18n::i18n_topology;
pub use i18n::i18n_tray;
pub use i18n::i18n_update;
pub use i18n::i18n_value_search;
pub use i18n::i18n_vector_set;
pub use i18n::i18n_zset_editor;
pub use migration::{LogStatus, MigrationEvent, MigrationJob, MigrationPhase, MigrationState};
pub use server::ConnectionHealth;
pub use server::ErrorMessage;
pub use server::ZedisServerState;
pub use server::event::ServerEvent;
pub use server::event::ServerTask;
// Used by the value-diff view to render the same RFC 7396 merge patch
// document the Save path sends as JSON.MERGE — single source of truth.
pub use server::stat::{RedisMetrics, ReplicaInfo, get_metrics_cache, load_persisted_metrics};
pub(crate) use server::stream::tail_read;
pub use server::string::detect_and_decode;
pub(crate) use server::value::json_merge_diff;
pub use server::value::*;
pub use session::*;
