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

//! `RedisClient` operations: what the panels read of the server and of one
//! key (memory usage, the slow log, hot keys, command and slot statistics,
//! `INFO keysizes`, a value's preview), the heartbeat's probe and the
//! flushes — beside `describe` (what the client is), `fan_out` (every
//! master at once) and `scan`.

use super::*;
#[cfg(target_family = "wasm")]
use crate::bridge::BridgeConn;
#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::floors::{self, Floor};
use crate::hotkeys::HotkeysReport;
use crate::keyspace::expire_seconds;
use crate::reply;
use crate::slot_stats::{SlotStatMetric, SlotStatRow, parse_slot_stats};
use redis::Pipeline;
use zedis_core::keysizes::{KeysizesDist, merge_keysizes, parse_keysizes};
use zedis_core::string::split_label;

mod describe;
mod fan_out;
mod scan;

impl RedisClient {
    /// This client's connection carrying `token` as the confirmation on
    /// every bridge request (`ServerDb::confirmed`); nothing on a socket.
    pub(crate) fn with_confirmation(mut self, token: Option<String>) -> Self {
        self.connection = self.connection.with_confirmation(token);
        self
    }

    /// Returns the connection to the Redis server.
    /// # Returns
    /// * `RedisAsyncConn` - The connection to the Redis server.
    pub fn connection(&self) -> RedisAsyncConn {
        self.connection.clone()
    }

    /// A second, uncached connection built from the same topology as
    /// [`connection`](Self::connection) — see
    /// [`ConnectionManager::open_dedicated_connection`].
    ///
    /// Native only: it dials, or fans a *pipeline* out to every master —
    /// neither of which the browser does. The bridge owns both (ADR 9).
    #[cfg(not(target_family = "wasm"))]
    pub async fn open_dedicated_connection(&self) -> Result<RedisAsyncConn> {
        get_async_connection(&self.client, self.db, false).await
    }

