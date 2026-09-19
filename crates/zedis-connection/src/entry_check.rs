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

//! Checks the server form makes against an entry that is still being edited —
//! so they take the [`RedisServer`] itself, not a [`ServerDb`](crate::ServerDb):
//! there is no saved entry to name yet.

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::config::RedisServer;
use crate::error::Error;
use crate::open_single_connection;
use redis::cmd;
use std::collections::HashMap;

type Result<T, E = Error> = std::result::Result<T, E>;

/// The names of the masters a sentinel monitors, for the form's "find the
/// master name" button. A sentinel commonly has no password while the data
/// nodes do, and the form has only one password field filled in at this
/// point — so a refused login is retried without one.
pub async fn sentinel_master_names(sentinel: &RedisServer) -> Result<Vec<String>> {
    let mut conn = match open_single_connection(sentinel, 0, false).await {
        Ok(conn) => conn,
        Err(e) => {
            if !e.to_string().contains("AuthenticationFailed") {
                return Err(e);
            }
            let mut anonymous = sentinel.clone();
            anonymous.password = None;
            open_single_connection(&anonymous, 0, false).await?
        }
    };
    let masters: Vec<HashMap<String, String>> = cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
    Ok(masters
        .into_iter()
        .filter_map(|master| master.get("name").cloned())
        .collect())
}

/// The form's Test button. The same dial as discovery — the sentinel's own
/// credentials when it has some, else the legacy retry without a password —
/// and for a Sentinel entry the test is only passed by reaching the *master*
/// the sentinel names: a sentinel that answers proves nothing about the data
/// node the user is about to work on.
#[cfg(not(target_family = "wasm"))]
pub async fn test_connection(server: &RedisServer) -> Result<()> {
    use crate::async_connection::open_seed_connection;
    use crate::config::SERVER_TYPE_SENTINEL;

    let invalid = |message: &str| Error::Invalid {
        message: message.to_string(),
    };
    let mut conn = open_seed_connection(server).await?;
    if server.server_type != Some(SERVER_TYPE_SENTINEL) {
        let _: () = cmd("PING").query_async(&mut conn).await?;
        return Ok(());
    }
    let masters: Vec<HashMap<String, String>> = cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
    let master = masters
        .into_iter()
        .next()
        .ok_or_else(|| invalid("no master found in sentinel"))?;
    let ip = master.get("ip").ok_or_else(|| invalid("master ip not found"))?;
    let port: u16 = master
        .get("port")
        .ok_or_else(|| invalid("master port not found"))?
        .parse()
        .map_err(|e| Error::Invalid {
            message: format!("invalid master port: {e}"),
        })?;
    let mut master_server = server.clone();
    master_server.host = ip.clone();
    master_server.port = port;
    let mut master_conn = open_single_connection(&master_server, 0, false).await?;
    let _: () = cmd("PING").query_async(&mut master_conn).await?;
    Ok(())
}
