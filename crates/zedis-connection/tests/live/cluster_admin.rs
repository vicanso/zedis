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

//! A cluster administered: slots moved by hand and atomically, repairs,
//! the rebalance plan, the per-node operations and slot statistics.

use super::*;

/// `CLUSTER SLOT-STATS` (8.2): per-master top lists merge, sort by the
/// chosen metric and stay key-count-only while the extended metrics config
/// is off (its default — it cannot be enabled at runtime).
/// The two cluster repairs the Topology page offers. A healthy cluster
/// has no coverage gap, and a reshard that was interrupted leaves a slot
/// marked on both ends — which the slot map pairs and
/// `CLUSTER SETSLOT … STABLE` settles.
///
/// The markers are cleared *before* the assertions run: a failing assert
/// must not leave the shared cluster with a stuck migration.
/// Valkey 9's atomic slot migration: `CLUSTER MIGRATESLOTS` hands a range
/// to another master, `GETSLOTMIGRATIONS` reports it until a terminal
/// state, and ownership has actually moved when it finishes. The slots are
/// migrated back, so the cluster ends as it started.
/// `redis-cli --cluster create` splits the slots evenly, so the rebalance
/// planner must find nothing to do on a fresh cluster. Read-only: it takes
/// no slot lock and mutates nothing.
#[test]
#[ignore]
fn cluster_rebalance_plan_is_empty_on_a_fresh_cluster() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-rebalance", addr)).await;
        let map = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("client")
            .nodes_description()
            .slot_map;
        let masters: Vec<(String, Vec<(u16, u16)>)> = map
            .masters
            .iter()
            .map(|master| {
                let ranges = map
                    .owners
                    .iter()
                    .filter(|range| range.node_id == master.node_id)
                    .map(|range| (range.start, range.end))
                    .collect();
                (master.node_id.clone(), ranges)
            })
            .collect();
        assert!(masters.len() >= 2, "the cluster has several masters: {masters:?}");
        let plan = plan_cluster_rebalance(&masters).expect("plan");
        assert!(
            plan.is_empty(),
            "an evenly created cluster needs no rebalance: {plan:?}"
        );
    });
}

