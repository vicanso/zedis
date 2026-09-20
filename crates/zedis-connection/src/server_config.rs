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

//! `CONFIG` and the full `INFO`: what the config editor, the persistence and
//! load panels and the INFO browser read and write.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::server_db::ServerDb;
use redis::cmd;
use std::collections::HashMap;

type Result<T, E = Error> = std::result::Result<T, E>;

/// A server's configuration as the editor opens it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerConfig {
    /// `CONFIG GET *`, sorted by name.
    pub params: Vec<(String, String)>,
    /// `config_file` from `INFO server` — empty when the server was started
    /// without one, in which case `CONFIG REWRITE` has nowhere to write and
    /// an edit cannot be made to survive a restart.
    pub config_file: String,
}

/// `CONFIG GET *` as a map — the form a comparison between two servers wants.
pub async fn config_get_all(at: &ServerDb) -> Result<HashMap<String, String>> {
    Ok(cmd("CONFIG")
        .arg("GET")
        .arg("*")
        .query_async(&mut at.connection().await?)
        .await?)
}

/// Every parameter, sorted, plus the config file's path. Only `CONFIG GET` is
/// fatal; a denied `INFO` just leaves the path empty.
pub async fn config_load(at: &ServerDb) -> Result<ServerConfig> {
    let mut conn = at.connection().await?;
    let map: HashMap<String, String> = cmd("CONFIG").arg("GET").arg("*").query_async(&mut conn).await?;
    let mut params: Vec<(String, String)> = map.into_iter().collect();
    params.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let info: String = cmd("INFO")
        .arg("server")
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    Ok(ServerConfig {
        params,
        config_file: config_file_of(&info),
    })
}

/// `CONFIG SET name value`, on the connected node.
pub async fn config_set(at: &ServerDb, name: &str, value: &str) -> Result<()> {
    Ok(cmd("CONFIG")
        .arg("SET")
        .arg(name)
        .arg(value)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `CONFIG REWRITE` on every master: each node has a config file of its own.
pub async fn config_rewrite(at: &ServerDb) -> Result<()> {
    let rewrite = cmd("CONFIG").arg("REWRITE").clone();
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![rewrite]).await?;
    Ok(())
}

/// `CONFIG GET name`: the value, `None` for a name the server does not know.
/// Unlike [`config_get_named`] a refusal is an error — for a caller that has
/// something to tell the user about it.
pub async fn config_get_one(at: &ServerDb, name: &str) -> Result<Option<String>> {
    let pair: Vec<String> = cmd("CONFIG")
        .arg("GET")
        .arg(name)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(pair.into_iter().nth(1))
}

/// `CONFIG GET name` for each of `names`, one at a time: `None` for a name the
/// server refused (a managed cloud hides some and answers others) or does not
/// know. `Err` only when there is no connection to ask on.
pub async fn config_get_named(at: &ServerDb, names: &[&str]) -> Result<Vec<Option<String>>> {
    let mut conn = at.connection().await?;
    let mut values = Vec::with_capacity(names.len());
    for name in names {
        // `[name, value]`; an unknown name answers an empty array.
        let reply: redis::RedisResult<Vec<String>> = cmd("CONFIG").arg("GET").arg(*name).query_async(&mut conn).await;
        values.push(reply.ok().and_then(|pair| pair.into_iter().nth(1)));
    }
    Ok(values)
}

/// `CONFIG RESETSTAT` — zeroes the counters `INFO stats` / `commandstats`
/// report, on the connected node.
pub async fn config_resetstat(at: &ServerDb) -> Result<()> {
    Ok(cmd("CONFIG")
        .arg("RESETSTAT")
        .query_async(&mut at.connection().await?)
        .await?)
}

/// The fullest `INFO` each master will give, labelled `host:port`:
/// `INFO everything` (7+), then `INFO all`, then plain `INFO`. An unknown
/// section answers with an empty string rather than an error, so "every node
/// came back empty" is the signal to fall back.
pub async fn info_everything(at: &ServerDb) -> Result<Vec<(String, String)>> {
    let client = at.client().await?;
    for section in ["everything", "all", ""] {
        let mut info = cmd("INFO");
        if !section.is_empty() {
            info.arg(section);
        }
        let (servers, replies): (_, Vec<String>) = client.query_async_masters(vec![info]).await?;
        if replies.iter().any(|text| !text.trim().is_empty()) {
            return Ok(servers
                .iter()
                .zip(replies)
                .map(|(server, text)| (format!("{}:{}", server.host, server.port), text))
                .collect());
        }
    }
    Ok(Vec::new())
}

/// The `config_file:` line of `INFO server`, trimmed; empty when absent.
fn config_file_of(info: &str) -> String {
    info.lines()
        .find_map(|line| line.trim().strip_prefix("config_file:"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::config_file_of;

    #[test]
    fn the_config_file_is_read_from_info_server_and_may_be_empty() {
        let info = "# Server\r\nredis_version:7.2.4\r\nconfig_file:/etc/redis/redis.conf\r\nio_threads_active:0\r\n";
        assert_eq!(config_file_of(info), "/etc/redis/redis.conf");
        // Started without a file: the line is there and empty.
        assert_eq!(config_file_of("redis_version:7.2.4\r\nconfig_file:\r\n"), "");
        // A proxy that strips the section.
        assert_eq!(config_file_of(""), "");
    }
}
