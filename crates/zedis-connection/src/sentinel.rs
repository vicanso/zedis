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

//! Sentinel administration — the `SENTINEL …` commands, which only a
//! sentinel answers.
//!
//! The pooled client of a Sentinel entry is a connection to the *data
//! master* the sentinels announced; `SENTINEL` is an unknown command
//! there. So every function here dials the sentinels themselves: the
//! seeds the entry lists (`host:port, host:port`) and, when a master name
//! is known, the peers those sentinels report for it (`SENTINEL
//! SENTINELS`), so a change reaches the whole quorum even when the entry
//! names one sentinel. Sentinel configuration is per process — `MONITOR`,
//! `SET`, `REMOVE`, `RESET` and `FLUSHCONFIG` are therefore broadcast and
//! answered per sentinel; `FAILOVER` goes to one sentinel, which runs it
//! for the quorum (the rest would only answer `INPROG`); `CKQUORUM` is
//! asked of each, since each has its own view of who is reachable.

use crate::async_connection::open_seed_endpoint;
use crate::config::RedisServer;
use crate::error::Error;
use futures::future::join_all;
use redis::aio::MultiplexedConnection;
use redis::cmd;
use std::collections::HashMap;
use std::str::FromStr;
use zedis_core::string::format_host_port;

type Result<T, E = Error> = std::result::Result<T, E>;

/// One monitored master as a sentinel describes it (`SENTINEL MASTERS`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SentinelMaster {
    pub name: String,
    pub ip: String,
    pub port: u16,
    /// `master`, or with `s_down` / `o_down` / `failover_in_progress` …
    pub flags: String,
    pub quorum: u32,
    pub num_replicas: u32,
    pub num_other_sentinels: u32,
    pub down_after_ms: u64,
    pub failover_timeout_ms: u64,
    pub parallel_syncs: u32,
}

impl SentinelMaster {
    /// Whether the sentinel currently sees this master as down.
    pub fn is_down(&self) -> bool {
        self.flags.split(',').any(|f| f == "s_down" || f == "o_down")
    }
}

/// What one sentinel answered to a broadcast command.
#[derive(Debug)]
pub struct SentinelReply {
    /// `host:port` of the sentinel.
    pub sentinel: String,
    pub result: Result<String>,
}

/// The tunable options `SENTINEL SET` accepts — the ones the panel edits.
pub const SENTINEL_SET_OPTIONS: [&str; 5] = [
    "quorum",
    "down-after-milliseconds",
    "failover-timeout",
    "parallel-syncs",
    "auth-pass",
];

/// The monitored masters, from the first reachable sentinel.
pub async fn sentinel_masters(server: &RedisServer) -> Result<Vec<SentinelMaster>> {
    let mut conn = first_sentinel(server).await?;
    let reply: Vec<HashMap<String, String>> = cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
    let mut masters: Vec<SentinelMaster> = reply.iter().map(parse_master).collect();
    masters.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(masters)
}

/// `SENTINEL FAILOVER name` on one sentinel — the quorum runs it.
pub async fn sentinel_failover(server: &RedisServer, master_name: &str) -> Result<String> {
    let mut conn = first_sentinel(server).await?;
    Ok(cmd("SENTINEL")
        .arg("FAILOVER")
        .arg(master_name)
        .query_async(&mut conn)
        .await?)
}

/// `SENTINEL CKQUORUM name` on every sentinel: each reports whether, from
/// where it stands, enough sentinels are reachable to fail over.
pub async fn sentinel_ckquorum(server: &RedisServer, master_name: &str) -> Result<Vec<SentinelReply>> {
    broadcast(server, Some(master_name), &["CKQUORUM", master_name]).await
}

/// `SENTINEL MONITOR name ip port quorum` on every sentinel.
pub async fn sentinel_monitor(
    server: &RedisServer,
    master_name: &str,
    ip: &str,
    port: u16,
    quorum: u32,
) -> Result<Vec<SentinelReply>> {
    let port = port.to_string();
    let quorum = quorum.to_string();
    broadcast(server, None, &["MONITOR", master_name, ip, &port, &quorum]).await
}

/// `SENTINEL SET name option value` on every sentinel, one command per
/// option (every sentinel version takes that form). The first option a
/// sentinel rejects ends that sentinel's run; its reply says which.
pub async fn sentinel_set(
    server: &RedisServer,
    master_name: &str,
    options: &[(String, String)],
) -> Result<Vec<SentinelReply>> {
    let endpoints = sentinel_endpoints(server, Some(master_name)).await?;
    let runs = endpoints.iter().map(|endpoint| async move {
        let sentinel = format_host_port(&endpoint.host, endpoint.port);
        let result = async {
            let mut conn = open_seed_endpoint(endpoint).await?;
            for (option, value) in options {
                let _: String = cmd("SENTINEL")
                    .arg("SET")
                    .arg(master_name)
                    .arg(option)
                    .arg(value)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Invalid {
                        message: format!("{option}: {e}"),
                    })?;
            }
            Ok(format!("{} options set", options.len()))
        }
        .await;
        SentinelReply { sentinel, result }
    });
    Ok(join_all(runs).await)
}

/// `SENTINEL REMOVE name` on every sentinel.
pub async fn sentinel_remove(server: &RedisServer, master_name: &str) -> Result<Vec<SentinelReply>> {
    broadcast(server, Some(master_name), &["REMOVE", master_name]).await
}