/// The hand reshard every server has: a slot with a key in it moves to
/// another master — the key with it, the ownership committed on every
/// master — and back again.
#[test]
#[ignore]
fn cluster_moves_a_slot_by_hand_with_its_keys() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-hand", addr)).await;
        let at = ServerDb::new(&*id, 0);
        let _slots = CLUSTER_SLOTS.lock().await;
        let entry = |label: &str, addr: &str| {
            let (host, port) = addr.rsplit_once(':').expect("host:port");
            protected_server(label, (host.to_string(), port.parse().expect("port")))
        };
        /// Who owns `slot`, by node id, from a freshly read topology.
        async fn owner_of(id: &str, slot: u16) -> String {
            get_connection_manager()
                .get_client_without_cache(id, 0)
                .await
                .expect("re-read the cluster")
                .nodes_description()
                .slot_map
                .owners
                .iter()
                .find(|range| range.start <= slot && slot <= range.end)
                .map(|range| range.node_id.clone())
                .unwrap_or_default()
        }

        let map = get_connection_manager()
            .get_client_without_cache(&id, 0)
            .await
            .expect("client")
            .nodes_description()
            .slot_map;
        let key = "{zedis-it-hand}:moved";
        let first = map.masters.first().expect("a master");
        let mut probe = open_single_connection(&entry("it-cluster-hand-probe", &first.addr), 0, false)
            .await
            .expect("connect");
        let slot: u16 = cmd("CLUSTER")
            .arg("KEYSLOT")
            .arg(key)
            .query_async(&mut probe)
            .await
            .expect("keyslot");
        let source_id = owner_of(&id, slot).await;
        let source = map
            .masters
            .iter()
            .find(|master| master.node_id == source_id)
            .expect("the slot's owner is a master")
            .clone();
        let target = map
            .masters
            .iter()
            .find(|master| master.node_id != source_id)
            .expect("a second master")
            .clone();
        let mut source_conn = open_single_connection(&entry("it-cluster-hand-src", &source.addr), 0, false)
            .await
            .expect("connect to the source");
        let mut target_conn = open_single_connection(&entry("it-cluster-hand-dst", &target.addr), 0, false)
            .await
            .expect("connect to the target");
        let _: () = cmd("SET")
            .arg(key)
            .arg("payload")
            .query_async(&mut source_conn)
            .await
            .expect("set");
        let masters = master_addrs(&at).await.expect("masters");
        let source_node = ClusterNode::new(&*id, &source.addr);
        let target_node = ClusterNode::new(&*id, &target.addr);

        migrate_slot(SlotMove {
            slot,
            source: &source_node,
            source_id: &source.node_id,
            target: &target_node,
            target_id: &target.node_id,
            master_addrs: &masters,
        })
        .await
        .expect("move the slot");
        let moved: Option<String> = cmd("GET")
            .arg(key)
            .query_async(&mut target_conn)
            .await
            .expect("get on the target");
        assert_eq!(moved.as_deref(), Some("payload"), "the key went with its slot");
        assert_eq!(owner_of(&id, slot).await, target.node_id, "ownership moved");

        migrate_slot(SlotMove {
            slot,
            source: &target_node,
            source_id: &target.node_id,
            target: &source_node,
            target_id: &source.node_id,
            master_addrs: &masters,
        })
        .await
        .expect("move it back");
        let back: Option<String> = cmd("GET")
            .arg(key)
            .query_async(&mut source_conn)
            .await
            .expect("get on the source");
        assert_eq!(back.as_deref(), Some("payload"), "and back with it");
        assert_eq!(owner_of(&id, slot).await, source.node_id, "and back again");
        let _: i64 = cmd("DEL")
            .arg(key)
            .query_async(&mut source_conn)
            .await
            .expect("clean up");
    });
}

