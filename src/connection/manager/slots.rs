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

//! Cluster topology parsing + slot reshard planning (pure functions).

use super::*;

fn parse_address(address_str: &str) -> Result<(String, u16, Option<u16>)> {
    // Split into address part and optional cluster bus port part
    let (addr_part, cport_part) = address_str
        .split_once('@')
        .map(|(a, c)| (a, Some(c)))
        .unwrap_or((address_str, None));

    // Parse IP and Port
    let (ip, port_str) = addr_part.split_once(':').ok_or_else(|| Error::Invalid {
        message: format!("Invalid address format: {}", addr_part),
    })?;

    let port = port_str.parse::<u16>().map_err(|e| Error::Invalid {
        message: format!("Invalid port '{}': {}", port_str, e),
    })?;

    // Parse cluster bus port if present
    let cport = cport_part
        .map(|s| {
            s.parse::<u16>().map_err(|e| Error::Invalid {
                message: format!("Invalid cluster bus port '{}': {}", s, e),
            })
        })
        .transpose()?;

    Ok((ip.to_string(), port, cport))
}

/// Parses one migration marker token from `CLUSTER NODES`
/// (`[slot->-peer]` or `[slot-<-peer]`). Returns `None` for malformed
/// tokens so a bad marker never aborts the whole parse.
pub(super) fn parse_slot_migration_token(raw: &str) -> Option<SlotMigration> {
    let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
    if let Some((slot_s, peer)) = inner.split_once("->-") {
        let slot = slot_s.parse().ok()?;
        if peer.is_empty() {
            return None;
        }
        return Some(SlotMigration {
            slot,
            kind: SlotMigrationKind::Migrating,
            peer_id: peer.to_string(),
        });
    }
    if let Some((slot_s, peer)) = inner.split_once("-<-") {
        let slot = slot_s.parse().ok()?;
        if peer.is_empty() {
            return None;
        }
        return Some(SlotMigration {
            slot,
            kind: SlotMigrationKind::Importing,
            peer_id: peer.to_string(),
        });
    }
    None
}

/// Parses the output of the `CLUSTER NODES` command.
///
/// Columns (whitespace-separated):
///  0: node id
///  1: addr (`ip:port@cport[,hostname]`)
///  2: flags (comma-list, e.g. `master,myself`)
///  3: master id (`-` for masters)
///  4..7: ping-sent / pong-recv / config-epoch / link-state
///  8..: slot ranges (`N` / `N-M`) and migration markers
///        (`[N->-id]` migrating, `[N-<-id]` importing).
pub(super) fn parse_cluster_nodes(raw_data: &str) -> Result<Vec<ClusterNodeInfo>> {
    let mut nodes = Vec::new();

    for line in raw_data.trim().lines() {
        debug!(line, "cluster nodes");
        let parts: Vec<&str> = line.split_whitespace().collect();

        // Basic validation: ensure enough columns exist
        if parts.len() < 8 {
            continue;
        }

        let id = parts[0].to_string();
        let (ip, port, _) = parse_address(parts[1])?;

        // Parse flags to determine role
        let flags: HashSet<String> = parts[2].split(',').map(String::from).collect();
        let role = if flags.contains("master") {
            NodeRole::Master
        } else if flags.contains("slave") {
            NodeRole::Slave
        } else if flags.contains("fail") {
            NodeRole::Fail
        } else {
            NodeRole::Unknown
        };

        let master_id = if parts[3] != "-" {
            Some(parts[3].to_string())
        } else {
            None
        };

        let mut slots = Vec::new();
        let mut migrations = Vec::new();
        for raw in parts.iter().skip(8) {
            if raw.starts_with('[') {
                if let Some(m) = parse_slot_migration_token(raw) {
                    migrations.push(m);
                }
                continue;
            }
            if let Some((lo, hi)) = raw.split_once('-')
                && let (Ok(lo), Ok(hi)) = (lo.parse::<u16>(), hi.parse::<u16>())
            {
                slots.push((lo, hi));
                continue;
            }
            if let Ok(single) = raw.parse::<u16>() {
                slots.push((single, single));
            }
        }

        nodes.push(ClusterNodeInfo {
            id,
            ip,
            port,
            role,
            master_id,
            slots,
            migrations,
        });
    }

    Ok(nodes)
}