    /// The one node this client talks to — the entry itself, or the master
    /// its Sentinel named — for a connection that is not redis-rs's own
    /// (`RawConnection`). `None` on a cluster, which is many nodes.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn single_node_config(&self) -> Option<RedisServer> {
        match &self.client {
            RClient::Single(config) => Some(config.as_ref().clone()),
            RClient::Cluster(_) | RClient::SshCluster(_) => None,
        }
    }

    /// The browser's dedicated connection: a bridge *session*, which is the
    /// same promise over HTTP — one backend connection, this caller's alone,
    /// so connection-scoped state stays where it was typed (ADR 4).
    #[cfg(target_family = "wasm")]
    pub async fn open_dedicated_connection(&self) -> Result<RedisAsyncConn> {
        let Some(conn) = self.connection.as_bridge() else {
            return Err(Error::Invalid {
                message: "not a bridge connection".to_string(),
            });
        };
        let transport = conn.transport();
        let session = transport
            .open_session(conn.server_id().to_string(), self.db)
            .await
            .map_err(|e| Error::Invalid { message: e.to_string() })?;
        Ok(RedisAsyncConn::Bridge(
            BridgeConn::new(transport, conn.server_id(), self.db).with_session(session),
        ))
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

        if self.supports(floors::MEMORY_USAGE) {
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

    /// Returns the slow logs of the Redis server, newest first.
    ///
    /// The count is explicit: `SLOWLOG GET` on its own answers ten entries,
    /// which is not the log (see [`MAX_COMMAND_LOG_ENTRIES`]).
    ///
    /// # Returns
    /// * `Vec<SlowLogEntry>` - A vector of slow log entries.
    pub async fn get_slow_logs(&self) -> Result<Vec<SlowLogEntry>> {
        let (_, logs_arr): (_, Vec<Vec<SlowLogEntry>>) = self
            .query_async_masters(vec![cmd("SLOWLOG").arg("GET").arg(MAX_COMMAND_LOG_ENTRIES).clone()])
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
    /// `DBSIZE` on the pooled connection. A standalone (or the Sentinel
    /// master) answers directly; on a cluster redis-rs fans the command out
    /// to every master of its *live* slot map and sums the replies
    /// (`ResponsePolicy::Aggregate(Sum)`). Not `query_async_masters`: that
    /// walks the node list captured at connect, which a failover or reshard
    /// leaves stale until the client is rebuilt — a demoted master still on
    /// it answers with its replica's count, so one shard is summed twice
    /// while `SCAN` (deduplicated by key) is not, and the status bar's total
    /// sits above what a completed scan found.
    pub async fn dbsize(&self) -> Result<u64> {
        let mut conn = self.connection.clone();
        Ok(cmd("DBSIZE").query_async(&mut conn).await?)
    }
    /// The heartbeat's liveness probe: `Some(info)` when the probe was itself
    /// the `INFO` the beat is after, `None` when it was a `PING` and the
    /// caller still has to fan the `INFO` out ([`Self::query_async_masters`]).
    /// The caller times this call — that is the latency the status bar shows.
    ///
    /// The probe has to travel on `self.connection`, the connection every
    /// user command uses. The `INFO` fan-out does not: it dials each master
    /// through the per-node connection cache, so it says nothing about
    /// whether the *client's* link is alive — which is what a dropped link,
    /// a failover or a NAT timeout breaks, and what the heartbeat exists to
    /// notice (ADR 5). That is why a `PING` used to precede it.
    ///
    /// With one master that `PING` is a wasted command: `INFO` sent on
    /// `self.connection` is the probe *and* the data, so a standalone or a
    /// Sentinel-managed master costs one command a beat instead of two — on
    /// a server billed per command, half the bill of an open window. The
    /// measured latency then includes the reply's transfer (a few KB), which
    /// is noise next to the round trip on anything but a very slow link.
    ///
    /// A cluster keeps the `PING`: its client connection routes a bare
    /// `INFO` to one arbitrary node, the per-master replies still need the
    /// fan-out, and saving one command in N+1 is not worth a weaker probe.
    /// Its cost is handled by beating less often instead.
    pub async fn heartbeat_probe(&self) -> Result<Option<String>> {
        if self.master_nodes.len() == 1 && !self.is_cluster() && !self.info_unavailable {
            let mut conn = self.connection.clone();
            let info: String = cmd("INFO").query_async(&mut conn).await?;
            return Ok(Some(info));
        }
        self.ping().await?;
        Ok(None)
    }
    /// Pings the server to check connectivity.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.connection.clone();
        let _: () = cmd("PING").query_async(&mut conn).await?;
        Ok(())
    }
    /// `OBJECT ENCODING` plus the one heat metric this server's eviction
    /// policy makes meaningful, for a single key — what the key editor's
    /// header chip shows.
    ///
    /// Best effort in both directions, because this only decorates a value
    /// that already loaded: a server without `OBJECT` (a proxy, a managed
    /// cloud, a `NOPERM` user) and a key that expired between the load and
    /// this call both answer "nothing to show" instead of failing. Both
    /// subcommands are O(1) — the object header already carries them.
    pub async fn object_meta(&self, key: &str, with_encoding: bool, heat: HeatProbe) -> (String, HeatMetric) {
        let heat_subcommand = heat.redis_subcommand();
        if !with_encoding && heat_subcommand.is_none() {
            return (String::new(), HeatMetric::None);
        }
        let mut pipe = redis::pipe();
        if with_encoding {
            pipe.cmd("OBJECT").arg("ENCODING").arg(key);
        }
        if let Some(sub) = heat_subcommand {
            pipe.cmd("OBJECT").arg(sub).arg(key);
        }
        let mut conn = self.connection.clone();
        // `Vec<Value>` keeps a per-command failure as `Value::ServerError`
        // rather than failing the batch, which is exactly the tolerance a
        // vanished key needs.
        let Ok(values): redis::RedisResult<Vec<Value>> = pipe.query_async(&mut conn).await else {
            return (String::new(), HeatMetric::None);
        };
        let mut values = values.into_iter();
        let encoding = if with_encoding {
            match values.next() {
                Some(Value::SimpleString(s)) => s,
                Some(Value::BulkString(d)) => String::from_utf8_lossy(&d).to_string(),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let heat = match (heat, values.next()) {
            (HeatProbe::Freq, Some(Value::Int(v))) => HeatMetric::Freq(v.max(0) as u64),
            (HeatProbe::IdleTime, Some(Value::Int(v))) => HeatMetric::IdleTime(v.max(0) as u64),
            _ => HeatMetric::None,
        };
        (encoding, heat)
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
        let mut stats: Vec<CommandStat> = agg
            .into_iter()
            .map(|(name, (calls, usec))| CommandStat { name, calls, usec })
            .collect();
        // In name order: a hash map's order is new with every map, so the
        // same server answered in a different order on every call (#199).
        stats.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(stats)
    }

    /// `HOTKEYS START` (Redis 8.6) on every master — tracking is per node,
    /// so a cluster collects on all of them at once. `count` is the top-K
    /// size; both metric flags false falls back to CPU (the server rejects
    /// an empty METRICS list).
    pub async fn hotkeys_start(&self, cpu: bool, net: bool, count: u64) -> Result<()> {
        let mut request = cmd("HOTKEYS");
        request.arg("START").arg("METRICS");
        match (cpu, net) {
            (true, true) => request.arg(2).arg("CPU").arg("NET"),
            (false, true) => request.arg(1).arg("NET"),
            _ => request.arg(1).arg("CPU"),
        };
        request.arg("COUNT").arg(count);
        // Replies are OK — or nil for a STOP/RESET with no session; type as
        // Value so the idempotent no-op cases don't fail decoding.
        let (_, _oks): (_, Vec<Value>) = self.query_async_masters(vec![request]).await?;
        Ok(())
    }

    /// `HOTKEYS STOP` on every master. Idempotent server-side — stopping an
    /// idle tracker answers OK, and the collected report stays readable.
    pub async fn hotkeys_stop(&self) -> Result<()> {
        let (_, _oks): (_, Vec<Value>) = self
            .query_async_masters(vec![cmd("HOTKEYS").arg("STOP").clone()])
            .await?;
        Ok(())
    }

    /// `HOTKEYS RESET` on every master — frees the collection. The server
    /// rejects it while tracking is active ("stop tracking first").
    pub async fn hotkeys_reset(&self) -> Result<()> {
        let (_, _oks): (_, Vec<Value>) = self
            .query_async_masters(vec![cmd("HOTKEYS").arg("RESET").clone()])
            .await?;
        Ok(())
    }

    /// `HOTKEYS GET` on every master, merged into one report (cluster slots
    /// are disjoint, so per-node top-K lists concatenate without double
    /// counting). An empty report means no node ever collected.
    pub async fn hotkeys_report(&self) -> Result<HotkeysReport> {
        let (_, replies): (_, Vec<Value>) = self
            .query_async_masters(vec![cmd("HOTKEYS").arg("GET").clone()])
            .await?;
        let mut report = HotkeysReport::default();
        for reply in &replies {
            report.merge_node_reply(reply);
        }
        report.sort();
        Ok(report)
    }

    /// `CLUSTER SLOT-STATS ORDERBY <metric> LIMIT <limit> DESC` (Redis 8.2)
    /// on every master. Each node reports only its own slots, so the union
    /// of per-node top lists contains the true global top `limit`; rows come
    /// back re-sorted by `metric`, truncated, and tagged with the owning
    /// master's `host:port`. Sorting by an extended metric fails server-side
    /// unless the cluster runs `cluster-slot-stats-enabled yes`.
    pub async fn cluster_slot_stats(&self, metric: SlotStatMetric, limit: u64) -> Result<Vec<SlotStatRow>> {
        let addrs = self.master_servers();
        let mut request = cmd("CLUSTER");
        request
            .arg("SLOT-STATS")
            .arg("ORDERBY")
            .arg(metric.word())
            .arg("LIMIT")
            .arg(limit)
            .arg("DESC");
        let cmds = vec![Some(request); addrs.len()];
        let replies: Vec<Option<Value>> = self.query_async_masters_with_option(cmds).await?;
        let mut rows = Vec::new();
        for (server, reply) in addrs.iter().zip(replies) {
            if let Some(reply) = reply {
                rows.extend(parse_slot_stats(&reply, &format!("{}:{}", server.host, server.port)));
            }
        }
        rows.sort_by(|a, b| b.metric(metric).cmp(&a.metric(metric)).then(a.slot.cmp(&b.slot)));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    /// `INFO keysizes` (Redis 8+) for the connection's database, summed
    /// across masters. Empty on servers without the section.
    pub async fn info_keysizes(&self) -> Result<Vec<KeysizesDist>> {
        let (_, texts): (_, Vec<String>) = self
            .query_async_masters(vec![cmd("INFO").arg("keysizes").clone()])
            .await?;
        Ok(merge_keysizes(
            texts.iter().map(|t| parse_keysizes(t, self.db)).collect(),
        ))
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
}