/// One job in two dialects: Valkey 9's `MIGRATESLOTS`, told to the source,
/// and Redis 8.4's `CLUSTER MIGRATION IMPORT`, told to the target. Either
/// way two slots move and come back, watched from the source's side.
#[test]
#[ignore]
fn cluster_migrates_slots_atomically_on_valkey_9_and_redis_8_4() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-atomic", addr)).await;
        let _slots = CLUSTER_SLOTS.lock().await;
        if !supports(&id, floors::ATOMIC_SLOT_MIGRATION).await {
            eprintln!("skipped: atomic slot migration is Valkey 9.0+ / Redis 8.4+");
            return;
        }
        let node = |addr: &str| {
            let (host, port) = addr.rsplit_once(':').expect("host:port");
            protected_server("it-cluster-node", (host.to_string(), port.parse().expect("port")))
        };
        /// Who owns `slot`, by node id, from a freshly read topology.
        async fn owner_of(id: &str, slot: u16) -> String {
            get_connection_manager()
                .get_client_without_cache(id, 0)
                .await
                .expect("re-read the cluster")
                .nodes_description()
                .slot_map
                .owners
                .iter()
                .find(|range| range.start <= slot && slot <= range.end)
                .map(|range| range.node_id.clone())
                .unwrap_or_default()
        }
        /// The names already on the node, so a job from an earlier run (or
        /// an earlier half of this one) cannot satisfy the wait below.
        async fn migration_names<C: redis::aio::ConnectionLike + Send>(
            conn: &mut C,
            dialect: SlotMigrationDialect,
        ) -> HashSet<String> {
            cluster_get_slot_migrations(conn, dialect)
                .await
                .expect("getslotmigrations")
                .into_iter()
                .map(|migration| migration.name)
                .collect()
        }

        /// Poll until a migration this call started — a name not in `before`,
        /// exporting to `target_id` — reaches a terminal state. An empty
        /// reply means the server has not registered the job yet, which is
        /// why "nothing is active" is not the condition.
        async fn wait_for_export<C: redis::aio::ConnectionLike + Send>(
            conn: &mut C,
            dialect: SlotMigrationDialect,
            before: &HashSet<String>,
            target_id: &str,
            what: &str,
        ) -> zedis_connection::AtomicSlotMigration {
            let mut last = Vec::new();
            if let Some(found) = poll_until(Duration::from_secs(40), Duration::from_millis(250), async || {
                last = cluster_get_slot_migrations(conn, dialect)
                    .await
                    .expect("migration status");
                last.iter()
                    .find(|migration| {
                        migration.is_export()
                            && migration.target_node == target_id
                            && !before.contains(&migration.name)
                            && !migration.is_active()
                    })
                    .cloned()
            })
            .await
            {
                return found;
            }
            panic!("{what}: {last:?}");
        }

        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let dialect = SlotMigrationDialect::for_flavor(client.is_valkey());
        let map = client.nodes_description().slot_map;
        // The master holding the most slots gives two of them up, so the
        // cluster keeps full coverage whatever happens.
        let source = map
            .masters
            .iter()
            .max_by_key(|master| master.slot_count)
            .expect("a master")
            .clone();
        let target = map
            .masters
            .iter()
            .find(|master| master.node_id != source.node_id)
            .expect("a second master")
            .clone();
        let range = map
            .owners
            .iter()
            .filter(|range| range.node_id == source.node_id)
            .max_by_key(|range| range.end - range.start)
            .expect("the source owns a range")
            .clone();
        assert!(
            range.end - range.start >= 2,
            "the range is big enough to lend two slots"
        );
        let moving = (range.end - 1, range.end);

        let mut source_conn = open_single_connection(&node(&source.addr), 0, false)
            .await
            .expect("connect to the source");
        let mut target_conn = open_single_connection(&node(&target.addr), 0, false)
            .await
            .expect("connect to the target");

        let before = migration_names(&mut source_conn, dialect).await;
        // Valkey's source pushes, Redis's target pulls: the start goes to
        // whichever end the dialect names, the watch stays on the source.
        let starter: &mut RedisAsyncConn = if dialect.starts_on_target() {
            &mut target_conn
        } else {
            &mut source_conn
        };
        cluster_migrate_slots(starter, dialect, &[moving], &target.node_id)
            .await
            .expect("start the migration");
        let job = wait_for_export(
            &mut source_conn,
            dialect,
            &before,
            &target.node_id,
            "the migration reached a terminal state",
        )
        .await;
        assert!(matches!(job.state.as_str(), "success" | "completed"), "{job:?}");
        assert_eq!(job.source_node, source.node_id);
        assert!(!job.slot_ranges.is_empty());
        assert_eq!(
            owner_of(&id, moving.1).await,
            target.node_id,
            "ownership moved with the slots"
        );

        // Put them back so the cluster ends where it started — through the
        // operation the Topology page calls, which is handed both ends and
        // picks the one its dialect is told on by itself.
        let before = migration_names(&mut target_conn, dialect).await;
        node_migrate_slots(
            &ClusterNode::new(&*id, &target.addr),
            &ClusterNode::new(&*id, &source.addr),
            &source.node_id,
            &[moving],
        )
        .await
        .expect("start the migration back");
        let back = wait_for_export(
            &mut target_conn,
            dialect,
            &before,
            &source.node_id,
            "the migration back reached a terminal state",
        )
        .await;
        assert!(matches!(back.state.as_str(), "success" | "completed"), "{back:?}");
        assert_eq!(owner_of(&id, moving.1).await, source.node_id, "and back again");
    });
}

