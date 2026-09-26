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

//! Connect flow (server-type detection, AccessMode probe, database
//! count) and the pooled `ConnectionManager`.

use super::*;
#[cfg(not(target_family = "wasm"))]
use crate::async_connection::open_single_client;
#[cfg(target_family = "wasm")]
use crate::bridge::BridgeConn;
#[cfg(target_family = "wasm")]
use crate::bridge::{BridgeQuery as _, bridge_transport};
use tracing::warn;
use uuid::Uuid;
#[cfg(target_family = "wasm")]
use zedis_core::string::split_host_port_or;

/// Detects the type of Redis server (Sentinel, Cluster, or Standalone).
/// This function checks the role of the Redis server and returns the server type.
/// # Arguments
/// * `client` - The Redis client to check the server type.
/// # Returns
/// * `ServerType` - The type of the Redis server.
#[cfg(not(target_family = "wasm"))]
async fn detect_server_type(conn: MultiplexedConnection) -> Result<ServerType> {
    detect_server_type_over(&mut RedisAsyncConn::Single(conn)).await
}

/// The detection itself: two commands and no dialling, so the browser runs
/// exactly this over the bridge.
async fn detect_server_type_over(conn: &mut RedisAsyncConn) -> Result<ServerType> {
    // Check if it's a Sentinel. `ROLE` is missing on some managed / old servers
    // (e.g. Upstash, which answers "command not available"). Treat that as
    // "not a sentinel" and fall through to the INFO check rather than failing
    // detection outright — only a genuine error is propagated.
    match cmd("ROLE").query_async::<Role>(conn).await {
        Ok(Role::Sentinel { .. }) => return Ok(ServerType::Sentinel),
        Ok(_) => {}
        Err(e) if is_ignorable_server_error(&e.to_string()) => {
            // Visible at the default INFO level (fires once per connection, not
            // per heartbeat), so a restricted server is still diagnosable.
            info!("ROLE command unavailable, assuming non-sentinel: {e}");
        }
        Err(e) => return Err(e.into()),
    }

    // Check if Cluster mode is enabled via INFO command
    let reply: Value = cmd("INFO").arg("cluster").query_async(conn).await?;
    if cluster_enabled(reply)? {
        Ok(ServerType::Cluster)
    } else {
        Ok(ServerType::Standalone)
    }
}

/// `cluster_enabled` out of an `INFO cluster` reply, in either shape it comes.
///
/// The desktop asks over a single connection to the seed and gets the text.
/// The browser asks over the bridge, whose pooled connection for an entry it
/// already knows to be a cluster is redis-rs's *cluster-routed* one — and that
/// sends a keyless `INFO` to every node and answers a map of node → text
/// (`ResponsePolicy::Special`). Reading only the first shape made the browser
/// call every cluster "standalone", after which `INFO server` took the
/// non-cluster branch, met the same map, and failed the whole selection. Any
/// node's answer settles it: they are all members of the one cluster.
fn cluster_enabled(reply: Value) -> Result<bool> {
    let text = match reply {
        Value::Map(nodes) => nodes.into_iter().next().map(|(_node, text)| text).unwrap_or(Value::Nil),
        text => text,
    };
    // The same conversion `query_async::<InfoDict>` makes, so a reply that is
    // neither shape is the error it always was.
    let info = InfoDict::from_redis_value(text).map_err(redis::RedisError::from)?;
    Ok(info.get("cluster_enabled").unwrap_or(0i64) == 1)
}

/// Whether the *caller* may write at all, before any Redis user is asked.
///
/// There is only a caller to ask about in the browser. On the desktop the
/// person running the app is the account, so nothing ever sets this and the
/// question is entirely the Redis user's; through a bridge there is a second
/// one in front of it, and an account marked read-only there is refused
/// whatever its Redis user may do (`zedis-bridge`'s `auth::Account`).
///
/// Ungated on purpose, even though only `zedis-web` ever writes it: a
/// `cfg(target_family = "wasm")` export here is invisible to `make lint`,
/// which builds every crate — `zedis-web` included — for the host.
static ACCOUNT_READ_ONLY_ON: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

fn account_is_read_only_on(server_id: &str) -> bool {
    ACCOUNT_READ_ONLY_ON
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(server_id)
}

