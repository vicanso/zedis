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

use tracing::info;

mod acl;
mod async_connection;
mod command;
mod config;
mod danger;
mod dump_restore;
mod functions;
mod latency;
mod lua_script;
mod manager;
mod search;
mod ssh_cluster_connection;
mod ssh_stream;
mod ssh_tunnel;

pub use acl::{AclUser, acl_del_user, acl_get_user, acl_list, acl_set_user, acl_whoami};
pub use async_connection::{
    RedisAsyncConn, open_monitor_connection, open_single_connection, set_redis_connection_timeout,
    set_redis_response_timeout,
};
pub use config::{RedisServer, get_server, get_server_groups, get_servers, save_servers, tag_color_index};
pub use danger::{
    ConfirmStrictness, DangerKind, classify_dangerous_line, confirm_strictness, is_write_command,
    requires_write_confirm,
};
pub use dump_restore::{
    ConflictMode, DumpEntry, DumpHeader, DumpReader, DumpWriter, RestoreStatus, dump_keys_chunk, restore_keys_chunk,
};
pub use functions::{FunctionLibrary, function_delete, function_list, function_load};
pub use latency::{
    LatencyEvent, LatencySample, latency_history, latency_latest, latency_monitor_threshold, latency_reset,
};
pub use lua_script::{ScriptRunOutcome, run_script};
pub use manager::{
    AccessMode, HeatMetric, HeatProbe, KeyMemoryUsage, RedisClientDescription, SlowLogEntry, get_connection_manager,
};
pub use search::{
    AggregateOptions, AggregateResult, CreateFieldSpec, CreateIndexOptions, FieldKind, FieldSchema, IndexInfo,
    ReducerFn, ReducerSpec, SearchOptions, SearchResult, ft_aggregate, ft_alter_add, ft_create, ft_dropindex, ft_info,
    ft_list, ft_search,
};
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
