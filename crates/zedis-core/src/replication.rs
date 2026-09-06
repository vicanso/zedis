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

//! The `INFO replication` section as a struct: which side of a primary /
//! replica link this server is on and how the link is doing. Pure
//! parsing — the commands that change the link (`REPLICAOF`, `FAILOVER`)
//! live in zedis-connection, the page that shows it in the app.
//!
//! Field names follow Redis's wire names (`slave_*`, `master_*`), and the
//! parser also accepts the `replica_*` spellings a fork may emit.

/// `role:` — the side of the link this server is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplicationRole {
    /// No `role:` line yet (nothing fetched, or a proxy that hides it).
    #[default]
    Unknown,
    Primary,
    Replica,
}

/// One `slaveN:` line of a primary's section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicationReplica {
    /// `ip:port`.
    pub addr: String,
    /// `online`, `wait_bgsave`, `send_bulk`.
    pub state: String,
    pub offset: i64,
    pub lag_seconds: i64,
    /// Bytes behind the primary's `master_repl_offset`, never negative.
    pub lag_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicationInfo {
    pub role: ReplicationRole,
    // ── the replica side ──
    pub master_host: String,
    pub master_port: u16,
    /// `up` / `down`.
    pub master_link_status: String,
    pub master_last_io_seconds_ago: i64,
    pub master_sync_in_progress: bool,
    /// Only while the link is down.
    pub master_link_down_since_seconds: i64,
    pub replica_read_only: bool,
    pub replica_repl_offset: i64,
    // ── the primary side ──
    pub connected_replicas: u64,
    pub replicas: Vec<ReplicationReplica>,
    pub master_replid: String,
    pub master_repl_offset: i64,
    pub second_repl_offset: i64,
    /// `no-failover`, `waiting-for-sync`, `failover-in-progress` (Redis
    /// 6.2+); empty on a server without `FAILOVER`.
    pub master_failover_state: String,
    pub repl_backlog_active: bool,
    pub repl_backlog_size: u64,
    pub repl_backlog_histlen: u64,
}

