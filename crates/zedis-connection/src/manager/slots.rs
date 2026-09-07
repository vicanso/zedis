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
use zedis_core::string::split_host_port;

fn parse_address(address_str: &str) -> Result<(String, u16, Option<u16>)> {
    // Split into address part and optional cluster bus port part
    let (addr_part, cport_part) = address_str
        .split_once('@')
        .map(|(a, c)| (a, Some(c)))
        .unwrap_or((address_str, None));

    // Parse IP and port. `CLUSTER NODES` prints an IPv6 address bare
    // (`::1:7000@17000`), so the last colon is the separator.
    let (ip, port) = split_host_port(addr_part).ok_or_else(|| Error::Invalid {
        message: format!("Invalid address format: {}", addr_part),
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

/// The hash slots no master owns, as inclusive ranges. A cluster with a
/// gap here answers `cluster_state:fail` (unless `cluster-require-full-
/// coverage no`) and refuses every key in it — the state `CLUSTER
/// ADDSLOTS` repairs. Overlapping ranges are tolerated: a slot claimed
/// twice is still covered.
pub fn unassigned_slot_ranges(owners: &[ClusterSlotRange]) -> Vec<(u16, u16)> {
    let mut owned: Vec<(u16, u16)> = owners.iter().map(|range| (range.start, range.end)).collect();
    owned.sort_unstable();
    let mut gaps: Vec<(u16, u16)> = Vec::new();
    // A u32 cursor so the slot after 16383 has somewhere to land.
    let mut cursor: u32 = 0;
    for (start, end) in owned {
        let (start, end) = (u32::from(start), u32::from(end));
        if start > cursor {
            gaps.push((cursor as u16, (start - 1) as u16));
        }
        cursor = cursor.max(end + 1);
    }
    if cursor < CLUSTER_HASH_SLOTS {
        gaps.push((cursor as u16, (CLUSTER_HASH_SLOTS - 1) as u16));
    }
    gaps
}

/// One master's share of a rebalance: the slots it hands to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebalanceMove {
    pub source_id: String,
    pub target_id: String,
    /// Taken from the source's high end, so what it keeps stays contiguous.
    pub slots: Vec<u16>,
}

/// How far off an even split a master may sit before a rebalance bothers
/// moving anything, as a percentage of its ideal share. `redis-cli
/// --cluster rebalance` uses the same 2% default, for the same reason: on
/// a large cluster the remainder alone puts every master a slot or two off
/// ideal, and chasing that would move data for nothing.
pub const REBALANCE_THRESHOLD_PCT: f64 = 2.0;

/// Plan an even redistribution of the assigned slots across `masters`.
///
/// Each master's ideal share is the assigned slots divided by the master
/// count, remainder spread over the first few. A master above its ideal
/// gives, one below takes, and the biggest giver is paired with the
/// biggest taker until the deficits are covered — the shape
/// `redis-cli --cluster rebalance` produces. Slots come off a giver's high
/// end so what it keeps stays contiguous.
///
/// Returns an empty plan when every master is already within
/// [`REBALANCE_THRESHOLD_PCT`] of its ideal, which is the "nothing to do"
/// answer, not an error.
pub fn plan_cluster_rebalance(masters: &[(String, Vec<(u16, u16)>)]) -> Result<Vec<RebalanceMove>, String> {
    if masters.len() < 2 {
        return Err("rebalancing needs at least two masters".into());
    }
    // Slot lists per master, ascending — the planner peels from the back.
    let mut held: Vec<(String, Vec<u16>)> = masters
        .iter()
        .map(|(id, ranges)| {
            let mut slots: Vec<u16> = ranges.iter().flat_map(|(lo, hi)| *lo..=*hi).collect();
            slots.sort_unstable();
            slots.dedup();
            (id.clone(), slots)
        })
        .collect();
    let assigned: usize = held.iter().map(|(_, slots)| slots.len()).sum();
    if assigned == 0 {
        return Err("no slots are assigned yet".into());
    }

    // Ideal share, remainder spread over the masters in list order so the
    // totals add back up to `assigned`.
    let base = assigned / held.len();
    let remainder = assigned % held.len();
    let ideal: Vec<usize> = (0..held.len())
        .map(|index| base + usize::from(index < remainder))
        .collect();

    // Within the threshold everywhere means there is nothing worth moving.
    let balanced = held.iter().zip(&ideal).all(|((_, slots), ideal)| {
        let allowed = (*ideal as f64) * REBALANCE_THRESHOLD_PCT / 100.0;
        (slots.len() as f64 - *ideal as f64).abs() <= allowed.max(1.0)
    });
    if balanced {
        return Ok(Vec::new());
    }

    // Givers by surplus, takers by deficit, biggest first on both sides.
    let mut givers: Vec<(usize, usize)> = Vec::new();
    let mut takers: Vec<(usize, usize)> = Vec::new();
    for (index, ((_, slots), ideal)) in held.iter().zip(&ideal).enumerate() {
        match slots.len().cmp(ideal) {
            std::cmp::Ordering::Greater => givers.push((index, slots.len() - ideal)),
            std::cmp::Ordering::Less => takers.push((index, ideal - slots.len())),
            std::cmp::Ordering::Equal => {}
        }
    }
    givers.sort_by_key(|giver| std::cmp::Reverse(giver.1));
    takers.sort_by_key(|taker| std::cmp::Reverse(taker.1));

    let mut moves: Vec<RebalanceMove> = Vec::new();
    let mut giver = 0;
    let mut taker = 0;
    while giver < givers.len() && taker < takers.len() {
        let count = givers[giver].1.min(takers[taker].1);
        if count == 0 {
            if givers[giver].1 == 0 {
                giver += 1;
            } else {
                taker += 1;
            }
            continue;
        }
        let source_index = givers[giver].0;
        let target_index = takers[taker].0;
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(slot) = held[source_index].1.pop() else {
                break;
            };
            slots.push(slot);
        }
        if slots.is_empty() {
            giver += 1;
            continue;
        }
        let moved = slots.len();
        slots.sort_unstable();
        moves.push(RebalanceMove {
            source_id: held[source_index].0.clone(),
            target_id: held[target_index].0.clone(),
            slots,
        });
        givers[giver].1 -= moved;
        takers[taker].1 -= moved;
        if givers[giver].1 == 0 {
            giver += 1;
        }
        if takers[taker].1 == 0 {
            taker += 1;
        }
    }
    Ok(moves)
}

/// Compress a slot list into inclusive ranges — what `CLUSTER
/// MIGRATESLOTS` takes, and far fewer arguments than one per slot.
pub fn group_slot_ranges(slots: &[u16]) -> Vec<(u16, u16)> {
    let mut sorted: Vec<u16> = slots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for slot in sorted {
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == slot => *end = slot,
            _ => ranges.push((slot, slot)),
        }
    }
    ranges
}