/// The node-addressed cluster commands (ADR 10). `CLUSTER REPLICATE`,
/// `FAILOVER`, `ADDSLOTS` and `SETSLOT` are not gossiped — they do what they
/// say on the node they reach — so they take a [`ClusterNode`], and this is
/// what proves the address is the one they land on.
///
/// Every step here is reversible on a healthy cluster: a slot is marked and
/// stabilised again, and the only write is to a slot holding no keys.
#[test]
#[ignore]
fn cluster_node_operations_address_the_node_they_name() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-nodes", addr)).await;
        let _slots = CLUSTER_SLOTS.lock().await;
        let at = ServerDb::new(&id, 0);
        let map = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("client")
            .nodes_description()
            .slot_map;

        let addrs = master_addrs(&at).await.expect("masters");
        assert_eq!(addrs.len(), map.masters.len(), "one address per master: {addrs:?}");
        assert!(
            addrs.iter().all(|addr| map.masters.iter().any(|m| m.addr == *addr)),
            "the addresses are the ones the slot map names: {addrs:?}"
        );

        // Each master answers for itself: the load sample and the migration
        // list are per node, not per cluster.
        for addr in &addrs {
            let node = ClusterNode::new(&*id, addr);
            assert_eq!(node.addr(), addr);
            let load = node_load(&node).await.expect("INFO on the node");
            assert!(load.used_memory > 0, "{addr} reported no memory: {load:?}");
            assert!(load.connected_clients > 0, "we are connected to {addr}");
            // Runs whatever the server's answer is: a build without atomic
            // slot migration has no list, which is not this test's business.
            let _ = node_slot_migrations(&node).await;
        }

        // A slot with no keys, so nothing this test marks can redirect
        // another test's traffic.
        let mut owners = map.owners.clone();
        owners.sort_by_key(|range| range.start);
        let source = owners.first().expect("a master owns slots").clone();
        let target = map
            .masters
            .iter()
            .find(|master| master.node_id != source.node_id)
            .expect("a second master")
            .clone();
        let source_node = ClusterNode::new(&*id, &source.addr);
        let mut source_conn = {
            let (host, port) = source.addr.rsplit_once(':').expect("host:port");
            let entry = protected_server("it-cluster-nodes-src", (host.to_string(), port.parse().expect("port")));
            open_single_connection(&entry, 0, false).await.expect("connect")
        };
        let mut empty_slot = None;
        for slot in source.start..=source.end.min(source.start.saturating_add(200)) {
            let keys: u64 = cmd("CLUSTER")
                .arg("COUNTKEYSINSLOT")
                .arg(slot)
                .query_async(&mut source_conn)
                .await
                .expect("countkeysinslot");
            if keys == 0 {
                empty_slot = Some(slot);
                break;
            }
        }
        let slot = empty_slot.expect("an empty slot in the first master's range");

        // Mark it as a half-finished migration would, then stabilise it
        // through the operation — the one the Topology panel's repair uses.
        let _: String = cmd("CLUSTER")
            .arg("SETSLOT")
            .arg(slot)
            .arg("MIGRATING")
            .arg(&target.node_id)
            .query_async(&mut source_conn)
            .await
            .expect("setslot migrating");
        node_stabilize_slot(&source_node, slot).await.expect("setslot stable");
        let settled = get_connection_manager()
            .get_client_without_cache(&id, 0)
            .await
            .expect("re-read the cluster")
            .nodes_description()
            .slot_map
            .migrations;
        assert!(
            !settled.iter().any(|entry| entry.slot == slot),
            "the operation settled it on the node it named: {settled:?}"
        );

        // `ADDSLOTS` on a slot that already has an owner is refused, which
        // is what makes it safe to send only the coverage gaps — and proves
        // the command reached the node rather than being gossiped away.
        let err = node_add_slots(&source_node, &[slot])
            .await
            .expect_err("the slot has an owner");
        assert!(err.to_string().contains("CLUSTER ADDSLOTS on "), "{err}");
        assert!(node_add_slots(&source_node, &[]).await.is_ok(), "nothing to add");

        // A master refuses `CLUSTER FAILOVER`: only a replica is promoted.
        // The error is the proof it ran on the node, not on the cluster.
        let failover = node_failover(&source_node, false).await;
        assert!(failover.is_err(), "a master cannot fail over to itself");
        // Likewise `REPLICATE`: a master with slots cannot become a replica.
        let replicate = node_replicate(&source_node, &target.node_id).await;
        assert!(replicate.is_err(), "a master holding slots stays one");

        // A node with no migration running takes the cancel where it has the
        // command (Valkey 9's `CANCELSLOTMIGRATIONS`, Redis 8.4's
        // `MIGRATION CANCEL`), and where it does not, the refusal names the
        // node it was sent to — whichever dialect's command it was.
        match node_cancel_slot_migrations(&source_node).await {
            Ok(()) => {}
            Err(e) => {
                let message = e.to_string();
                assert!(
                    message.contains(&format!(" on {}: ", source.addr)),
                    "the error names the node it was sent to: {message}"
                );
            }
        }
    });
}

