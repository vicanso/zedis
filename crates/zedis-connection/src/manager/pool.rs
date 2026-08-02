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

/// Detects the type of Redis server (Sentinel, Cluster, or Standalone).
/// This function checks the role of the Redis server and returns the server type.
/// # Arguments
/// * `client` - The Redis client to check the server type.
/// # Returns
/// * `ServerType` - The type of the Redis server.
async fn detect_server_type(mut conn: MultiplexedConnection) -> Result<ServerType> {
    // Check if it's a Sentinel. `ROLE` is missing on some managed / old servers
    // (e.g. Upstash, which answers "command not available"). Treat that as
    // "not a sentinel" and fall through to the INFO check rather than failing
    // detection outright — only a genuine error is propagated.
    match cmd("ROLE").query_async::<Role>(&mut conn).await {
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
    let info: InfoDict = cmd("INFO").arg("cluster").query_async(&mut conn).await?;
    let cluster_enabled = info.get("cluster_enabled").unwrap_or(0i64);

    if cluster_enabled == 1 {
        Ok(ServerType::Cluster)
    } else {
        Ok(ServerType::Standalone)
    }
}

async fn check_permission_by_probing(mut conn: RedisAsyncConn) -> bool {
    let probe: redis::RedisResult<String> = cmd("SET")
        .arg("_zedis_auth_test_")
        .arg("1")
        .arg("EX")
        .arg("1")
        .query_async(&mut conn)
        .await;

    match probe {
        Ok(_) => false,
        Err(e) => e.code() == Some("NOPERM"),
    }
}

async fn safe_check_user_readonly(mut conn: RedisAsyncConn) -> bool {
    let user: String = cmd("ACL")
        .arg("WHOAMI")
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    let result: redis::RedisResult<String> = cmd("ACL")
        .arg("DRYRUN")
        .arg(user)
        .arg("SET")
        .arg("zedis")
        .arg("treexie")
        .query_async(&mut conn)
        .await;
    match result {
        Ok(res) => res != "OK",

        Err(e) => {
            if let Some(code) = e.code()
                && code == "NOPERM"
            {
                if e.to_string().contains("acl|dryrun") {
                    return check_permission_by_probing(conn).await;
                }
                return true;
            }
            false
        }
    }
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
impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            clients: TtlCache::new(Duration::from_secs(5 * 60)),
        }
    }
    /// Discovers Redis nodes and server type based on initial configuration.
    /// `pub(super)` so the sharded Pub/Sub sibling module can build its
    /// dedicated connection from the same node discovery.
    pub(super) async fn get_redis_nodes(&self, name: &str) -> Result<(Vec<RedisNode>, ServerType)> {
        let config = get_server(name)?;
        let (mut conn, server_type) = {
            let conn = match open_single_connection(&config, 0, false).await {
                Ok(conn) => conn,
                Err(e) => {
                    if !e.to_string().contains("AuthenticationFailed") {
                        error!("detect server type failed: {e:?}, use standalone mode");
                        return Ok((
                            vec![RedisNode {
                                server: config.clone(),
                                role: NodeRole::Master,
                                ..Default::default()
                            }],
                            ServerType::Standalone,
                        ));
                    }
                    // sentinel without password
                    // detect server type again
                    let mut tmp_config = config.clone();
                    tmp_config.password = None;
                    open_single_connection(&tmp_config, 0, false).await?
                }
            };
            if let Some(server_type) = config.server_type
                && server_type > 0
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
                Ok((nodes, server_type))
            }
            ServerType::Sentinel => {
                // let mut conn = client.get_multiplexed_async_connection().await?;
                // Fetch masters from Sentinel
                let masters_response: Vec<HashMap<String, String>> =
                    cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
                let mut nodes = vec![];

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
                    // Filter by master name if configured
                    if let Some(master_name) = &config.master_name
                        && name != master_name
                    {
                        continue;
                    }
                    let mut tmp_config = config.clone();
                    tmp_config.host = ip.clone();
                    tmp_config.port = port;

                    nodes.push(RedisNode {
                        server: tmp_config,
                        role: NodeRole::Master,
                        master_name: Some(name.clone()),
                        ..Default::default()
                    });
                }
                // Check for ambiguous master configuration
                let unique_masters: HashSet<_> = nodes.iter().filter_map(|n| n.master_name.as_ref()).collect();
                if unique_masters.len() > 1 {
                    return Err(Error::Invalid {
                        message: format!(
                            "Multiple masters found in Sentinel, please specify master_name, master_names: {unique_masters:?}"
                        ),
                    });
                }

                Ok((nodes, server_type))
            }
            _ => Ok((
                vec![RedisNode {
                    server: config.clone(),
                    role: NodeRole::Master,
                    ..Default::default()
                }],
                server_type,
            )),
        }
    }
    pub fn remove_client(&self, server_id: &str, db: usize) {
        let Ok(config) = get_server(server_id) else {
            return;
        };
        let key = config.get_hash(db);
        self.clients.remove(&key);
        remove_connection_from_pool(&config, db);
    }
    pub async fn get_pubsub_connection(&self, server_id: &str) -> Result<redis::aio::PubSub> {
        let config = get_server(server_id)?;
        let url = config.get_connection_url();
        let client = if let Some(certificates) = config.tls_certificates() {
            redis::Client::build_with_tls(url, certificates)
        } else {
            redis::Client::open(url)
        }?;
        let pubsub = client.get_async_pubsub().await?;
        Ok(pubsub)
    }
    /// Retrieves or creates a RedisClient for the given configuration name without caching.
    pub async fn get_client_without_cache(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        let (nodes, server_type) = self.get_redis_nodes(server_id).await?;
        debug!(server_id, server_type = ?server_type, nodes = ?nodes, "get redis nodes");
        let Some(first_node) = nodes.first() else {
            return Err(Error::Invalid {
                message: "no nodes found".to_string(),
            });
        };
        let client = match server_type {
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
                if let Some(certificates) = first_node.server.tls_certificates() {
                    builder = builder.certs(certificates);
                }
                if first_node.server.insecure.unwrap_or(false) {
                    builder = builder.danger_accept_invalid_hostnames(true);
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
        let connection = get_async_connection(&client, db, false).await?;
        let access_mode = if safe_check_user_readonly(connection.clone()).await {
            AccessMode::StrictReadOnly
        } else if config.readonly.unwrap_or(false) {
            AccessMode::SafeMode
        } else {
            AccessMode::ReadWrite
        };
        let modules = get_modules(connection.clone()).await.unwrap_or_default();
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
            version: Version::new(0, 0, 0),
            is_valkey: false,
            connection,
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
}
