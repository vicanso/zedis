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

//! A primary / replica pair: promote, relink and fail over.

use super::*;

/// `INFO replication` of `id` polled until `ok` holds — a role change or a
/// link coming up takes the pair a moment.
async fn wait_replication(id: &str, what: &str, ok: impl Fn(&ReplicationInfo) -> bool) -> ReplicationInfo {
    let mut last = ReplicationInfo::default();
    if let Some(info) = poll_until(Duration::from_secs(20), Duration::from_millis(250), async || {
        let client = get_connection_manager().get_client(id, 0).await.expect("client");
        last = client.replication_info().await.expect("INFO replication");
        ok(&last).then(|| last.clone())
    })
    .await
    {
        return info;
    }
    panic!("{what}: {last:?}");
}

/// Standalone replication on a pair of its own: both sides of the link as
/// `INFO replication` shows them, then the changes the Topology page makes
/// — promote the replica, link it back, and (6.2+) hand the primary role
/// over with FAILOVER and back again, so a re-run finds the pair as up.sh
/// started it.
#[test]
#[ignore]
fn replication_pair_promotes_relinks_and_fails_over() {
    smol::block_on(async {
        let primary_addr = skip_unless!("ZEDIS_IT_REPL_PRIMARY");
        let replica_addr = skip_unless!("ZEDIS_IT_REPL_REPLICA");
        let primary = register(server("it-repl-primary", primary_addr.clone())).await;
        let replica = register(server("it-repl-replica", replica_addr.clone())).await;
        let primary_client = get_connection_manager().get_client(&primary, 0).await.expect("client");
        let replica_client = get_connection_manager().get_client(&replica, 0).await.expect("client");

        let info = wait_replication(&replica, "the replica follows the primary", |i| {
            i.role == ReplicationRole::Replica && i.link_up()
        })
        .await;
        assert_eq!(info.master_addr(), format!("127.0.0.1:{}", primary_addr.1));
        assert!(info.replica_read_only, "a replica is read-only by default: {info:?}");
        let info = wait_replication(&primary, "the primary lists its replica online", |i| {
            i.role == ReplicationRole::Primary && i.replicas.iter().any(|r| r.state == "online")
        })
        .await;
        assert_eq!(info.connected_replicas, 1);
        assert_eq!(info.replicas[0].addr, format!("127.0.0.1:{}", replica_addr.1));
        assert!(!info.master_replid.is_empty());

        // Promote: the replica keeps its data and is a primary of its own.
        replica_client.replicaof_no_one().await.expect("REPLICAOF NO ONE");
        wait_replication(&replica, "promoted", |i| i.role == ReplicationRole::Primary).await;
        wait_replication(&primary, "the primary lost its replica", |i| i.replicas.is_empty()).await;

        // Link it back.
        replica_client
            .replicaof("127.0.0.1", primary_addr.1)
            .await
            .expect("REPLICAOF");
        wait_replication(&replica, "linked back", |i| {
            i.role == ReplicationRole::Replica && i.link_up()
        })
        .await;

        if supports(&primary, floors::FAILOVER).await {
            let info = wait_replication(&primary, "no failover pending", |i| !i.failover_in_progress()).await;
            assert_eq!(info.master_failover_state, "no-failover");
            // With nothing to abort, the abort reaches the primary and is
            // refused for that reason.
            let refused = failover_abort(&ServerDb::new(&primary, 0))
                .await
                .expect_err("no failover to abort");
            assert!(
                refused.to_string().to_lowercase().contains("no failover in progress"),
                "{refused}"
            );
            primary_client
                .failover(Some(("127.0.0.1", replica_addr.1)), false, FAILOVER_TIMEOUT_MS)
                .await
                .expect("FAILOVER");
            wait_replication(&replica, "the replica took over", |i| {
                i.role == ReplicationRole::Primary
            })
            .await;
            wait_replication(&primary, "the old primary follows it", |i| {
                i.role == ReplicationRole::Replica && i.link_up()
            })
            .await;
            // And back.
            replica_client
                .failover(Some(("127.0.0.1", primary_addr.1)), false, FAILOVER_TIMEOUT_MS)
                .await
                .expect("FAILOVER back");
            wait_replication(&primary, "the primary is back", |i| i.role == ReplicationRole::Primary).await;
            wait_replication(&replica, "the replica follows again", |i| {
                i.role == ReplicationRole::Replica && i.link_up()
            })
            .await;
        } else {
            eprintln!("skipped the FAILOVER half: the server predates 6.2");
        }
    });
}