/// A half-finished reshard leaves a slot marked importing on one node and
/// migrating on the other; `SETSLOT … STABLE` on both is the repair, and
/// what the Topology panel offers once it can see the pair.
#[test]
#[ignore]
fn cluster_slot_repairs_clear_a_stuck_migration() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-repair", addr)).await;
        let _slots = CLUSTER_SLOTS.lock().await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let map = client.nodes_description().slot_map;
        assert!(
            unassigned_slot_ranges(&map.owners).is_empty(),
            "a healthy cluster covers all 16384 slots: {:?}",
            map.owners
        );

        // Two masters, and a slot the first owns that holds no keys — so
        // nothing this test marks can redirect another test's traffic.
        let mut owners = map.owners.clone();
        owners.sort_by_key(|range| range.start);
        let source = owners.first().expect("a master owns slots").clone();
        let target = map
            .masters
            .iter()
            .find(|master| master.node_id != source.node_id)
            .expect("a second master")
            .clone();
        let node = |addr: &str| {
            let (host, port) = addr.rsplit_once(':').expect("host:port");
            protected_server("it-cluster-node", (host.to_string(), port.parse().expect("port")))
        };
        let mut source_conn = open_single_connection(&node(&source.addr), 0, false)
            .await
            .expect("connect to the source master");
        let mut target_conn = open_single_connection(&node(&target.addr), 0, false)
            .await
            .expect("connect to the target master");

        let mut empty_slot = None;
        for slot in source.start..=source.end.min(source.start.saturating_add(200)) {
            let keys: u64 = cmd("CLUSTER")
                .arg("COUNTKEYSINSLOT")
                .arg(slot)
                .query_async(&mut source_conn)
                .await
                .expect("countkeysinslot");
            if keys == 0 {
                empty_slot = Some(slot);
                break;
            }
        }
        let slot = empty_slot.expect("an empty slot in the first master's range");

        // Exactly what an interrupted reshard leaves behind.
        let setslot = |slot: u16, state: &'static str, peer: String| {
            cmd("CLUSTER").arg("SETSLOT").arg(slot).arg(state).arg(peer).clone()
        };
        let _: String = setslot(slot, "IMPORTING", source.node_id.clone())
            .query_async(&mut target_conn)
            .await
            .expect("setslot importing");
        let _: String = setslot(slot, "MIGRATING", target.node_id.clone())
            .query_async(&mut source_conn)
            .await
            .expect("setslot migrating");

        // Re-read the topology (the map is built at connect, so not the
        // cached client), then clear before asserting anything.
        let stuck = get_connection_manager()
            .get_client_without_cache(&id, 0)
            .await
            .expect("re-read the cluster")
            .nodes_description()
            .slot_map
            .migrations
            .clone();

        for conn in [&mut source_conn, &mut target_conn] {
            let _: String = cmd("CLUSTER")
                .arg("SETSLOT")
                .arg(slot)
                .arg("STABLE")
                .query_async(conn)
                .await
                .expect("setslot stable");
        }
        let settled = get_connection_manager()
            .get_client_without_cache(&id, 0)
            .await
            .expect("re-read the cluster")
            .nodes_description()
            .slot_map
            .migrations
            .clone();

        let entry = stuck
            .iter()
            .find(|entry| entry.slot == slot)
            .unwrap_or_else(|| panic!("the half-done migration is paired: {stuck:?}"));
        assert_eq!(entry.source_id, source.node_id);
        assert_eq!(entry.target_id, target.node_id);
        assert_eq!(
            entry.source_addr, source.addr,
            "both ends are addressable, which STABLE needs"
        );
        assert_eq!(entry.target_addr, target.addr);
        assert!(
            !settled.iter().any(|entry| entry.slot == slot),
            "STABLE settled it: {settled:?}"
        );
    });
}

