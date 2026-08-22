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

//! `RedisClient` operations: scans, memory usage, value search,
//! slow logs, topology.

use super::*;

impl RedisClient {
    pub fn nodes(&self) -> (usize, usize) {
        (self.master_nodes.len(), self.nodes.len())
    }
    pub fn version(&self) -> String {
        self.version.to_string()
    }
    pub fn databases(&self) -> usize {
        self.databases
    }
    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    /// Returns the list of master node server configurations.
    pub fn master_servers(&self) -> Vec<RedisServer> {
        self.master_nodes.iter().map(|node| node.server.clone()).collect()
    }

    pub fn nodes_description(&self) -> RedisClientDescription {
        let master_nodes: Vec<String> = self.master_nodes.iter().map(|node| node.host_port()).collect();
        let slave_nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| !master_nodes.contains(&node.host_port()))
            .map(|node| node.host_port().clone())
            .collect();
        let modules = self
            .modules
            .iter()
            .map(|(name, ver)| format!("{name}@{ver}"))
            .collect::<Vec<_>>()
            .join(", ");
        let topology = self.build_topology();
        let slot_map = self.build_slot_map();
        RedisClientDescription {
            is_valkey: self.is_valkey,
            server_type: format!("{:?}", self.server_type),
            master_nodes: master_nodes.join(","),
            slave_nodes: slave_nodes.join(","),
            modules,
            topology,
            slot_map,
        }
    }

    /// Build the structured slot map consumed by Topology's slot bar and
    /// reshard planner. Only populated in cluster mode.
    fn build_slot_map(&self) -> ClusterSlotMap {
        if self.server_type != ServerType::Cluster {
            return ClusterSlotMap::default();
        }

        // Master order is the stable colour index source of truth.
        let masters: Vec<ClusterMasterSlotSummary> = self
            .master_nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, m)| {
                let id = m.cluster_id.clone()?;
                let slot_count = m
                    .slots
                    .iter()
                    .map(|(lo, hi)| u32::from(hi.saturating_sub(*lo).saturating_add(1)))
                    .sum();
                Some(ClusterMasterSlotSummary {
                    node_id: id,
                    addr: m.host_port(),
                    slot_count,
                    color_index: idx,
                })
            })
            .collect();

        let id_to_addr: HashMap<String, String> = self
            .nodes
            .iter()
            .filter_map(|n| n.cluster_id.clone().map(|id| (id, n.host_port())))
            .collect();
        let id_to_color: HashMap<String, usize> =
            masters.iter().map(|m| (m.node_id.to_string(), m.color_index)).collect();

        // Expand every owned range into ClusterSlotRange segments, then
        // merge contiguous same-owner ranges so the bar isn't 16k pieces.
        let mut flat: Vec<(u16, u16, String, String, usize)> = Vec::new();
        for m in self.master_nodes.iter() {
            let Some(id) = m.cluster_id.as_ref() else {
                continue;
            };
            let color = id_to_color.get(id).copied().unwrap_or(0);
            let addr = m.host_port();
            for &(lo, hi) in &m.slots {
                flat.push((lo, hi, id.clone(), addr.clone(), color));
            }
        }
        flat.sort_by_key(|(lo, _, _, _, _)| *lo);

        let mut owners: Vec<ClusterSlotRange> = Vec::new();
        for (lo, hi, id, addr, color) in flat {
            if let Some(last) = owners.last_mut()
                && last.node_id.as_str() == id
                && last.end.saturating_add(1) == lo
            {
                last.end = hi;
                continue;
            }
            owners.push(ClusterSlotRange {
                start: lo,
                end: hi,
                node_id: id,
                addr,
                color_index: color,
            });
        }

        let assigned_slots = owners
            .iter()
            .map(|r| u32::from(r.end.saturating_sub(r.start).saturating_add(1)))
            .sum();

        // Pair migrating (source) with importing (target) by slot.
        // Either side alone is enough to surface the in-flight slot.
        #[derive(Default)]
        struct PairSides {
            source: Option<(String, String)>,
            target: Option<(String, String)>,
        }
        let mut by_slot: HashMap<u16, PairSides> = HashMap::new();
        for node in self.nodes.iter() {
            let Some(self_id) = node.cluster_id.as_ref() else {
                continue;
            };
            let self_addr = node.host_port();
            for m in &node.migrations {
                let entry = by_slot.entry(m.slot).or_default();
                match m.kind {
                    SlotMigrationKind::Migrating => {
                        entry.source = Some((self_id.clone(), self_addr.clone()));
                        // peer is target; fill target side if still empty
                        if entry.target.is_none() {
                            let t_addr = id_to_addr.get(&m.peer_id).cloned().unwrap_or_default();
                            entry.target = Some((m.peer_id.clone(), t_addr));
                        }
                    }
                    SlotMigrationKind::Importing => {
                        entry.target = Some((self_id.clone(), self_addr.clone()));
                        if entry.source.is_none() {
                            let s_addr = id_to_addr.get(&m.peer_id).cloned().unwrap_or_default();
                            entry.source = Some((m.peer_id.clone(), s_addr));
                        }
                    }
                }
            }
        }

        let mut migrations: Vec<ClusterMigrationEntry> = by_slot
            .into_iter()
            .map(|(slot, sides)| {
                let (source_id, source_addr) = sides.source.unwrap_or_default();
                let (target_id, target_addr) = sides.target.unwrap_or_default();
                ClusterMigrationEntry {
                    slot,
                    source_id,
                    source_addr,
                    target_id,
                    target_addr,
                }
            })
            .collect();
        migrations.sort_by_key(|m| m.slot);

        ClusterSlotMap {
            owners,
            migrations,
            masters,
            assigned_slots,
        }
    }

    /// Build the structured topology consumed by the status-bar tooltip.
    ///
    /// Cluster mode groups replicas under their master via `master_cluster_id`
    /// and annotates the master with its slot ranges. Sentinel groups by
    /// `master_name`. Standalone returns an empty list so the caller falls
    /// back to its existing flat summary.
    fn build_topology(&self) -> Vec<TopologyMaster> {
        let role_marker = |role: &NodeRole| -> String {
            match role {
                NodeRole::Master => "●",
                NodeRole::Slave => "↳",
                NodeRole::Fail => "✗",
                NodeRole::Unknown => "?",
            }
            .into()
        };
        let format_slots = |slots: &[(u16, u16)]| -> String {
            if slots.is_empty() {
                return String::new();
            }
            let parts: Vec<String> = slots
                .iter()
                .map(|(lo, hi)| if lo == hi { lo.to_string() } else { format!("{lo}-{hi}") })
                .collect();
            format!("slots {}", parts.join(","))
        };

        let mut out: Vec<TopologyMaster> = Vec::new();

        match self.server_type {
            ServerType::Cluster => {
                for master in self.master_nodes.iter() {
                    let entry = TopologyEntry {
                        addr: master.host_port(),
                        role_marker: role_marker(&master.role),
                        annotation: format_slots(&master.slots),
                        node_id: master.cluster_id.clone().unwrap_or_default(),
                        master_name: String::new(),
                    };
                    let replicas: Vec<TopologyEntry> = if let Some(master_id) = master.cluster_id.as_ref() {
                        self.nodes
                            .iter()
                            .filter(|n| {
                                n.master_cluster_id.as_ref() == Some(master_id)
                                    && (n.role == NodeRole::Slave || n.role == NodeRole::Fail)
                            })
                            .map(|replica| TopologyEntry {
                                addr: replica.host_port(),
                                role_marker: role_marker(&replica.role),
                                annotation: String::new(),
                                node_id: replica.cluster_id.clone().unwrap_or_default(),
                                master_name: String::new(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Sentinel => {
                for master in self.master_nodes.iter() {
                    let label = master.master_name.as_deref().unwrap_or("");
                    // node_id is left empty in sentinel mode — CLUSTER FORGET /
                    // REPLICATE don't apply. master_name is populated only on
                    // the master row (replicas don't directly receive
                    // SENTINEL ops, which all target by master name).
                    let entry = TopologyEntry {
                        addr: master.host_port(),
                        role_marker: role_marker(&master.role),
                        annotation: if label.is_empty() {
                            String::new()
                        } else {
                            format!("({label})")
                        },
                        node_id: String::new(),
                        master_name: label.to_string(),
                    };
                    let replicas: Vec<TopologyEntry> = self
                        .nodes
                        .iter()
                        .filter(|n| n.role == NodeRole::Slave && n.master_name.as_deref() == Some(label))
                        .map(|replica| TopologyEntry {
                            addr: replica.host_port(),
                            role_marker: role_marker(&replica.role),
                            annotation: String::new(),
                            node_id: String::new(),
                            master_name: String::new(),
                        })
                        .collect();
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Standalone => {
                // No replicas surfaced from a single Standalone connection — let
                // the caller fall back to its existing summary.
            }
        }

        out
    }
    /// Returns the connection to the Redis server.
    /// # Returns
    /// * `RedisAsyncConn` - The connection to the Redis server.
    pub fn connection(&self) -> RedisAsyncConn {
        self.connection.clone()
    }

    /// Checks if the client is a cluster client.
    /// # Returns
    /// * `bool` - True if the client is a cluster client, false otherwise.
    pub fn is_cluster(&self) -> bool {
        self.server_type == ServerType::Cluster
    }
    /// Checks if the client version is at least the given version.
    /// # Arguments
    /// * `version` - The version to check.
    /// # Returns
    /// * `bool` - True if the client version is at least the given version, false otherwise.
    pub fn is_at_least_version(&self, version: &str) -> bool {
        self.version >= Version::parse(version).unwrap_or(Version::new(0, 0, 0))
    }
    pub fn supports_rejson(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "ReJSON")
    }

    /// Whether the RediSearch module is loaded on this server. The
    /// module name reported by `MODULE LIST` is the lowercase string
    /// `"search"` (true across all RediSearch versions 1.x → 2.x).
    /// Drives the visibility of the Search entry in the Tools menu.
    pub fn supports_search(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "search")
    }

    /// Unlinks keys on all master nodes concurrently.
    /// # Arguments
    /// * `keys_per_node` - A vector of vectors of keys, one for each master node.
    /// # Returns
    /// * `Result<(), Error>` - The result of the operation.
    pub async fn unlike_keys(&self, keys_per_node: Vec<Vec<String>>) -> Result<(), Error> {
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (index, keys) in keys_per_node.iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("UNLINK").arg(key.as_str());
            }
            pipes[index] = Some(pipe);
        }
        query_async_masters_pipeline(master_addrs, self.db, pipes).await?;
        Ok(())
    }

    /// Unlinks keys that may be distributed across different nodes.
    pub async fn unlike_keys_scattered(&self, keys: Vec<String>) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.is_cluster() {
            let mut conn = self.connection();
            let mut pipe = redis::pipe();
            for key in &keys {
                pipe.cmd("UNLINK").arg(key.as_str());
            }
            let _: () = pipe.query_async(&mut conn).await?;
            return Ok(());
        }
        let conn = self.connection();
        for chunk in keys.chunks(1000) {
            let futures = chunk.iter().map(|key| {
                let mut conn_clone = conn.clone();
                async move {
                    let _: () = cmd("UNLINK").arg(key.as_str()).query_async(&mut conn_clone).await?;
                    Ok::<(), Error>(())
                }
            });
            let _: Vec<()> = try_join_all(futures).await?;
        }
        Ok(())
    }

    /// Apply `EXPIRE` (`ttl_secs = Some`) or `PERSIST` (`None`) to many keys,
    /// cluster-safe. Mirrors [`Self::unlike_keys_scattered`]: one pipeline on a
    /// standalone, per-key concurrent commands (chunked) on a cluster so no
    /// cross-slot pipeline is ever built.
    pub async fn set_ttl_keys_scattered(&self, keys: Vec<String>, ttl_secs: Option<u64>) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.is_cluster() {
            let mut conn = self.connection();
            let mut pipe = redis::pipe();
            for key in &keys {
                match ttl_secs {
                    Some(secs) => pipe.cmd("EXPIRE").arg(key.as_str()).arg(secs),
                    None => pipe.cmd("PERSIST").arg(key.as_str()),
                };
            }
            let _: Vec<i64> = pipe.query_async(&mut conn).await?;
            return Ok(());
        }
        let conn = self.connection();
        for chunk in keys.chunks(1000) {
            let futures = chunk.iter().map(|key| {
                let mut conn_clone = conn.clone();
                let key = key.clone();
                async move {
                    match ttl_secs {
                        Some(secs) => {
                            let _: i64 = cmd("EXPIRE")
                                .arg(key.as_str())
                                .arg(secs)
                                .query_async(&mut conn_clone)
                                .await?;
                        }
                        None => {
                            let _: i64 = cmd("PERSIST").arg(key.as_str()).query_async(&mut conn_clone).await?;
                        }
                    }
                    Ok::<(), Error>(())
                }
            });
            let _: Vec<()> = try_join_all(futures).await?;
        }
        Ok(())
    }

    /// Returns the memory usage of a key.
    /// # Arguments
    /// * `key` - The key to get the memory usage of.
    /// * `key_type` - The type of the key.
    /// # Returns
    /// * `Result<u64>` - The memory usage of the key.
    pub async fn memory_usage(&self, key: &str, key_type: &str) -> Result<u64> {
        let mut conn = self.connection.clone();
        let key_type = key_type.to_lowercase();

        if self.is_at_least_version("4.0.0") {
            let memory_usage: u64 = cmd("MEMORY").arg("USAGE").arg(key).query_async(&mut conn).await?;
            return Ok(memory_usage);
        }

        if key_type == "str" {
            let len: u64 = cmd("STRLEN").arg(key).query_async(&mut conn).await?;
            return Ok(len + 56);
        }

        let total_count: u64 = match key_type.as_str() {
            "list" => cmd("LLEN").arg(key).query_async(&mut conn).await?,
            "hash" => cmd("HLEN").arg(key).query_async(&mut conn).await?,
            "set" => cmd("SCARD").arg(key).query_async(&mut conn).await?,
            "zset" => cmd("ZCARD").arg(key).query_async(&mut conn).await?,
            _ => 0,
        };

        if total_count == 0 {
            return Ok(0);
        }

        if total_count < 1000 {
            let data: Vec<u8> = cmd("DUMP").arg(key).query_async(&mut conn).await?;
            return Ok(data.len() as u64);
        }

        let sample_count = 100;

        let (sample_bytes, actual_count) = match key_type.as_str() {
            "list" => {
                let items: Vec<Vec<u8>> = cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(sample_count - 1)
                    .query_async(&mut conn)
                    .await?;
                let len: usize = items.iter().map(|x| x.len()).sum();
                (len, items.len())
            }
            "set" => {
                let items: Vec<Vec<u8>> = cmd("SRANDMEMBER")
                    .arg(key)
                    .arg(sample_count)
                    .query_async(&mut conn)
                    .await?;
                let len: usize = items.iter().map(|x| x.len()).sum();
                (len, items.len())
            }
            "zset" => {
                let items: Vec<Vec<u8>> = cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(sample_count - 1)
                    .query_async(&mut conn)
                    .await?;

                let members_len: usize = items.iter().map(|x| x.len()).sum();
                // ZSET score (double, 8 bytes)
                let scores_len = items.len() * 8;
                (members_len + scores_len, items.len())
            }
            "hash" => {
                let (_, items): HashScanValue = cmd("HSCAN")
                    .arg(key)
                    .arg(0)
                    .arg("COUNT")
                    .arg(sample_count)
                    .query_async(&mut conn)
                    .await?;

                let len: usize = items.iter().map(|(k, v)| k.len() + v.len()).sum();
                (len, items.len())
            }
            _ => (0, 0),
        };

        if actual_count == 0 {
            return Ok(0);
        }

        let avg_len = sample_bytes as u64 / actual_count as u64;

        let overhead = match key_type.as_str() {
            "zset" => 64,
            "hash" | "set" => 32,
            _ => 16,
        };

        Ok(total_count * (avg_len + overhead))
    }

    /// Returns the slow logs of the Redis server, optionally filtered by timestamp.
    ///
    /// # Returns
    /// * `Vec<SlowLogEntry>` - A vector of slow log entries.
    pub async fn get_slow_logs(&self) -> Result<Vec<SlowLogEntry>> {
        let (_, logs_arr): (_, Vec<Vec<SlowLogEntry>>) = self
            .query_async_masters(vec![cmd("SLOWLOG").arg("GET").clone()])
            .await?;

        let mut logs: Vec<SlowLogEntry> = logs_arr.into_iter().flatten().collect();
        logs.sort_unstable_by_key(|entry| Reverse(entry.timestamp));

        Ok(logs)
    }
    /// Runs `SLOWLOG RESET` on every master node, clearing the recorded
    /// slow-log entries cluster-wide.
    pub async fn slowlog_reset(&self) -> Result<()> {
        let (_, _statuses): (_, Vec<String>) = self
            .query_async_masters(vec![cmd("SLOWLOG").arg("RESET").clone()])
            .await?;
        Ok(())
    }
    /// `FLUSHDB` — drop every key in the connection's current database.
    ///
    /// Fanned out to every master: on a cluster each master owns its own
    /// slice of the keyspace, so a single-node FLUSHDB would leave the
    /// other shards untouched.
    pub async fn flush_db(&self) -> Result<()> {
        let (_, _statuses): (_, Vec<String>) = self.query_async_masters(vec![cmd("FLUSHDB")]).await?;
        Ok(())
    }
    /// `FLUSHALL` — drop every key in every database on the instance.
    /// Fanned out across masters for the same reason as [`Self::flush_db`].
    pub async fn flush_all(&self) -> Result<()> {
        let (_, _statuses): (_, Vec<String>) = self.query_async_masters(vec![cmd("FLUSHALL")]).await?;
        Ok(())
    }
    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<T>` - A vector of results from the commands.
    pub async fn query_async_masters<T: FromRedisValue>(&self, cmds: Vec<Cmd>) -> Result<(Vec<RedisServer>, Vec<T>)> {
        let Some(first) = cmds.first() else {
            return Err(Error::Invalid {
                message: "Commands are empty".to_string(),
            });
        };
        let addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut new_cmds = vec![Some(first.clone()); addrs.len()];
        for (index, cmd) in cmds.iter().enumerate() {
            new_cmds[index] = Some(cmd.clone());
        }
        let values = query_async_masters(&addrs, self.db, new_cmds).await?;
        let values: Vec<T> = values.into_iter().flatten().collect();
        Ok((addrs, values))
    }
    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<Option<T>>` - A vector of results from the commands.
    pub async fn query_async_masters_with_option<T: FromRedisValue>(
        &self,
        cmds: Vec<Option<Cmd>>,
    ) -> Result<Vec<Option<T>>> {
        let addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let values = query_async_masters(&addrs, self.db, cmds).await?;
        Ok(values)
    }
    /// Calculates the total DB size across all masters.
    /// # Returns
    /// * `u64` - The total DB size.
    pub async fn dbsize(&self) -> Result<u64> {
        let (_, list): (_, Vec<u64>) = self.query_async_masters(vec![cmd("DBSIZE")]).await?;
        Ok(list.iter().sum())
    }
    /// Pings the server to check connectivity.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.connection.clone();
        let _: () = cmd("PING").query_async(&mut conn).await?;
        Ok(())
    }
    /// Returns the number of master nodes.
    /// # Returns
    /// * `usize` - The number of master nodes.
    pub fn count_masters(&self) -> Result<usize> {
        Ok(self.master_nodes.len())
    }
    /// Samples a subset of keys from the Redis server.
    /// # Arguments
    /// * `ratio` - The ratio of keys to sample.
    /// * `count` - The count of keys to sample.
    /// * `cursors` - The cursors to continue the scan from.
    /// # Returns
    /// * `(Vec<u64>, Vec<KeyMemoryUsage>)` - A tuple containing the new cursors and the key memory usage.
    pub async fn sample_scan_memory_usage(
        &self,
        ratio: f32,
        count: u64,
        cursors: Option<Vec<u64>>,
        heat: HeatProbe,
    ) -> Result<(u64, Vec<u64>, Vec<KeyMemoryUsage>)> {
        let pattern = "*";
        let (cursors, mut keys_per_node) = self.scan_nodes(cursors, pattern, count, None).await?;

        let total_count: usize = keys_per_node.iter().map(|keys| keys.len()).sum();

        if ratio < 1.0 {
            let mut rng = rand::rng();
            for keys in keys_per_node.iter_mut() {
                keys.retain(|_| rng.random::<f32>() < ratio);
            }
        }
        let capacity = keys_per_node.iter().map(|keys| keys.len()).sum();
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        let heat_subcommand = heat.redis_subcommand();
        let cmds_per_key: usize = if heat_subcommand.is_some() { 4 } else { 3 };
        for (index, keys) in keys_per_node.iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("TYPE")
                    .arg(key.as_str())
                    .cmd("MEMORY")
                    .arg("USAGE")
                    .arg(key.as_str())
                    .arg("SAMPLES")
                    .arg("5")
                    .cmd("TTL")
                    .arg(key.as_str());
                if let Some(sub) = heat_subcommand {
                    // OBJECT FREQ / OBJECT IDLETIME — both O(1).
                    pipe.cmd("OBJECT").arg(sub).arg(key.as_str());
                }
            }
            pipes[index] = Some(pipe);
        }

        let results_per_node = query_async_masters_pipeline(master_addrs, self.db, pipes).await?;

        let mut keys_memory_usage = Vec::with_capacity(capacity);
        for (index, results) in results_per_node.into_iter().enumerate() {
            let Some(results) = results else {
                continue;
            };
            let keys = &keys_per_node[index];
            for (i, chunk) in results.chunks_exact(cmds_per_key).enumerate() {
                if i >= keys.len() {
                    break;
                }
                let key = &keys[i];

                let key_type = match &chunk[0] {
                    Value::SimpleString(s) => s.clone(),
                    Value::BulkString(d) => String::from_utf8_lossy(d).to_string(),
                    _ => "unknown".to_string(),
                };

                let memory: u64 = match &chunk[1] {
                    Value::Int(m) => *m as u64,
                    _ => 0,
                };

                let ttl: i64 = match &chunk[2] {
                    Value::Int(t) => *t,
                    _ => -2,
                };
                if ttl == -2 {
                    continue;
                }

                // OBJECT FREQ/IDLETIME may legitimately error per-key
                // (key vanished between SCAN and pipeline execution, or
                // policy mismatch on an older Redis). Treat any non-Int
                // result as "unknown heat" rather than failing the batch.
                let heat = match (heat, chunk.get(3)) {
                    (HeatProbe::Freq, Some(Value::Int(v))) => HeatMetric::Freq((*v).max(0) as u64),
                    (HeatProbe::IdleTime, Some(Value::Int(v))) => HeatMetric::IdleTime((*v).max(0) as u64),
                    _ => HeatMetric::None,
                };

                keys_memory_usage.push(KeyMemoryUsage {
                    key: key.clone(),
                    memory_usage: memory,
                    key_type,
                    ttl,
                    heat,
                });
            }
        }

        Ok((total_count as u64, cursors, keys_memory_usage))
    }

    /// Returns the active `maxmemory-policy` setting, or an empty string if
    /// the command fails (NOPERM, restricted environment). The caller should
    /// treat empty as "unknown" and skip heat probing.
    pub async fn maxmemory_policy(&self) -> Result<String> {
        let mut conn = self.connection.clone();
        let res: redis::RedisResult<HashMap<String, String>> = cmd("CONFIG")
            .arg("GET")
            .arg("maxmemory-policy")
            .query_async(&mut conn)
            .await;
        match res {
            Ok(map) => Ok(map.get("maxmemory-policy").cloned().unwrap_or_default()),
            Err(_) => Ok(String::new()),
        }
    }

    /// `TYPE` for a single key on the shared connection — answers "none"
    /// when the key doesn't exist, which doubles as an existence probe
    /// (used by the multi-database exact lookup).
    pub async fn key_type(&self, key: &str) -> Result<String> {
        let mut conn = self.connection.clone();
        let key_type: String = cmd("TYPE").arg(key).query_async(&mut conn).await?;
        Ok(key_type)
    }

    /// Fetch `INFO commandstats`, aggregated across all master nodes on a
    /// cluster (where the reply is a per-node map) or the single node
    /// otherwise. Returns one [`CommandStat`] per command.
    pub async fn command_stats(&self) -> Result<Vec<CommandStat>> {
        let mut conn = self.connection.clone();
        let info: Value = cmd("INFO").arg("commandstats").query_async(&mut conn).await?;
        let mut agg: HashMap<String, (u64, u64)> = HashMap::new();
        match info {
            Value::Map(items) => {
                for (_, node_val) in items {
                    if let Ok(text) = String::from_redis_value(node_val) {
                        aggregate_command_stats(&text, &mut agg);
                    }
                }
            }
            other => {
                if let Ok(text) = String::from_redis_value(other) {
                    aggregate_command_stats(&text, &mut agg);
                }
            }
        }
        Ok(agg
            .into_iter()
            .map(|(name, (calls, usec))| CommandStat { name, calls, usec })
            .collect())
    }

    /// One bounded round of **search-by-value**: SCAN a single page across all
    /// masters, then for each key read its value (size-gated) and
    /// case-insensitively substring-match it against `needle_lower` (which must
    /// already be lowercased). Unsupported types are ignored; values/containers
    /// larger than the caps are skipped (counted in `skipped_oversized`).
    ///
    /// The caller drives this in a cancellable loop, accumulating across pages
    /// until the keyspace is exhausted (`done`) or a scan/time budget trips —
    /// results are an explicit **sample**, never guaranteed exhaustive.
    ///
    /// Reads are batched into three pipelines per master (TYPE → length gate →
    /// value fetch), so a page costs ~3 round trips per master instead of 2–3
    /// per *key* — the per-key serial version burned the whole time budget on
    /// network latency before touching most of the page. A key that vanishes
    /// mid-round types as "none" (dropped) or reads as empty (no match); a
    /// mid-round *type change* can fail the round's pipeline, which surfaces
    /// as this round's error — same contract as the key-tree `scan`.
    pub async fn scan_values_round(
        &self,
        pattern: &str,
        needle_lower: &str,
        max_value_bytes: u64,
        max_container_elems: u64,
        cursors: Option<Vec<u64>>,
        page_count: u64,
    ) -> Result<ValueSearchRound> {
        let (cursors, keys_per_node) = self.scan_nodes(cursors, pattern, page_count, None).await?;
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        // Every per-node collection below is indexed/zipped against the
        // master list; `scan_nodes` output is aligned with it by
        // construction, so a mismatch means stale cursors from a different
        // topology — refuse loudly instead of misrouting pipelines.
        if keys_per_node.len() != master_addrs.len() {
            return Err(Error::Invalid {
                message: "scan pages and master addresses length mismatch".to_string(),
            });
        }

        let mut scanned = 0usize;
        let mut skipped_oversized = 0usize;

        // Phase 1 — resolve every key's type, one pipeline per master.
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (idx, keys) in keys_per_node.iter().enumerate() {
            scanned += keys.len();
            if keys.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("TYPE").arg(key.as_str());
            }
            pipes[idx] = Some(pipe);
        }
        let type_results = query_async_masters_pipeline(master_addrs.clone(), self.db, pipes).await?;

        let redis_string = |val: &Value| match val {
            Value::SimpleString(s) => s.clone(),
            Value::BulkString(d) => String::from_utf8_lossy(d).into_owned(),
            _ => String::new(),
        };

        // Keep the per-node grouping throughout so follow-up pipelines stay
        // routed to the master that owns the keys. Streams and module types
        // aren't searched.
        //
        // Invariant for all three phase-result zips below: `None` means "no
        // pipeline was submitted for this node", which by construction only
        // happens when that node's input list is empty — node *failures*
        // (connect, timeout, server error) fail the whole
        // `query_async_masters_pipeline` call instead of yielding `None`,
        // so an `if let Some` here never silently drops real keys. The
        // debug_asserts pin that contract against future changes.
        let mut candidates_per_node: Vec<Vec<(String, String)>> = Vec::with_capacity(keys_per_node.len());
        for (keys, types) in keys_per_node.into_iter().zip(type_results) {
            debug_assert_eq!(types.is_some(), !keys.is_empty());
            let mut node_candidates = Vec::with_capacity(keys.len());
            if let Some(types) = types {
                for (key, type_val) in keys.into_iter().zip(types) {
                    let key_type = redis_string(&type_val);
                    if matches!(key_type.as_str(), "string" | "hash" | "list" | "set" | "zset") {
                        node_candidates.push((key, key_type));
                    }
                }
            }
            candidates_per_node.push(node_candidates);
        }

        // Phase 2 — size gate: one length-probe pipeline per master.
        // Containers are gated on element count so a giant collection isn't
        // pulled whole.
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (idx, candidates) in candidates_per_node.iter().enumerate() {
            if candidates.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for (key, key_type) in candidates {
                let len_cmd = match key_type.as_str() {
                    "string" => "STRLEN",
                    "hash" => "HLEN",
                    "list" => "LLEN",
                    "set" => "SCARD",
                    _ => "ZCARD",
                };
                pipe.cmd(len_cmd).arg(key.as_str());
            }
            pipes[idx] = Some(pipe);
        }
        let len_results = query_async_masters_pipeline(master_addrs.clone(), self.db, pipes).await?;

        let mut survivors_per_node: Vec<Vec<(String, String)>> = Vec::with_capacity(candidates_per_node.len());
        for (candidates, lens) in candidates_per_node.into_iter().zip(len_results) {
            debug_assert_eq!(lens.is_some(), !candidates.is_empty());
            let mut node_survivors = Vec::with_capacity(candidates.len());
            if let Some(lens) = lens {
                for ((key, key_type), len_val) in candidates.into_iter().zip(lens) {
                    let len = match len_val {
                        Value::Int(n) => n.max(0) as u64,
                        // A vanished key answers 0 — keep it; its read below
                        // comes back empty and simply doesn't match.
                        _ => 0,
                    };
                    let cap = if key_type == "string" {
                        max_value_bytes
                    } else {
                        max_container_elems
                    };
                    if len > cap {
                        skipped_oversized += 1;
                    } else {
                        node_survivors.push((key, key_type));
                    }
                }
            }
            survivors_per_node.push(node_survivors);
        }

        // Phase 3 — value reads for the keys that passed the gate.
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (idx, survivors) in survivors_per_node.iter().enumerate() {
            if survivors.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for (key, key_type) in survivors {
                match key_type.as_str() {
                    "string" => pipe.cmd("GET").arg(key.as_str()),
                    "hash" => pipe.cmd("HGETALL").arg(key.as_str()),
                    "list" => pipe.cmd("LRANGE").arg(key.as_str()).arg(0).arg(-1),
                    "set" => pipe.cmd("SMEMBERS").arg(key.as_str()),
                    _ => pipe.cmd("ZRANGE").arg(key.as_str()).arg(0).arg(-1),
                };
            }
            pipes[idx] = Some(pipe);
        }
        let value_results = query_async_masters_pipeline(master_addrs, self.db, pipes).await?;

        // Match evaluation is pure CPU from here. Only the first match per
        // key is recorded (one row per key); the inline preview shows the
        // full value. Conversion failures read as empty — "no match" —
        // mirroring the old per-key `unwrap_or_default` semantics.
        let mut matches = Vec::new();
        for (survivors, values) in survivors_per_node.into_iter().zip(value_results) {
            debug_assert_eq!(values.is_some(), !survivors.is_empty());
            let Some(values) = values else {
                continue;
            };
            for ((key, key_type), value) in survivors.into_iter().zip(values) {
                let location = match key_type.as_str() {
                    "string" => {
                        let value: Vec<u8> = Vec::from_redis_value(value).unwrap_or_default();
                        contains_needle(&value, needle_lower).then_some(MatchLocation::Value)
                    }
                    "hash" => {
                        let fields: Vec<(Vec<u8>, Vec<u8>)> = Vec::from_redis_value(value).unwrap_or_default();
                        fields
                            .into_iter()
                            .find(|(f, v)| contains_needle(f, needle_lower) || contains_needle(v, needle_lower))
                            .map(|(f, _)| MatchLocation::Field(truncate_member(&f)))
                    }
                    "list" => {
                        let items: Vec<Vec<u8>> = Vec::from_redis_value(value).unwrap_or_default();
                        items
                            .into_iter()
                            .position(|e| contains_needle(&e, needle_lower))
                            .map(MatchLocation::Index)
                    }
                    // set / zset
                    _ => {
                        let members: Vec<Vec<u8>> = Vec::from_redis_value(value).unwrap_or_default();
                        members
                            .into_iter()
                            .find(|m| contains_needle(m, needle_lower))
                            .map(|m| MatchLocation::Member(truncate_member(&m)))
                    }
                };
                if let Some(location) = location {
                    matches.push(ValueMatch {
                        key,
                        key_type,
                        location,
                    });
                }
            }
        }

        let done = cursors.iter().all(|&c| c == 0);
        Ok(ValueSearchRound {
            cursors,
            matches,
            scanned,
            skipped_oversized,
            done,
        })
    }

    /// Build a bounded, type-aware text preview of a key's value (for the
    /// value-search preview pane). Containers are sampled to ~200 elements.
    pub async fn get_value_preview(&self, key: &str) -> Result<String> {
        let mut conn = self.connection.clone();
        let key_type: String = cmd("TYPE").arg(key).query_async(&mut conn).await?;
        const N: isize = 200;
        let text = match key_type.as_str() {
            "string" => {
                let v: Vec<u8> = cmd("GET").arg(key).query_async(&mut conn).await?;
                String::from_utf8_lossy(&v).into_owned()
            }
            "hash" => {
                let fields: Vec<(Vec<u8>, Vec<u8>)> = cmd("HGETALL").arg(key).query_async(&mut conn).await?;
                fields
                    .iter()
                    .take(N as usize)
                    .map(|(f, v)| format!("{}: {}", String::from_utf8_lossy(f), String::from_utf8_lossy(v)))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "list" => {
                let items: Vec<Vec<u8>> = cmd("LRANGE").arg(key).arg(0).arg(N - 1).query_async(&mut conn).await?;
                items
                    .iter()
                    .map(|e| String::from_utf8_lossy(e).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "set" => {
                let members: Vec<Vec<u8>> = cmd("SRANDMEMBER").arg(key).arg(N).query_async(&mut conn).await?;
                members
                    .iter()
                    .map(|m| String::from_utf8_lossy(m).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "zset" => {
                let members: Vec<(Vec<u8>, f64)> = cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(N - 1)
                    .arg("WITHSCORES")
                    .query_async(&mut conn)
                    .await?;
                members
                    .iter()
                    .map(|(m, s)| format!("{} ({s})", String::from_utf8_lossy(m)))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            other => format!("(type: {other} — open in editor to view)"),
        };
        Ok(text)
    }

    /// Fetch a key's raw value bytes via `GET` for the cross-server diff
    /// (empty when the key is absent). A wrong-type key surfaces an error.
    pub async fn get_key_bytes(&self, key: &str) -> Result<Vec<u8>> {
        let mut conn = self.connection.clone();
        let value: Option<Vec<u8>> = cmd("GET").arg(key).query_async(&mut conn).await?;
        Ok(value.unwrap_or_default())
    }

    /// Initiates a SCAN operation across all masters.
    /// # Arguments
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<String>)` - A tuple containing the new cursors and the keys.
    pub async fn first_scan(
        &self,
        pattern: &str,
        count: u64,
        with_ttl: bool,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<(String, String, i64)>)> {
        let (cursors, keys) = self.scan(None, pattern, count, with_ttl, type_filter).await?;
        Ok((cursors, keys))
    }
    pub async fn scan_nodes(
        &self,
        cursors: Option<Vec<u64>>,
        pattern: &str,
        count: u64,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<Vec<String>>)> {
        debug!("scan, cursors: {cursors:?}, pattern: {pattern}, count: {count}");
        let mut first_scan = false;
        let cur = if let Some(cursors) = cursors {
            cursors
        } else {
            first_scan = true;
            let master_count = self.count_masters()?;
            vec![0; master_count]
        };

        if !first_scan && cur.iter().all(|&c| c == 0) {
            return Ok((cur.clone(), vec![vec![]; cur.len()]));
        }

        // Build one Option<Cmd> per master: Some for active nodes, None for completed ones.
        let cmds: Vec<Option<Cmd>> = cur
            .iter()
            .map(|&cursor| {
                if first_scan || cursor != 0 {
                    let mut c = cmd("SCAN");
                    c.cursor_arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(count);
                    // Redis 6.0+ server-side type filter (gated by the caller).
                    if let Some(t) = type_filter {
                        c.arg("TYPE").arg(t);
                    }
                    Some(c)
                } else {
                    None
                }
            })
            .collect();

        let values: Vec<Option<(u64, Vec<Vec<u8>>)>> = self.query_async_masters_with_option(cmds).await?;

        let mut next_cursors = cur;
        let mut keys_per_node: Vec<Vec<String>> = vec![vec![]; next_cursors.len()];

        for (idx, result) in values.into_iter().enumerate() {
            if let Some((new_cursor, keys_in_node)) = result {
                next_cursors[idx] = new_cursor;
                let mut node_keys = Vec::with_capacity(keys_in_node.len());
                for k in keys_in_node {
                    node_keys.push(String::from_utf8_lossy(&k).into_owned());
                }
                keys_per_node[idx] = node_keys;
            }
        }

        Ok((next_cursors, keys_per_node))
    }
    /// Continues a SCAN operation.
    /// # Arguments
    /// * `cursors` - A vector of cursors for each master.
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<String>)` - A tuple containing the new cursors and the keys.
    pub async fn scan(
        &self,
        cursors: Option<Vec<u64>>,
        pattern: &str,
        count: u64,
        with_ttl: bool,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<(String, String, i64)>)> {
        // Server-side TYPE filter on Redis 6.0+; the client-side `retain` below
        // covers older servers (the per-key TYPE is fetched regardless). TYPE
        // filters within each COUNT batch, so a sparse type just needs more
        // rounds — the caller's paging loop handles that.
        let server_type = if type_filter.is_some() && self.is_at_least_version("6.0.0") {
            type_filter
        } else {
            None
        };
        let (new_cursors, keys_per_node) = self.scan_nodes(cursors, pattern, count, server_type).await?;

        // Pipeline TYPE (+ optional TTL) per key in one RTT per master.
        // TTL is skipped entirely when the caller doesn't need it — saves
        // one command per key on large dbs where the chip is disabled.
        let cmds_per_key = if with_ttl { 2 } else { 1 };
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (idx, keys) in keys_per_node.iter().enumerate() {
            if !keys.is_empty() {
                let mut pipe = redis::pipe();
                for key in keys {
                    pipe.cmd("TYPE").arg(key.as_str());
                    if with_ttl {
                        pipe.cmd("TTL").arg(key.as_str());
                    }
                }
                pipes[idx] = Some(pipe);
            }
        }

        let pipe_results = query_async_masters_pipeline(master_addrs, self.db, pipes).await?;

        let capacity: usize = keys_per_node.iter().map(|ks| ks.len()).sum();
        let mut all_keys = Vec::with_capacity(capacity);
        for (idx, keys) in keys_per_node.into_iter().enumerate() {
            let results = pipe_results.get(idx).and_then(|r| r.as_ref());
            for (i, key) in keys.into_iter().enumerate() {
                let type_val = results.and_then(|vals| vals.get(i * cmds_per_key));
                let key_type = type_val
                    .map(|val| match val {
                        Value::SimpleString(s) => s.clone(),
                        Value::BulkString(d) => String::from_utf8_lossy(d).into_owned(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let ttl_secs: i64 = if with_ttl {
                    match results.and_then(|vals| vals.get(i * cmds_per_key + 1)) {
                        Some(Value::Int(t)) => *t,
                        _ => -2,
                    }
                } else {
                    // Sentinel: TTL was not fetched. Renderer treats -2 as
                    // "no chip" so it's safe to use it here too.
                    -2
                };
                all_keys.push((key, key_type, ttl_secs));
            }
        }
        // Always filter client-side too: covers Redis < 6.0 (no server TYPE)
        // and is a no-op when the server already filtered.
        if let Some(t) = type_filter {
            all_keys.retain(|(_, key_type, _)| key_type.as_str() == t);
        }
        Ok((new_cursors, all_keys))
    }
}
