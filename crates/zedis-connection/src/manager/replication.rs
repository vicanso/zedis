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

//! Standalone replication on the pooled client's node: reading the link
//! (`INFO replication`) and the three commands that change it —
//! `REPLICAOF host port`, `REPLICAOF NO ONE` and `FAILOVER` (Redis 6.2,
//! [`floors::FAILOVER`](crate::floors::FAILOVER)). They are meant for a
//! plain primary / replica pair: a Sentinel overrides a hand-made
//! `REPLICAOF`, and a cluster node answers with its own `CLUSTER
//! REPLICATE` / `CLUSTER FAILOVER`, which the Topology page offers there
//! instead.

use super::RedisClient;
use crate::error::Error;
use redis::cmd;
use zedis_core::replication::ReplicationInfo;

type Result<T, E = Error> = std::result::Result<T, E>;

/// The `TIMEOUT` every `FAILOVER` carries. Writes pause from the moment
/// the primary accepts the command until the replica has caught up, so a
/// failover without a timeout can hold a primary's writes for as long as
/// a stalled replica takes; on expiry the failover is abandoned and
/// writes resume — or, with `FORCE`, the replica is promoted as it is.
pub const FAILOVER_TIMEOUT_MS: u64 = 10_000;

impl RedisClient {
    /// `INFO replication`, parsed.
    pub async fn replication_info(&self) -> Result<ReplicationInfo> {
        let mut conn = self.connection();
        let text: String = cmd("INFO").arg("replication").query_async(&mut conn).await?;
        Ok(ReplicationInfo::parse(&text))
    }

    /// `REPLICAOF host port` — this node drops its dataset and follows
    /// `host:port` from a full sync.
    pub async fn replicaof(&self, host: &str, port: u16) -> Result<()> {
        let mut conn = self.connection();
        let _: String = cmd("REPLICAOF").arg(host).arg(port).query_async(&mut conn).await?;
        Ok(())
    }

    /// `REPLICAOF NO ONE` — this node stops following its primary and
    /// keeps the data it has; writes are accepted again.
    pub async fn replicaof_no_one(&self) -> Result<()> {
        let mut conn = self.connection();
        let _: String = cmd("REPLICAOF").arg("NO").arg("ONE").query_async(&mut conn).await?;
        Ok(())
    }

    /// `FAILOVER [TO host port [FORCE]] TIMEOUT ms` on this primary: writes
    /// pause, the replica catches up, it is promoted and this node starts
    /// replicating from it. `FORCE` (which Redis only accepts with a target
    /// and a timeout) promotes on expiry even if the replica is behind.
    pub async fn failover(&self, target: Option<(&str, u16)>, force: bool, timeout_ms: u64) -> Result<()> {
        let mut c = cmd("FAILOVER");
        if let Some((host, port)) = target {
            c.arg("TO").arg(host).arg(port);
            if force {
                c.arg("FORCE");
            }
        }
        c.arg("TIMEOUT").arg(timeout_ms);
        let mut conn = self.connection();
        let _: String = c.query_async(&mut conn).await?;
        Ok(())
    }

    /// `FAILOVER ABORT` — cancels a failover still waiting for the replica
    /// to sync; writes resume on this primary.
    pub async fn failover_abort(&self) -> Result<()> {
        let mut conn = self.connection();
        let _: String = cmd("FAILOVER").arg("ABORT").query_async(&mut conn).await?;
        Ok(())
    }
}