impl ReplicationInfo {
    /// Reads the replication lines out of an `INFO` text — the whole
    /// reply or just the `replication` section; other sections' keys are
    /// ignored.
    pub fn parse(text: &str) -> Self {
        let mut info = Self::default();
        let mut replicas: Vec<ReplicationReplica> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            if let Some(replica) = replica_line(key, value) {
                replicas.push(replica);
                continue;
            }
            match key {
                "role" => {
                    info.role = match value {
                        "master" | "primary" => ReplicationRole::Primary,
                        "slave" | "replica" => ReplicationRole::Replica,
                        _ => ReplicationRole::Unknown,
                    }
                }
                "master_host" => info.master_host = value.to_string(),
                "master_port" => info.master_port = value.parse().unwrap_or(0),
                "master_link_status" => info.master_link_status = value.to_string(),
                "master_last_io_seconds_ago" => info.master_last_io_seconds_ago = int(value),
                "master_sync_in_progress" => info.master_sync_in_progress = value == "1",
                "master_link_down_since_seconds" => info.master_link_down_since_seconds = int(value),
                "slave_read_only" | "replica_read_only" => info.replica_read_only = value == "1",
                "slave_repl_offset" | "replica_repl_offset" => info.replica_repl_offset = int(value),
                "connected_slaves" | "connected_replicas" => info.connected_replicas = value.parse().unwrap_or(0),
                "master_replid" => info.master_replid = value.to_string(),
                "master_repl_offset" => info.master_repl_offset = int(value),
                "second_repl_offset" => info.second_repl_offset = int(value),
                "master_failover_state" => info.master_failover_state = value.to_string(),
                "repl_backlog_active" => info.repl_backlog_active = value == "1",
                "repl_backlog_size" => info.repl_backlog_size = value.parse().unwrap_or(0),
                "repl_backlog_histlen" => info.repl_backlog_histlen = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        // `slaveN` lines and `master_repl_offset` come in either order.
        for mut replica in replicas {
            replica.lag_bytes = (info.master_repl_offset - replica.offset).max(0);
            info.replicas.push(replica);
        }
        info
    }

    /// The primary this replica follows, as `host:port`.
    pub fn master_addr(&self) -> String {
        format!("{}:{}", self.master_host, self.master_port)
    }

    pub fn link_up(&self) -> bool {
        self.master_link_status == "up"
    }

    /// A `FAILOVER` is under way on this primary (writes are paused).
    pub fn failover_in_progress(&self) -> bool {
        !self.master_failover_state.is_empty() && self.master_failover_state != "no-failover"
    }
}

fn int(value: &str) -> i64 {
    value.parse().unwrap_or(0)
}

/// `slave0:ip=10.0.0.4,port=6379,state=online,offset=900,lag=0` (or
/// `replica0:`), else `None`.
fn replica_line(key: &str, value: &str) -> Option<ReplicationReplica> {
    let index = key.strip_prefix("slave").or_else(|| key.strip_prefix("replica"))?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut replica = ReplicationReplica::default();
    let (mut ip, mut port) = ("", "");
    for field in value.split(',') {
        let Some((name, field_value)) = field.split_once('=') else {
            continue;
        };
        match name {
            "ip" => ip = field_value,
            "port" => port = field_value,
            "state" => replica.state = field_value.to_string(),
            "offset" => replica.offset = int(field_value),
            "lag" => replica.lag_seconds = int(field_value),
            _ => {}
        }
    }
    if ip.is_empty() {
        return None;
    }
    replica.addr = format!("{ip}:{port}");
    Some(replica)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_primary_section_lists_its_replicas_with_their_lag() {
        let text = "# Replication\n\
                    role:master\n\
                    connected_slaves:2\n\
                    slave0:ip=10.0.0.4,port=6379,state=online,offset=900,lag=0\n\
                    slave1:ip=10.0.0.5,port=6379,state=wait_bgsave,offset=600,lag=2\n\
                    master_failover_state:no-failover\n\
                    master_replid:8f0d1c2b\n\
                    master_repl_offset:1000\n\
                    second_repl_offset:-1\n\
                    repl_backlog_active:1\n\
                    repl_backlog_size:1048576\n\
                    repl_backlog_histlen:1000\n";
        let info = ReplicationInfo::parse(text);
        assert_eq!(info.role, ReplicationRole::Primary);
        assert_eq!(info.connected_replicas, 2);
        assert_eq!(info.replicas.len(), 2);
        assert_eq!(info.replicas[0].addr, "10.0.0.4:6379");
        assert_eq!(info.replicas[0].lag_bytes, 100);
        assert_eq!(info.replicas[1].state, "wait_bgsave");
        assert_eq!(info.replicas[1].lag_seconds, 2);
        assert_eq!(info.replicas[1].lag_bytes, 400);
        assert_eq!(info.master_replid, "8f0d1c2b");
        assert!(info.repl_backlog_active);
        assert_eq!(info.repl_backlog_size, 1_048_576);
        assert!(!info.failover_in_progress());
    }

    #[test]
    fn a_replica_section_names_its_primary_and_the_link() {
        let text = "role:slave\n\
                    master_host:10.0.0.1\n\
                    master_port:6379\n\
                    master_link_status:down\n\
                    master_last_io_seconds_ago:-1\n\
                    master_sync_in_progress:1\n\
                    master_link_down_since_seconds:12\n\
                    slave_read_only:1\n\
                    slave_repl_offset:4242\n\
                    connected_slaves:0\n";
        let info = ReplicationInfo::parse(text);
        assert_eq!(info.role, ReplicationRole::Replica);
        assert_eq!(info.master_addr(), "10.0.0.1:6379");
        assert!(!info.link_up());
        assert!(info.master_sync_in_progress);
        assert_eq!(info.master_link_down_since_seconds, 12);
        assert!(info.replica_read_only);
        assert_eq!(info.replica_repl_offset, 4242);
        assert!(info.replicas.is_empty());
    }

    #[test]
    fn fork_spellings_and_a_running_failover_are_read() {
        let text = "role:replica\nreplica_read_only:1\nreplica_repl_offset:7\n\
                    replica0:ip=10.0.0.9,port=6380,state=online,offset=7,lag=1\n\
                    master_repl_offset:9\nmaster_failover_state:waiting-for-sync\n";
        let info = ReplicationInfo::parse(text);
        assert_eq!(info.role, ReplicationRole::Replica);
        assert!(info.replica_read_only);
        assert_eq!(info.replicas[0].addr, "10.0.0.9:6380");
        assert_eq!(info.replicas[0].lag_bytes, 2);
        assert!(info.failover_in_progress());
    }

    #[test]
    fn other_sections_and_junk_are_ignored() {
        let info = ReplicationInfo::parse("# Server\nredis_version:7.2.4\nslaves:nope\nslave:ip=1\n\n");
        assert_eq!(info, ReplicationInfo::default());
    }
}