/// Record on which entries the signed-in bridge account may not write. Set
/// from the server list — the whole set each time, so an entry the bridge
/// no longer marks drops out — before anything is connected.
///
/// A read-only *account* marks every entry; an account whose `servers`
/// rules grant an entry as `:ro` marks that entry. Either way the
/// connection becomes [`AccessMode::StrictReadOnly`] — the one the UI
/// cannot switch off — rather than the entry-level `SafeMode`, because this
/// is not a setting the user chose and so is not one they can revoke. The
/// bridge refuses the write either way; this is what stops the app offering
/// a button whose only possible outcome is a `403`.
pub fn set_account_read_only_on(server_ids: std::collections::HashSet<String>) {
    *ACCOUNT_READ_ONLY_ON
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = server_ids;
}

/// What a permission probe learned about the connected ACL user's right to
/// write. Only [`CommandDenied`](Self::CommandDenied) makes the connection
/// [`AccessMode::StrictReadOnly`]; everything uncertain leans writable,
/// because a wrongly locked UI blocks real work while a wrongly unlocked one
/// just surfaces the server's own `NOPERM` on the first write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteVerdict {
    /// The write would go through.
    Allowed,
    /// The user may not run the write command at all — a read-only user.
    CommandDenied,
    /// The command is allowed, just not on the probe key: the user writes
    /// within a key scope (`~app:*`). A `%R~*` read-only key permission
    /// lands here too — the one shape a fresh-key probe cannot tell apart.
    KeyDenied,
    /// The reply said nothing about the user: a replica's `READONLY`, an
    /// `OOM`, a proxy's refusal.
    Unknown,
}

/// Read a denial — `ACL DRYRUN`'s reply text or a `NOPERM` error — for
/// *what* was denied. Every Redis since 6.0 words a command denial as
/// "…no permissions to run the '<cmd>' command" and a key denial as
/// "…no permissions to access …" (6.x: "one of the keys used as arguments";
/// 7.0+: "a key" / "the '<key>' key"). Unrecognised text counts as a
/// command denial, the conservative reading of "not OK".
fn classify_denial(text: &str) -> WriteVerdict {
    if text.contains("to run the") {
        WriteVerdict::CommandDenied
    } else if text.contains("to access") {
        WriteVerdict::KeyDenied
    } else {
        WriteVerdict::CommandDenied
    }
}

/// Key name for the write probes. Fresh per call, so it cannot collide with
/// a real key: `SET … XX` only ever meets an absent key and stays a no-op.
fn probe_key() -> String {
    format!("zedis:acl-probe:{}", Uuid::now_v7())
}

/// Fallback when `ACL DRYRUN` is unavailable (before 7.0) or denied to this
/// user (it is `@admin` — exactly what a restricted user lacks): send a real
/// write so the ACL check runs, but one that cannot mutate. `SET <fresh key>
/// 1 XX` (2.6.12+) aborts with nil before touching the dataset when the key
/// is absent: no key, no keyspace event, nothing replicated or appended to
/// the AOF. Only MONITOR, `INFO commandstats` and, when denied, `ACL LOG`
/// see it. Its predecessor, `SET … EX 1`, really wrote a key on production
/// and fired set / expire / expired events on every connect.
async fn probe_write_permission(mut conn: RedisAsyncConn) -> WriteVerdict {
    let key = probe_key();
    let probe: redis::RedisResult<Value> = cmd("SET").arg(&key).arg("1").arg("XX").query_async(&mut conn).await;
    match probe {
        Ok(Value::Okay) => {
            // Only possible if the fresh key already existed. Nothing can be
            // undone safely from here, so say so.
            warn!(key, "write probe found its fresh key present and overwrote it");
            WriteVerdict::Allowed
        }
        Ok(_) => WriteVerdict::Allowed,
        Err(e) => match e.code() {
            Some("NOPERM") => classify_denial(&e.to_string()),
            // A replica refuses every write whatever the ACL says (`READONLY`);
            // `OOM`, `NOREPLICAS` or a proxy's refusal are about the server
            // too. None of them describes the user.
            Some(code) => {
                debug!(code, error = %e, "write probe inconclusive");
                WriteVerdict::Unknown
            }
            None => WriteVerdict::Unknown,
        },
    }
}

/// Stage 2: `ACL DRYRUN` (7.0+) is a pure simulation — no side effects at
/// all — so it is asked first. When the server has no DRYRUN ("unknown
/// subcommand" before 7.0) or the user may not run it, the no-op write
/// answers instead.
async fn dryrun_or_probe(mut conn: RedisAsyncConn, user: &str) -> WriteVerdict {
    let result: redis::RedisResult<String> = cmd("ACL")
        .arg("DRYRUN")
        .arg(user)
        .arg("SET")
        .arg(probe_key())
        .arg("1")
        .query_async(&mut conn)
        .await;
    match result {
        Ok(res) if res == "OK" => WriteVerdict::Allowed,
        Ok(res) => classify_denial(&res),
        Err(e) if e.code() == Some("NOPERM") || e.to_string().to_ascii_lowercase().contains("unknown") => {
            probe_write_permission(conn).await
        }
        Err(e) => {
            debug!(error = %e, "ACL DRYRUN inconclusive");
            WriteVerdict::Unknown
        }
    }
}

