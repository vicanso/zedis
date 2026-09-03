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

//! Redis connection layer for Zedis: pooled clients, server config,
//! SSH tunnels, and command helpers. GUI-free — the app re-exports this
//! crate through `crate::connection` / `crate::helpers` / `crate::error`.

use tracing::info;

pub mod error;
pub mod floors;
pub mod string;
pub mod time;

mod acl;
mod async_connection;
mod command;
mod config;
mod danger;
mod diagnostics;
mod dump_restore;
mod functions;
mod hash_fields;
mod hotkeys;
mod import_clients;
mod latency;
mod lua_script;
mod manager;
mod master_key;
mod multi_search;
mod probe;
mod readable_export;
mod readable_import;
mod search;
mod slot_stats;
mod ssh_cluster_connection;
mod ssh_stream;
mod ssh_tunnel;

pub use acl::{AclSelector, AclUser, acl_del_user, acl_get_user, acl_list, acl_set_user, acl_whoami, split_acl_rules};
pub use async_connection::{
    RedisAsyncConn, client_name, open_monitor_connection, open_node_connection, open_node_connection_cached,
    open_seed_connection, open_single_connection, set_redis_connection_timeout, set_redis_response_timeout,
};
pub use config::{
    ImportError, RedisServer, SERVER_TYPE_AUTO, SERVER_TYPE_CLUSTER, SERVER_TYPE_SENTINEL, SERVER_TYPE_STANDALONE,
    TAG_ENV_LABELS, get_server, get_server_groups, get_servers, save_servers, servers_toml_redacted, tag_color_index,
};
pub use danger::{
    ConfirmStrictness, DangerKind, classify_dangerous_line, confirm_strictness, is_write_command,
    requires_write_confirm,
};
pub use diagnostics::{
    DiagHint, DiagOutcome, DiagStage, DiagStatus, diag_stages, diag_timeout, dial_endpoint, probe_dns, probe_redis,
    probe_ssh_auth, probe_ssh_tunnel, probe_tcp,
};
pub use dump_restore::{
    ConflictMode, ConflictPreview, DumpEntry, DumpHeader, DumpReader, DumpWriter, RestoreStatus, copy_key,
    dump_keys_chunk, preview_dump_conflicts, restore_keys_chunk,
};
pub use functions::{
    FunctionLibrary, FunctionMeta, FunctionRestorePolicy, FunctionStats, LibraryValidateError, LibraryValidation,
    function_delete, function_dump, function_fcall, function_flush, function_list, function_load, function_restore,
    function_stats, validate_library_source,
};
pub use hash_fields::{FieldTtl, rename_hash_field, write_hash_field};
pub use hotkeys::{HotkeyEntry, HotkeysReport};
pub use latency::{
    LatencyEvent, LatencySample, latency_history, latency_latest, latency_monitor_threshold, latency_reset,
};
pub use lua_script::{ScriptRunOutcome, max_keys_index, run_script, script_exists, script_flush, script_load};
pub use multi_search::{MultiSearchHit, MultiSearchServerResult, multi_search_exact, multi_search_scan};
pub use probe::{get_server_features, invalidate_server_features, note_server_command_error, probe_server_features};
pub use readable_export::{
    ReadLimits, ReadableEntry, ReadableValue, csv_header, entry_to_csv, entry_to_json, next_stream_id,
    read_readable_chunk,
};
pub use readable_import::{
    ImportFormat, ReadableWriteStatus, detect_import_format, parse_readable_entries, preview_import_conflicts,
    sniff_import_format, write_readable_chunk,
};
pub use slot_stats::{SlotStatMetric, SlotStatRow};
pub use ssh_tunnel::{HostKeyApprover, HostKeyDecision, HostKeyPrompt, set_host_key_approver};

pub use manager::{
    AccessMode, CLUSTER_HASH_SLOTS, ClusterSlotMap, CommandStat, ExpireCondition, HeatMetric, HeatProbe,
    KeyMemoryUsage, MatchLocation, RedisClientDescription, ShardedPubSub, SlowLogEntry, ValueMatch, ValueSearchRound,
    get_connection_manager, plan_reshard_slots,
};
pub use search::{
    AggregateOptions, AggregateResult, CreateFieldSpec, CreateIndexOptions, FieldKind, FieldSchema, IndexInfo,
    ReducerFn, ReducerSpec, SearchOptions, SearchResult, ft_aggregate, ft_alter_add, ft_create, ft_dropindex,
    ft_explain, ft_info, ft_list, ft_profile, ft_search,
};
// The capability matrix is pure logic and lives in `zedis-core`; re-exported
// here so call sites keep using `crate::connection::Capability` unchanged.
pub use zedis_core::capability::Capability;
pub use zedis_core::features::{CommandStatus, ServerCommand, ServerFeatures, ServerFlavor};
pub fn clear_expired_cache() {
    let (removed_count, total_count) = async_connection::clear_expired_connection_pool();
    if removed_count > 0 {
        info!(removed_count, total_count, "clear expired redis connection")
    }

    let (removed_count, total_count) = manager::clear_expired_clients();
    if removed_count > 0 {
        info!(removed_count, total_count, "clear expired redis client")
    }

    let (removed_count, total_count) = ssh_tunnel::clear_expired_ssh_sessions();
    if removed_count > 0 {
        info!(removed_count, total_count, "clear expired ssh session")
    }
}
pub use command::*;
