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

mod async_connection;
mod command;
mod config;
mod manager;
mod ssh_cluster_connection;
mod ssh_stream;
mod ssh_tunnel;

pub use async_connection::{RedisAsyncConn, set_redis_connection_timeout, set_redis_response_timeout};
pub use config::{RedisServer, get_servers, save_servers};
pub use manager::{AccessMode, RedisClientDescription, SlowLogEntry, get_connection_manager};
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