/// Every slot inside `ranges`, in order — what `CLUSTER ADDSLOTS` takes
/// (it has no range form before Redis 7.0, so the caller chunks these).
pub fn slots_in_ranges(ranges: &[(u16, u16)]) -> Vec<u16> {
    let mut slots = Vec::new();
    for (start, end) in ranges {
        for slot in *start..=*end {
            slots.push(slot);
        }
    }
    slots
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

    fn owner(start: u16, end: u16) -> ClusterSlotRange {
        ClusterSlotRange {
            start,
            end,
            node_id: "n".to_string(),
            addr: "127.0.0.1:7000".to_string(),
            color_index: 0,
        }
    }

    fn master(id: &str, ranges: &[(u16, u16)]) -> (String, Vec<(u16, u16)>) {
        (id.to_string(), ranges.to_vec())
    }

    #[test]
    fn a_rebalance_moves_from_the_fullest_to_the_emptiest() {
        // A fresh node with nothing: it should receive a third of 16384.
        let plan = plan_cluster_rebalance(&[
            master("a", &[(0, 8191)]),
            master("b", &[(8192, 16383)]),
            master("c", &[]),
        ])
        .expect("plan");
        let onto_c: usize = plan.iter().filter(|m| m.target_id == "c").map(|m| m.slots.len()).sum();
        assert_eq!(onto_c, 5461, "c reaches its ideal share");
        assert!(plan.iter().all(|m| m.target_id == "c"), "only c is short: {plan:?}");
        // Taken off the high end, so what a giver keeps stays contiguous.
        let from_a = plan.iter().find(|m| m.source_id == "a").expect("a gives");
        assert_eq!(*from_a.slots.last().expect("slots"), 8191);

        // Every master keeps what the plan says it keeps.
        let mut totals = std::collections::HashMap::new();
        for (id, count) in [("a", 8192), ("b", 8192), ("c", 0)] {
            totals.insert(id.to_string(), count as i64);
        }
        for m in &plan {
            *totals.get_mut(&m.source_id).expect("source") -= m.slots.len() as i64;
            *totals.get_mut(&m.target_id).expect("target") += m.slots.len() as i64;
        }
        assert_eq!(totals.values().sum::<i64>(), 16384);
        assert!(
            totals.values().all(|count| (*count - 5461).abs() <= 1),
            "every master lands on its ideal share: {totals:?}"
        );
    }

    #[test]
    fn an_even_cluster_needs_no_rebalance() {
        let even = plan_cluster_rebalance(&[
            master("a", &[(0, 5460)]),
            master("b", &[(5461, 10922)]),
            master("c", &[(10923, 16383)]),
        ])
        .expect("plan");
        assert!(even.is_empty(), "nothing worth moving: {even:?}");
        // A single slot off ideal is inside the threshold too.
        let nearly = plan_cluster_rebalance(&[
            master("a", &[(0, 5461)]),
            master("b", &[(5462, 10922)]),
            master("c", &[(10923, 16383)]),
        ])
        .expect("plan");
        assert!(nearly.is_empty(), "within the threshold: {nearly:?}");
        // One master cannot be rebalanced against itself.
        assert!(plan_cluster_rebalance(&[master("a", &[(0, 16383)])]).is_err());
    }

    #[test]
    fn a_slot_list_compresses_into_the_ranges_migrateslots_takes() {
        assert_eq!(group_slot_ranges(&[3, 1, 2, 7, 8, 20]), vec![(1, 3), (7, 8), (20, 20)]);
        assert_eq!(group_slot_ranges(&[5, 5, 5]), vec![(5, 5)]);
        assert!(group_slot_ranges(&[]).is_empty());
        // The round trip the reshard path relies on.
        let slots: Vec<u16> = (100..=180).chain(500..=500).collect();
        assert_eq!(slots_in_ranges(&group_slot_ranges(&slots)), slots);
    }

    #[test]
    fn coverage_gaps_are_the_slots_addslots_has_to_repair() {
        // A cluster missing the slots one master used to own.
        let gaps = unassigned_slot_ranges(&[owner(0, 5460), owner(10923, 16383)]);
        assert_eq!(gaps, vec![(5461, 10922)]);
        assert_eq!(slots_in_ranges(&gaps).len(), 5462);

        // Full coverage, given out of order and overlapping.
        assert!(unassigned_slot_ranges(&[owner(10923, 16383), owner(0, 10922), owner(5000, 6000)]).is_empty());
        // Both ends open.
        assert_eq!(unassigned_slot_ranges(&[owner(10, 20)]), vec![(0, 9), (21, 16383)]);
        // Nothing assigned at all.
        assert_eq!(unassigned_slot_ranges(&[]), vec![(0, 16383)]);
    }

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