/// `SENTINEL RESET pattern` on every sentinel.
pub async fn sentinel_reset(server: &RedisServer, pattern: &str) -> Result<Vec<SentinelReply>> {
    broadcast(server, None, &["RESET", pattern]).await
}

/// `SENTINEL FLUSHCONFIG` on every sentinel: each rewrites its config file.
pub async fn sentinel_flushconfig(server: &RedisServer) -> Result<Vec<SentinelReply>> {
    broadcast(server, None, &["FLUSHCONFIG"]).await
}

/// One line for a notification: `3 sentinels: OK` or the failures named.
pub fn summarize_replies(replies: &[SentinelReply]) -> (usize, Vec<String>) {
    let failed: Vec<String> = replies
        .iter()
        .filter_map(|r| r.result.as_ref().err().map(|e| format!("{}: {e}", r.sentinel)))
        .collect();
    (replies.len(), failed)
}

async fn first_sentinel(server: &RedisServer) -> Result<MultiplexedConnection> {
    crate::async_connection::open_seed_connection(server).await
}

/// Every sentinel of the quorum: the entry's seeds plus, when the master
/// is named, the peers the first reachable seed knows for it. De-duplicated
/// by address; a seed that cannot be reached still gets its own reply.
async fn sentinel_endpoints(server: &RedisServer, master_name: Option<&str>) -> Result<Vec<RedisServer>> {
    let mut addresses: Vec<(String, u16)> = server.seed_endpoints();
    if let Some(name) = master_name
        && let Ok(mut conn) = first_sentinel(server).await
        && let Ok(peers) = cmd("SENTINEL")
            .arg("SENTINELS")
            .arg(name)
            .query_async::<Vec<HashMap<String, String>>>(&mut conn)
            .await
    {
        for peer in peers {
            if let (Some(ip), Some(port)) = (peer.get("ip"), peer.get("port").and_then(|p| p.parse::<u16>().ok())) {
                addresses.push((ip.clone(), port));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    addresses.retain(|addr| seen.insert(addr.clone()));
    if addresses.is_empty() {
        return Err(Error::Invalid {
            message: "no sentinel address to dial".to_string(),
        });
    }
    Ok(addresses
        .into_iter()
        .map(|(host, port)| {
            let mut one = server.clone();
            one.host = host;
            one.port = port;
            one
        })
        .collect())
}

async fn broadcast(server: &RedisServer, master_name: Option<&str>, args: &[&str]) -> Result<Vec<SentinelReply>> {
    let endpoints = sentinel_endpoints(server, master_name).await?;
    let runs = endpoints.iter().map(|endpoint| async move {
        let sentinel = format_host_port(&endpoint.host, endpoint.port);
        let result = async {
            let mut conn = open_seed_endpoint(endpoint).await?;
            let mut command = cmd("SENTINEL");
            for arg in args {
                command.arg(*arg);
            }
            let reply: redis::Value = command.query_async(&mut conn).await?;
            Ok(crate::string::redis_value_to_string(&reply))
        }
        .await;
        SentinelReply { sentinel, result }
    });
    Ok(join_all(runs).await)
}

/// One `SENTINEL MASTERS` / `SENTINEL MASTER` reply map.
fn parse_master(map: &HashMap<String, String>) -> SentinelMaster {
    let field = |key: &str| map.get(key).cloned().unwrap_or_default();
    fn number<T: FromStr + Default>(map: &HashMap<String, String>, key: &str) -> T {
        map.get(key).and_then(|v| v.parse().ok()).unwrap_or_default()
    }
    SentinelMaster {
        name: field("name"),
        ip: field("ip"),
        port: number(map, "port"),
        flags: field("flags"),
        quorum: number(map, "quorum"),
        num_replicas: number(map, "num-slaves"),
        num_other_sentinels: number(map, "num-other-sentinels"),
        down_after_ms: number(map, "down-after-milliseconds"),
        failover_timeout_ms: number(map, "failover-timeout"),
        parallel_syncs: number(map, "parallel-syncs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_masters_reply_and_reads_the_down_flags() {
        let map: HashMap<String, String> = [
            ("name", "mymaster"),
            ("ip", "10.0.0.5"),
            ("port", "6379"),
            ("flags", "master,s_down"),
            ("quorum", "2"),
            ("num-slaves", "1"),
            ("num-other-sentinels", "2"),
            ("down-after-milliseconds", "5000"),
            ("failover-timeout", "180000"),
            ("parallel-syncs", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let master = parse_master(&map);
        assert_eq!(master.name, "mymaster");
        assert_eq!((master.ip.as_str(), master.port), ("10.0.0.5", 6379));
        assert_eq!(
            (master.quorum, master.num_replicas, master.num_other_sentinels),
            (2, 1, 2)
        );
        assert_eq!(
            (master.down_after_ms, master.failover_timeout_ms, master.parallel_syncs),
            (5000, 180000, 1)
        );
        assert!(master.is_down());
        assert!(!SentinelMaster::default().is_down());
    }

    #[test]
    fn a_summary_names_only_the_sentinels_that_refused() {
        let replies = vec![
            SentinelReply {
                sentinel: "a:26379".into(),
                result: Ok("OK".into()),
            },
            SentinelReply {
                sentinel: "b:26379".into(),
                result: Err(Error::Invalid {
                    message: "ERR no such master".into(),
                }),
            },
        ];
        let (total, failed) = summarize_replies(&replies);
        assert_eq!(total, 2);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].starts_with("b:26379: "), "{failed:?}");
        assert!(failed[0].ends_with("ERR no such master"), "{failed:?}");
    }
}