/// Pick up to `count` slots to move toward `target_id`.
///
/// When `source_id` is `Some`, take only from that master; otherwise
/// drain from the masters that currently hold the most slots (excluding
/// the target). Slots are taken from the high end of each range so the
/// remaining ownership stays more contiguous — the same heuristic
/// `redis-cli --cluster reshard` uses.
pub fn plan_reshard_slots(
    masters: &[(String, Vec<(u16, u16)>)],
    source_id: Option<&str>,
    target_id: &str,
    count: u32,
) -> Result<Vec<u16>, String> {
    if count == 0 {
        return Err("slot count must be > 0".into());
    }
    if target_id.is_empty() {
        return Err("target master is required".into());
    }

    // Expand ranges → individual slots, grouped by master.
    let mut by_master: Vec<(String, Vec<u16>)> = masters
        .iter()
        .filter(|(id, _)| id.as_str() != target_id)
        .filter(|(id, _)| source_id.is_none_or(|s| s == id.as_str()))
        .map(|(id, ranges)| {
            let mut slots = Vec::new();
            for &(lo, hi) in ranges {
                for s in lo..=hi {
                    slots.push(s);
                }
            }
            // Prefer high end first (pop from the back after sort).
            slots.sort_unstable();
            (id.clone(), slots)
        })
        .filter(|(_, slots)| !slots.is_empty())
        .collect();

    if by_master.is_empty() {
        return Err("no source slots available".into());
    }

    // Always drain the currently largest source first so an automatic
    // rebalance tends toward evenness.
    by_master.sort_by_key(|b| std::cmp::Reverse(b.1.len()));

    let mut planned = Vec::with_capacity(count as usize);
    let mut remaining = count;
    while remaining > 0 {
        // Re-sort each round so we keep peeling from the current largest.
        by_master.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
        let Some((_, slots)) = by_master.iter_mut().find(|(_, s)| !s.is_empty()) else {
            break;
        };
        if let Some(slot) = slots.pop() {
            planned.push(slot);
            remaining -= 1;
        }
    }

    if planned.is_empty() {
        return Err("no source slots available".into());
    }
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cluster_nodes_extracts_id_master_and_slots() {
        let raw = "07c37dfeb235213a872192d90877d0cd55635b91 127.0.0.1:30004@31004 slave e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 0 0 4 connected\n\
                   67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 127.0.0.1:30002@31002 master - 0 0 2 connected 5461-10922\n\
                   e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 127.0.0.1:30001@31001 myself,master - 0 0 1 connected 0-5460 [12345->-67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1]";
        let parsed = parse_cluster_nodes(raw).expect("parse must succeed");
        assert_eq!(parsed.len(), 3);

        let slave = &parsed[0];
        assert_eq!(slave.role, NodeRole::Slave);
        assert_eq!(
            slave.master_id.as_deref(),
            Some("e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca")
        );
        assert!(slave.slots.is_empty());

        let m1 = &parsed[1];
        assert_eq!(m1.role, NodeRole::Master);
        assert!(m1.master_id.is_none());
        assert_eq!(m1.slots, vec![(5461, 10922)]);

        // Owned ranges stay on the node; migration markers are captured
        // separately rather than discarded.
        let m2 = &parsed[2];
        assert_eq!(m2.slots, vec![(0, 5460)]);
        assert_eq!(m2.id, "e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca");
        assert_eq!(m2.migrations.len(), 1);
        assert_eq!(m2.migrations[0].slot, 12345);
        assert_eq!(m2.migrations[0].kind, SlotMigrationKind::Migrating);
        assert_eq!(m2.migrations[0].peer_id, "67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1");
    }

    #[test]
    fn parse_cluster_nodes_importing_marker() {
        let raw = "aabb 127.0.0.1:7001@17001 master - 0 0 1 connected 0-100 [50-<-ccdd]\n\
                   ccdd 127.0.0.1:7002@17002 master - 0 0 2 connected 101-200 [50->-aabb]";
        let parsed = parse_cluster_nodes(raw).expect("parse");
        assert_eq!(parsed[0].migrations[0].kind, SlotMigrationKind::Importing);
        assert_eq!(parsed[0].migrations[0].peer_id, "ccdd");
        assert_eq!(parsed[1].migrations[0].kind, SlotMigrationKind::Migrating);
        assert_eq!(parsed[1].migrations[0].peer_id, "aabb");
    }

    #[test]
    fn plan_reshard_takes_from_largest_source() {
        let masters = vec![
            ("a".into(), vec![(0, 9)]),   // 10 slots
            ("b".into(), vec![(10, 14)]), // 5 slots
            ("c".into(), vec![(15, 19)]), // 5 slots target
        ];
        let planned = plan_reshard_slots(&masters, None, "c", 3).expect("plan");
        assert_eq!(planned.len(), 3);
        // High end of the largest source first.
        assert_eq!(planned, vec![9, 8, 7]);
    }

    #[test]
    fn plan_reshard_respects_source_filter() {
        let masters = vec![("a".into(), vec![(0, 9)]), ("b".into(), vec![(10, 19)])];
        let planned = plan_reshard_slots(&masters, Some("b"), "a", 2).expect("plan");
        assert_eq!(planned, vec![19, 18]);
    }
}