#[test]
#[ignore]
fn cluster_slot_stats_ranks_slots_by_key_count() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-slot-stats", addr)).await;
        if !supports(&id, floors::CLUSTER_SLOT_STATS).await {
            eprintln!("skipped: cluster predates SLOT-STATS (Redis 8.2 / Valkey 8.0)");
            return;
        }
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let prefix = unique("ss");
        let mut c = conn(&id, 0).await;
        let keys: Vec<String> = (0..20).map(|i| format!("{prefix}:{i}")).collect();
        for key in &keys {
            cmd("SET").arg(key).arg("x").exec_async(&mut c).await.expect("set");
        }

        let rows = client
            .cluster_slot_stats(SlotStatMetric::KeyCount, 10)
            .await
            .expect("slot stats");
        assert_eq!(rows.len(), 10, "3 masters × top 10 → global top 10");
        assert!(rows[0].key_count >= 1, "the busiest slot holds at least one test key");
        assert!(
            rows.windows(2).all(|w| w[0].key_count >= w[1].key_count),
            "descending by key-count"
        );
        assert!(rows.iter().all(|r| !r.node.is_empty()), "rows carry the owning master");
        assert!(
            rows.iter().all(|r| !r.has_extended_metrics()),
            "extended metrics stay None while cluster-slot-stats-enabled is off"
        );

        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

/// `CLUSTER MEET` and `CLUSTER FORGET` are told to every master (ADR 10).
/// The harness has no spare node to add and take away, so this introduces a
/// member every master already has — each takes the handshake and settles
/// it on the node it knows — and forgets an id none of them knows, which
/// each refuses by name. The cluster ends as it started.
#[test]
#[ignore]
fn cluster_meet_and_forget_are_told_to_every_master() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-meet", addr)).await;
        // Until it settles, the handshake is an extra entry in every node's
        // table, which the tests holding this lock count.
        let _slots = CLUSTER_SLOTS.lock().await;
        let at = ServerDb::new(&*id, 0);

        /// The node ids each master's own table lists, handshakes included.
        async fn node_tables(addrs: &[String]) -> Vec<HashSet<String>> {
            let mut tables = Vec::new();
            for addr in addrs {
                let (host, port) = addr.rsplit_once(':').expect("host:port");
                let entry = protected_server("it-cluster-meet-node", (host.to_string(), port.parse().expect("port")));
                let mut c = open_single_connection(&entry, 0, false)
                    .await
                    .expect("connect to a master");
                let nodes: String = cmd("CLUSTER")
                    .arg("NODES")
                    .query_async(&mut c)
                    .await
                    .expect("cluster nodes");
                tables.push(
                    nodes
                        .lines()
                        .filter_map(|line| line.split_whitespace().next())
                        .map(str::to_string)
                        .collect(),
                );
            }
            tables
        }

        let masters = master_addrs(&at).await.expect("masters");
        let before = node_tables(&masters).await;
        let member = masters.last().expect("a master");
        let (host, port) = member.rsplit_once(':').expect("host:port");
        cluster_meet(&at, host, port.parse().expect("port"))
            .await
            .expect("every master takes a member it knows");
        let mut after = Vec::new();
        let _ = wait_until(Duration::from_secs(20), Duration::from_millis(250), async || {
            after = node_tables(&masters).await;
            after == before
        })
        .await;
        assert_eq!(after, before, "the handshake settled on the node every master had");

        let unknown = "deadbeef".repeat(5);
        let refused = cluster_forget(&at, &unknown)
            .await
            .expect_err("no master knows that id");
        assert!(
            refused.to_string().to_lowercase().contains("unknown node"),
            "the master says why: {refused}"
        );
    });
}