/// Whether the connected ACL user is read-only. Three stages, each asked
/// only when the previous one could not answer, and none of them writes:
///
/// 1. `ACL WHOAMI`. "Unknown command" means no ACL at all (pre-6.0, some
///    proxies) and therefore no read-only users. A `NOPERM` is itself
///    information — ACL exists, the user just cannot introspect — and goes
///    straight to stage 3: `+@read ~*`, the most ordinary read-only user,
///    lands here, and used to be reported writable because the check gave
///    up on it.
/// 2. [`dryrun_or_probe`] — `ACL DRYRUN`, falling through to
/// 3. [`probe_write_permission`] — the no-op write.
///
/// Only a denial of the *command* is read-only; a denial of the probe *key*
/// means the user writes within a key scope and must keep the write UI
/// (`~app:*` users used to be locked out entirely).
async fn safe_check_user_readonly(mut conn: RedisAsyncConn) -> bool {
    let whoami: redis::RedisResult<String> = cmd("ACL").arg("WHOAMI").query_async(&mut conn).await;
    let verdict = match whoami {
        Ok(user) if !user.is_empty() => dryrun_or_probe(conn, &user).await,
        Ok(_) => WriteVerdict::Unknown,
        Err(e) if e.code() == Some("NOPERM") => probe_write_permission(conn).await,
        Err(e) => {
            debug!(error = %e, "ACL WHOAMI unavailable, assuming a server without ACL users");
            WriteVerdict::Unknown
        }
    };
    verdict == WriteVerdict::CommandDenied
}

async fn get_modules(mut conn: RedisAsyncConn) -> Result<Vec<(String, Version)>> {
    let module_list: Vec<redis::Value> = cmd("MODULE").arg("LIST").query_async(&mut conn).await?;
    let mut modules = Vec::with_capacity(module_list.len());
    for module_info in module_list {
        if let Value::Array(info_kv) = module_info {
            // 遍历内部的 Key-Value 数组 (例如 ["name", "ReJSON", "ver", 20407])
            let mut name: Option<String> = None;
            let mut version = Version::new(0, 0, 0);
            let mut iter = info_kv.chunks(2);
            while let Some([key, val]) = iter.next() {
                if let Value::BulkString(k) = key {
                    match k.as_slice() {
                        b"name" => {
                            if let Value::BulkString(v) = val {
                                name = Some(String::from_utf8_lossy(v).into_owned());
                            }
                        }
                        b"ver" => {
                            if let Value::Int(v) = val {
                                let v = *v as u64;
                                version = Version::new(v / 10000, (v % 10000) / 100, v % 100);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if let Some(name) = name {
                modules.push((name, version));
            }
        }
    }
    Ok(modules)
}
async fn get_databases(mut conn: RedisAsyncConn, is_cluster: bool) -> Result<usize> {
    // Step 1 — CONFIG GET databases: the exact count on self-hosted /
    // unrestricted servers. Correct in cluster mode too: stock Redis
    // forces `databases` to 1 at startup when cluster is enabled
    // ("Changing databases number from 16 to 1 since we are in cluster
    // mode"), while Valkey 9+ cluster reports its real multi-db count.
    let config_reply: redis::RedisResult<Vec<String>> =
        cmd("CONFIG").arg("GET").arg("databases").query_async(&mut conn).await;
    if let Ok(reply) = config_reply
        && let Some(count) = reply.get(1).and_then(|s| s.parse::<usize>().ok()).filter(|&n| n > 0)
    {
        return Ok(count);
    }

    // Step 2 — CONFIG blocked (e.g. AWS ElastiCache) → degrade to INFO
    // keyspace, which ACLs and managed clouds almost always allow. It lists
    // only *non-empty* DBs, so the highest `dbN:` line proves at least N+1
    // selectable DBs exist; clamp up to the conventional 16 so the switcher
    // offers the usual range. NOT in cluster mode: a stock cluster is
    // db0-only, so rounding "saw db0" up to 16 would offer 15 databases
    // that don't exist — stick to the proven count there.
    let info_reply: redis::RedisResult<String> = cmd("INFO").arg("keyspace").query_async(&mut conn).await;
    if let Ok(info) = info_reply {
        let mut max_db: Option<usize> = None;
        for line in info.lines() {
            if let Some(rest) = line.strip_prefix("db")
                && let Some(n) = rest.split(':').next().and_then(|s| s.parse::<usize>().ok())
            {
                max_db = Some(max_db.map_or(n, |m| m.max(n)));
            }
        }
        if let Some(n) = max_db {
            return Ok(if is_cluster { n + 1 } else { (n + 1).max(16) });
        }
    }

    // Both probes inconclusive (CONFIG blocked + every DB empty). Fall back to
    // 1 — the user can still set an explicit count in the server config.
    Ok(1)
}
/// A server that has been reached, before anything is asked of it.
///
/// The one thing that genuinely differs between the two builds. Splitting it
/// out is what keeps `get_client_without_cache` single: everything past this
/// point is commands, and commands travel the same way on both (ADR 9).
struct Reached {
    connection: RedisAsyncConn,
    server_type: ServerType,
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    sentinel_master_names: Vec<String>,
    /// What built the connection, so a second one can be opened without
    /// re-running discovery. Nothing builds connections in the browser.
    #[cfg(not(target_family = "wasm"))]
    rclient: RClient,
}

/// The cluster's masters, as the labels the bridge answers a fan-out with.
///
/// A `PING` is the cheapest command that reaches every master, and the reply
/// is thrown away — only the `nodes` list beside it is wanted.
#[cfg(target_family = "wasm")]
async fn bridge_master_nodes(connection: &RedisAsyncConn) -> Vec<RedisNode> {
    let Some(conn) = connection.as_bridge() else {
        return Vec::new();
    };
    match conn.fanout_masters(vec![cmd("PING").get_packed_command()]).await {
        Ok((labels, _)) => labels
            .iter()
            .map(|label| {
                let (host, port) = split_host_port_or(label, 0);
                RedisNode {
                    server: RedisServer {
                        name: label.clone(),
                        host: host.to_string(),
                        port,
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })
            .collect(),
        Err(e) => {
            warn!(error = %e, "the bridge could not list the masters");
            Vec::new()
        }
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            clients: TtlCache::new(Duration::from_secs(5 * 60)),
        }
    }
    /// Discovers Redis nodes and server type based on initial configuration.
    /// `pub(super)` so the sharded Pub/Sub sibling module can build its
    /// dedicated connection from the same node discovery.
    #[cfg(not(target_family = "wasm"))]
    pub(super) async fn get_redis_nodes(&self, name: &str) -> Result<NodeDiscovery> {
        let config = get_server(name)?;
        let (mut conn, server_type) = {
            // A seed the helper cannot reach is not a standalone in
            // disguise. The old fallback re-dialled the very same endpoint
            // with the same credentials and TLS material as a standalone,
            // so an unreachable server cost two handshakes per attempt and
            // surfaced as a "standalone" whose PING then failed a second
            // later. The error (already retried with the sentinel's
            // credentials, or none) is the answer; only a server that
            // *answers* but rejects the detection commands is treated as a
            // standalone below (ADR 5).
            let conn = open_seed_connection(&config).await?;
            if let Some(server_type) = config.server_type
                && server_type != SERVER_TYPE_AUTO
            {
                (conn, server_type.into())
            } else {
                match detect_server_type(conn.clone()).await {
                    Ok(server_type) => (conn, server_type),
                    Err(e) => {
                        if !is_ignorable_server_error(&e.to_string()) {
                            return Err(e);
                        }
                        // Expected on restricted servers (Upstash etc.) — not an
                        // error, but logged at INFO so it stays visible by default.
                        info!("server type detection unsupported, using standalone mode: {e:?}");
                        (conn, ServerType::Standalone)
                    }
                }
            }
        };
        match server_type {
            ServerType::Cluster => {
                // Fetch cluster topology
                let nodes: String = cmd("CLUSTER").arg("NODES").query_async(&mut conn).await?;
                // Parse nodes and convert to RedisNode
                let nodes = parse_cluster_nodes(&nodes)?
                    .iter()
                    .map(|item| {
                        let mut tmp_config = config.clone();
                        tmp_config.port = item.port;
                        if !item.ip.is_empty() {
                            tmp_config.host = item.ip.clone();
                        }

                        RedisNode {
                            server: tmp_config,
                            role: item.role.clone(),
                            cluster_id: Some(item.id.clone()),
                            master_cluster_id: item.master_id.clone(),
                            slots: item.slots.clone(),
                            migrations: item.migrations.clone(),
                            ..Default::default()
                        }
                    })
                    .collect();
                Ok(NodeDiscovery::new(nodes, server_type))
            }
            ServerType::Sentinel => {
                // Every master the sentinel monitors, by name. The entry's
                // master name picks one; without it the first by name is
                // used and the whole list travels with the client, so the
                // Topology panel can offer the others instead of the connect
                // failing on an ambiguity the user has not been shown yet.
                let masters_response: Vec<HashMap<String, String>> =
                    cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
                let mut masters: Vec<(String, RedisNode)> = vec![];
                for item in masters_response {
                    let ip = item.get("ip").ok_or_else(|| Error::Invalid {
                        message: "ip is not found".to_string(),
                    })?;
                    let port: u16 = item
                        .get("port")
                        .ok_or_else(|| Error::Invalid {
                            message: "port is not found".to_string(),
                        })?
                        .parse()
                        .map_err(|e| Error::Invalid {
                            message: format!("Invalid port {e:?}"),
                        })?;
                    let name = item.get("name").ok_or_else(|| Error::Invalid {
                        message: "master_name is not found".to_string(),
                    })?;
                    let mut tmp_config = config.clone();
                    tmp_config.host = ip.clone();
                    tmp_config.port = port;
                    masters.push((
                        name.clone(),
                        RedisNode {
                            server: tmp_config,
                            role: NodeRole::Master,
                            master_name: Some(name.clone()),
                            ..Default::default()
                        },
                    ));
                }
                masters.sort_by(|a, b| a.0.cmp(&b.0));
                let names: Vec<String> = masters.iter().map(|(name, _)| name.clone()).collect();
                let chosen = match config.master_name.as_deref().filter(|n| !n.is_empty()) {
                    Some(wanted) => masters
                        .into_iter()
                        .find(|(name, _)| name == wanted)
                        .map(|(_, node)| node)
                        .ok_or_else(|| Error::Invalid {
                            message: format!(
                                "master {wanted} is not monitored by this sentinel; it monitors {names:?}"
                            ),
                        })?,
                    None => masters
                        .into_iter()
                        .next()
                        .map(|(_, node)| node)
                        .ok_or_else(|| Error::Invalid {
                            message: "the sentinel monitors no master".to_string(),
                        })?,
                };
                Ok(NodeDiscovery {
                    nodes: vec![chosen],
                    server_type,
                    sentinel_master_names: names,
                })
            }
            _ => Ok(NodeDiscovery::new(
                vec![RedisNode {
                    server: config.clone(),
                    role: NodeRole::Master,
                    ..Default::default()
                }],
                server_type,
            )),
        }
    }
    /// The masters of the pooled client for `(server, db)` when one is
    /// cached — read without dialling, so a server answering `BUSY` to
    /// everything still yields the nodes a kill has to reach.
    pub fn cached_master_servers(&self, server_id: &str, db: usize) -> Option<Vec<RedisServer>> {
        let config = get_server(server_id).ok()?;
        self.clients
            .get(&config.get_hash(db))
            .map(|client| client.master_servers())
    }

    pub fn remove_client(&self, server_id: &str, db: usize) {
        let Ok(config) = get_server(server_id) else {
            return;
        };
        let key = config.get_hash(db);
        self.clients.remove(&key);
        #[cfg(not(target_family = "wasm"))]
        remove_connection_from_pool(&config, db);
    }
    /// A long-lived push stream, which the bridge's request/response protocol
    /// has no shape for — the web build drops the panels that subscribe
    /// (ADR 9).
    #[cfg(not(target_family = "wasm"))]
    pub async fn get_pubsub_connection(&self, server_id: &str) -> Result<redis::aio::PubSub> {
        let config = get_server(server_id)?;
        // The shared builder, so a subscription gets the same TLS handling
        // and the keepalive probes a read-only connection depends on.
        let client = open_single_client(&config)?;
        let pubsub = client.get_async_pubsub().await?;
        Ok(pubsub)
    }
    /// Retrieves or creates a RedisClient for the given configuration name without caching.
    ///
    /// Everything after [`Reached`] is the same on both targets, because it is
    /// all ordinary commands: the access-mode probe, `MODULE LIST`, the
    /// database count, `INFO server`. Only *getting there* differs (ADR 9).
    pub async fn get_client_without_cache(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        // The desktop reads the entry again inside discovery; the browser has
        // nothing to discover and reads it here.
        #[cfg(not(target_family = "wasm"))]
        let reached = self.reach(server_id, db).await?;
        #[cfg(target_family = "wasm")]
        let reached = self.reach(server_id, &config, db).await?;
        let Reached {
            connection,
            server_type,
            nodes,
            master_nodes,
            sentinel_master_names,
            #[cfg(not(target_family = "wasm"))]
            rclient,
        } = reached;
        let access_mode = if account_is_read_only_on(&config.id) || safe_check_user_readonly(connection.clone()).await {
            AccessMode::StrictReadOnly
        } else if config.readonly.unwrap_or(false) {
            AccessMode::SafeMode
        } else {
            AccessMode::ReadWrite
        };
        // `MODULE LIST` is denied on most managed clouds; module panels
        // then stay hidden, which is right — but the reason belongs in the log.
        let modules = get_modules(connection.clone()).await.unwrap_or_else(|e| {
            debug!(error = %e, "MODULE LIST unavailable, assuming no modules");
            Vec::new()
        });
        // Prefer the user-configured count — it works on managed clouds that
        // block `CONFIG` (ElastiCache) and on Valkey cluster (multi-db). Only
        // probe `CONFIG GET databases` when the server config leaves it unset.
        let databases = match config.databases {
            Some(n) => n,
            None => get_databases(connection.clone(), server_type == ServerType::Cluster)
                .await
                .unwrap_or(1),
        };
        let mut client = RedisClient {
            db,
            databases,
            modules,
            access_mode,
            server_type: server_type.clone(),
            nodes,
            master_nodes,
            sentinel_master_names,
            version: Version::new(0, 0, 0),
            is_valkey: false,
            connection,
            #[cfg(not(target_family = "wasm"))]
            client: rclient,
        };
        let mut conn = client.connection.clone();
        let get_version = |info: InfoDict| -> (bool, Option<Version>) {
            if let Some(v) = info.get::<String>("valkey_version") {
                return (true, Version::parse(&v).ok());
            }
            if let Some(v) = info.get::<String>("redis_version") {
                return (false, Version::parse(&v).ok());
            }
            (false, None)
        };

        (client.is_valkey, client.version) = match server_type {
            ServerType::Cluster => {
                let info: redis::Value = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let mut version = None;
                let mut is_valkey = false;
                if let redis::Value::Map(items) = info {
                    for (_, node_info_val) in items {
                        if let Ok(info) = InfoDict::from_redis_value(node_info_val)
                            && let (valkey, Some(v)) = get_version(info)
                        {
                            version = Some(v);
                            is_valkey = valkey;
                            break;
                        }
                    }
                }
                (is_valkey, version.unwrap_or(Version::new(0, 0, 0)))
            }
            _ => {
                let info: InfoDict = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let (is_valkey, version) = get_version(info);
                (is_valkey, version.unwrap_or(Version::new(0, 0, 0)))
            }
        };

        debug!(server_id, version = client.version(), modules = ?client.modules, db, access_mode = ?client.access_mode(), "create redis client success");
        Ok(client)
    }

    /// Reach the server the desktop way: discover the topology, build the
    /// redis-rs client for it, and open a connection.
    #[cfg(not(target_family = "wasm"))]
    async fn reach(&self, server_id: &str, db: usize) -> Result<Reached> {
        let NodeDiscovery {
            nodes,
            server_type,
            sentinel_master_names,
        } = self.get_redis_nodes(server_id).await?;
        debug!(server_id, server_type = ?server_type, nodes = ?nodes, "get redis nodes");
        let Some(first_node) = nodes.first() else {
            return Err(Error::Invalid {
                message: "no nodes found".to_string(),
            });
        };
        let rclient = match server_type {
            ServerType::Cluster => {
                let addrs: Vec<String> = nodes.iter().map(|n| n.server.get_connection_url()).collect();
                // Bake the (per-server, else global) timeouts into the client
                // here — the cluster connection getters use the client's
                // configured timeouts rather than a per-call override.
                let mut builder = cluster::ClusterClientBuilder::new(addrs)
                    .connection_timeout(resolve_connection_timeout(&first_node.server))
                    .response_timeout(resolve_response_timeout(&first_node.server));
                // Valkey 9+ supports multiple databases in cluster mode;
                // clients are cached per `(server, db)`, so bake the db into
                // the cluster client (the driver issues SELECT on connect and
                // re-applies it after reconnects). Skipped for db 0 — the
                // default — so stock Redis clusters (db0-only) never see a
                // SELECT in the handshake.
                if db != 0 {
                    builder = builder.database_id(db as i64);
                }
                if let Some(certificates) = first_node.server.tls_certificates()? {
                    builder = builder.certs(certificates);
                }
                if first_node.server.insecure.unwrap_or(false) {
                    builder = builder.danger_accept_invalid_hostnames(true);
                }
                // Reads go to a replica of the slot's shard when asked;
                // scans are unaffected (they fan out to the masters explicitly).
                if first_node.server.cluster_read_replicas.unwrap_or(false) {
                    builder = builder.read_routing_strategy(redis::cluster_read_routing::RandomReplicaStrategy);
                }
                if first_node.server.is_ssh_tunnel() {
                    builder = builder.username(server_id);

                    RClient::SshCluster(builder.build()?)
                } else {
                    RClient::Cluster(builder.build()?)
                }
            }
            _ => RClient::Single(Box::new(first_node.server.clone())),
        };
        let master_nodes: Vec<RedisNode> = nodes
            .iter()
            .filter(|node| node.role == NodeRole::Master)
            .cloned()
            .collect();
        let master_nodes_description: Vec<String> = master_nodes.iter().map(|node| node.host_port()).collect();
        info!(master_nodes = ?master_nodes_description, "server master nodes");
        let connection = get_async_connection(&rclient, db, false).await?;
        Ok(Reached {
            connection,
            server_type,
            nodes,
            master_nodes,
            sentinel_master_names,
            rclient,
        })
    }

    /// Reach the server the browser way: there is nothing to dial, so the
    /// connection is a bridge request and the questions that follow are the
    /// commands `get_client_without_cache` was going to ask anyway.
    #[cfg(target_family = "wasm")]
    async fn reach(&self, server_id: &str, config: &RedisServer, _db: usize) -> Result<Reached> {
        let transport = bridge_transport().ok_or_else(|| Error::Invalid {
            message: "no bridge transport is installed".to_string(),
        })?;
        let mut connection = RedisAsyncConn::Bridge(BridgeConn::new(transport, server_id, _db));
        // What kind of server the bridge reached is still answered by
        // commands, so this is the desktop's own detection, unchanged.
        let server_type = match config.server_type {
            Some(pinned) if pinned != SERVER_TYPE_AUTO => pinned.into(),
            _ => detect_server_type_over(&mut connection).await.unwrap_or_else(|e| {
                info!("server type detection unsupported, using standalone mode: {e:?}");
                ServerType::Standalone
            }),
        };
        // Masters as *labels*, learned from the bridge's own fan-out rather
        // than from `CLUSTER NODES` — for every topology, not only a cluster. A
        // label is all `query_async_masters_with_option` aligns replies on, and
        // the browser's copy of the entry has no host to build one from: the
        // bridge's list gives ids and names, because handing the browser a
        // dialable address is precisely what the bridge exists to avoid
        // (ADR 9). A standalone answers with its one label the same way.
        let mut master_nodes = bridge_master_nodes(&connection).await;
        if master_nodes.is_empty() {
            // The fan-out did not answer; keep the entry itself so the client
            // still has a node to count, even if it cannot aim a scan.
            master_nodes.push(RedisNode {
                server: config.clone(),
                ..Default::default()
            });
        }
        Ok(Reached {
            connection,
            server_type,
            nodes: master_nodes.clone(),
            master_nodes,
            // Sentinel administration dials the sentinels themselves, which
            // is server-side work the web build does not offer.
            sentinel_master_names: Vec::new(),
        })
    }

    /// Retrieves or creates a RedisClient for the given configuration name.
    pub async fn get_client(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        let key = config.get_hash(db);
        if let Some(client) = self.clients.get(&key) {
            debug!(server_id, db, "get client from cache");
            return Ok(client.clone());
        }
        let client = self.get_client_without_cache(server_id, db).await?;
        // Cache the client
        self.clients.insert(key, client.clone());
        Ok(client)
    }
    /// Shorthand to get an async connection directly.
    pub async fn get_connection(&self, server_id: &str, db: usize) -> Result<RedisAsyncConn> {
        let client = self.get_client(server_id, db).await?;
        Ok(client.connection.clone())
    }

    /// A fresh connection that shares nothing with the pooled client, for an
    /// owner that sends *connection-scoped* commands. The terminal is the
    /// case: `SELECT`, `AUTH`, `CLIENT SETNAME`, `CLIENT TRACKING` and
    /// `MULTI` typed there change the state of whichever connection carries
    /// them — and on the pooled one that state is shared with the key tree,
    /// so a `SELECT 3` silently moved every later `SCAN` to db 3.
    ///
    /// Topology (cluster / SSH cluster) comes from the cached client, so no
    /// discovery re-runs. The connection itself is never cached, never
    /// heartbeat-checked and never healed: the caller owns it, drops it to
    /// close it, and reopens after a link error.
    pub async fn open_dedicated_connection(&self, server_id: &str, db: usize) -> Result<RedisAsyncConn> {
        let client = self.get_client(server_id, db).await?;
        client.open_dedicated_connection().await
    }
}

#[cfg(test)]
mod tests {
    use super::{WriteVerdict, classify_denial, cluster_enabled, probe_key};
    use redis::Value;

    fn text(info: &str) -> Value {
        Value::BulkString(info.as_bytes().to_vec())
    }

    #[test]
    fn cluster_mode_is_read_from_a_single_reply_and_from_a_per_node_map() {
        // A single connection to the seed: what the desktop's detection sees.
        assert!(cluster_enabled(text("# Cluster\r\ncluster_enabled:1\r\n")).expect("text"));
        assert!(!cluster_enabled(text("# Cluster\r\ncluster_enabled:0\r\n")).expect("text"));
        assert!(!cluster_enabled(text("# Cluster\r\n")).expect("absent means no"));

        // redis-rs's cluster-routed connection: what the bridge hands the
        // browser for an entry it knows to be a cluster. Read as text only,
        // this was an error, the browser fell back to "standalone", and the
        // selection then failed on the same map from `INFO server`.
        let per_node = Value::Map(vec![
            (text("10.51.168.20:31545"), text("# Cluster\r\ncluster_enabled:1\r\n")),
            (text("10.51.135.55:30505"), text("# Cluster\r\ncluster_enabled:1\r\n")),
        ]);
        assert!(cluster_enabled(per_node).expect("map"));
        // No node answered: an error, which the caller logs — not a quiet
        // "not a cluster".
        assert!(cluster_enabled(Value::Map(Vec::new())).is_err());
    }

    #[test]
    fn denial_texts_of_every_supported_server_are_classified() {
        // Redis 7.0+ / Valkey — DRYRUN reply and the real NOPERM.
        assert_eq!(
            classify_denial("User ro has no permissions to run the 'set' command"),
            WriteVerdict::CommandDenied
        );
        assert_eq!(
            classify_denial("NOPERM User ro has no permissions to run the 'set' command"),
            WriteVerdict::CommandDenied
        );
        assert_eq!(
            classify_denial("User scoped has no permissions to access the 'zedis:acl-probe:x' key"),
            WriteVerdict::KeyDenied
        );
        assert_eq!(
            classify_denial("NOPERM No permissions to access a key"),
            WriteVerdict::KeyDenied
        );
        // Redis 6.x wording.
        assert_eq!(
            classify_denial("NOPERM this user has no permissions to run the 'set' command or its subcommand"),
            WriteVerdict::CommandDenied
        );
        assert_eq!(
            classify_denial("NOPERM this user has no permissions to access one of the keys used as arguments"),
            WriteVerdict::KeyDenied
        );
        // Anything else that was still "not OK" stays on the safe side.
        assert_eq!(classify_denial("NOPERM"), WriteVerdict::CommandDenied);
    }

    #[test]
    fn probe_keys_never_repeat() {
        let a = probe_key();
        let b = probe_key();
        assert!(a.starts_with("zedis:acl-probe:"));
        assert_ne!(
            a, b,
            "a repeated probe key could meet a key the previous probe left behind"
        );
    }

    /// An endpoint that refuses the connection is reported as the error it
    /// is and never re-dialled as a "standalone" (ADR 5): the heartbeat used
    /// to pay two handshakes per tick for that against a dead server.
    #[test]
    fn unreachable_seed_is_an_error_not_a_standalone() {
        use crate::config::{RedisServer, save_servers};
        use crate::error::ConnectionErrorKind;
        use crate::get_connection_manager;
        use std::net::TcpListener;
        use zedis_core::fs::override_config_dir;

        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        // Bind an ephemeral port and release it: nothing listens there now.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("ephemeral port")
            .port();
        let id = "unreachable-seed";
        let server = RedisServer {
            id: id.to_string(),
            name: id.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            ..Default::default()
        };
        let err = smol::block_on(async {
            save_servers(vec![server]).await.expect("save servers");
            get_connection_manager()
                .get_client(id, 0)
                .await
                .err()
                .expect("a refused connection must not become a standalone client")
        });
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Network, "{err}");
    }
}
