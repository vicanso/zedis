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

//! A cluster read and written as one keyspace: discovery, scans across
//! masters, events from every master, the database count.

use super::keys::check_batch_ttl_conditions;
use super::*;

/// Keyspace notifications are published by the node that holds the key, so
/// a subscription on one node heard about a third of a three-master
/// cluster's keys. `SubscribeKind::KeyspaceEvents` subscribes on every
/// master and hears them all.
#[test]
#[ignore]
fn cluster_keyspace_events_come_from_every_master() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-keyspace", addr)).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        // Routed to every node by the cluster client.
        cmd("CONFIG")
            .arg("SET")
            .arg("notify-keyspace-events")
            .arg("K$")
            .exec_async(&mut c)
            .await
            .expect("enable notifications");
        let prefix = unique("ks-events");
        let pattern = format!("__keyspace@0__:{prefix}:*");
        let mut subscription = ChannelSubscription::open(&at, SubscribeKind::KeyspaceEvents, &[pattern.as_str()])
            .await
            .expect("subscribe on every master");
        let keys: Vec<String> = (0..20).map(|i| format!("{prefix}:{i}")).collect();
        for key in &keys {
            cmd("SET").arg(key).arg("v").exec_async(&mut c).await.expect("set");
        }
        let mut heard = HashSet::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while heard.len() < keys.len() && std::time::Instant::now() < deadline {
            let next = smol::future::or(async { subscription.next_message().await }, async {
                smol::Timer::after(std::time::Duration::from_millis(200)).await;
                None
            })
            .await;
            if let Some(message) = next {
                heard.insert(message.channel.trim_start_matches("__keyspace@0__:").to_string());
            }
        }
        assert_eq!(
            heard.len(),
            keys.len(),
            "an event for every key, whichever master holds it"
        );

        drop(subscription);
        cmd("CONFIG")
            .arg("SET")
            .arg("notify-keyspace-events")
            .arg("")
            .exec_async(&mut c)
            .await
            .expect("disable notifications");
        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

/// A chunk of keys spans hash slots, and redis-rs refuses a cluster pipeline
/// that does. The conflict preview (`EXISTS` per key) and the readable
/// export (`TYPE` + `PTTL` per key) used to send the whole chunk as one
/// pipeline, so on a cluster the first chunk failed; they now go key by key
/// there (`RedisClient::pipeline_per_key`).
#[test]
#[ignore]
fn cluster_chunk_reads_span_slots() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-chunks", addr)).await;
        let prefix = unique("chunks");
        let keys: Vec<String> = (0..20).map(|i| format!("{prefix}:{i}")).collect();
        let mut c = conn(&id, 0).await;
        for key in &keys {
            cmd("SET").arg(key).arg("v").exec_async(&mut c).await.expect("set");
        }
        let missing = format!("{prefix}:missing");

        let mut asked = keys.clone();
        asked.push(missing.clone());
        let cancel = AtomicBool::new(false);
        let preview = preview_key_conflicts(&id, 0, &asked, 5, &cancel)
            .await
            .expect("a preview across slots");
        assert_eq!((preview.conflicting, preview.free), (20, 1));

        let at = ServerDb::new(&id, 0);
        let entries = read_readable_chunk(&at, &asked, ReadLimits::default())
            .await
            .expect("an export chunk across slots");
        assert_eq!(entries.len(), 20, "the missing key is dropped, the rest read");

        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

#[test]
#[ignore]
fn cluster_discovers_nodes_and_scans_every_master() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster", addr)).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("cluster client");
        assert_eq!(client.nodes(), (3, 6), "3 masters out of 6 nodes");
        assert_eq!(client.nodes_description().server_type, "Cluster");

        // Keys without hash tags spread across slots, so a full SCAN has to
        // visit every master to find them all.
        let prefix = unique("cluster");
        let mut c = conn(&id, 0).await;
        let keys: Vec<String> = (0..30).map(|i| format!("{prefix}:{i}")).collect();
        for key in &keys {
            cmd("SET").arg(key).arg("x").exec_async(&mut c).await.expect("set");
        }
        let mut found = HashSet::new();
        let mut cursors = None;
        loop {
            let (next, page) = client
                .scan(cursors, &format!("{prefix}:*"), 10, false, None)
                .await
                .expect("scan");
            found.extend(page.into_iter().map(|(k, _, _)| k));
            if next.iter().sum::<u64>() == 0 {
                break;
            }
            cursors = Some(next);
        }
        assert_eq!(found.len(), keys.len(), "every key on every master");
        // The same walk narrowed by a regex. Each master's page loses
        // different rows, and the TYPE / TTL pipeline is built per master
        // from what is left: a row has to come back with its own key's
        // answers, not its neighbour's.
        cmd("EXPIRE")
            .arg(&keys[4])
            .arg(600)
            .exec_async(&mut c)
            .await
            .expect("expire");
        let even = Regex::new(&format!(r"^{}:\d*[02468]$", regex::escape(&prefix))).expect("regex");
        let mut kept: Vec<(String, String, i64)> = Vec::new();
        let mut cursors = None;
        loop {
            let (next, page) = client
                .scan_matching(cursors, &format!("{prefix}:*"), 10, true, None, Some(&even))
                .await
                .expect("scan");
            kept.extend(page);
            if next.iter().sum::<u64>() == 0 {
                break;
            }
            cursors = Some(next);
        }
        let names: HashSet<&str> = kept.iter().map(|(key, _, _)| key.as_str()).collect();
        let expected: HashSet<&str> = keys.iter().step_by(2).map(String::as_str).collect();
        assert_eq!(names, expected, "the even-numbered keys and no others");
        for (key, kind, ttl) in &kept {
            assert_eq!(kind, "string", "{key}");
            if *key == keys[4] {
                assert!((1..=600).contains(ttl), "{key} is the one that expires: {ttl}");
            } else {
                assert_eq!(*ttl, -1, "{key} has no expiry");
            }
        }
        let total = client.dbsize().await.expect("dbsize");
        assert!(total >= keys.len() as u64, "dbsize sums the masters: {total}");
        // The routed DBSIZE (redis-rs fans it out and sums) must agree with
        // asking each master ourselves — the count the status bar shows
        // against the scanned keys. The other cluster tests run in parallel
        // and write keys between the two snapshots, so on a mismatch take
        // both again rather than fail on a one-key race.
        let mut agreed = false;
        for _ in 0..50 {
            let routed = client.dbsize().await.expect("dbsize");
            let (_, per_master): (_, Vec<u64>) = client
                .query_async_masters(vec![cmd("DBSIZE")])
                .await
                .expect("per-master dbsize");
            if routed == per_master.iter().sum::<u64>() {
                agreed = true;
                break;
            }
            // Back-to-back retries can all land inside another test's burst
            // of writes; a short pause lets one pair fall between them.
            smol::Timer::after(std::time::Duration::from_millis(20)).await;
        }
        assert!(agreed, "routed DBSIZE sums every master");

        let features = probe_server_features(&id, 0).await.expect("probe");
        assert_eq!(features.status(ServerCommand::ClusterInfo), CommandStatus::Available);
        assert_eq!(features.status(ServerCommand::Scan), CommandStatus::Available);

        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

/// The database count a cluster client reports is the one `SELECT`
/// accepts: `cluster-databases` where the server has it (Valkey 9, default
/// 1), else 1 — never `databases`, which Redis 6.2 leaves at 16 in cluster
/// mode while refusing every `SELECT`. Found on a default Valkey 9.0
/// cluster first: it keeps `databases` at 16
/// and answers `SELECT 1` with "DB index is out of range", and the switcher
/// listed fifteen databases that did not exist. With more than one
/// database (`--cluster-databases 16`), a key written to the last one is
/// found by a scan of that database — the per-master fan-out selects it —
/// and not by a scan of db 0.
#[test]
#[ignore]
fn cluster_database_count_is_the_one_select_accepts() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-databases", addr)).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("cluster client");
        let at = ServerDb::new(&id, 0);
        let expected = match config_get_one(&at, "cluster-databases")
            .await
            .expect("CONFIG GET cluster-databases")
        {
            Some(count) => count.parse::<usize>().expect("cluster-databases is a count"),
            None => 1,
        };
        assert_eq!(client.databases(), expected, "the switcher offers what SELECT accepts");

        // One past the end is refused, at the handshake or on first use.
        let beyond = get_connection_manager().get_client(&id, expected).await;
        let refused = match beyond {
            Err(_) => true,
            Ok(client) => client.dbsize().await.is_err(),
        };
        assert!(refused, "db {expected} does not exist on this cluster");
        get_connection_manager().remove_client(&id, expected);

        if expected == 1 {
            eprintln!("skipped the multi-database half: this cluster has db 0 alone");
            return;
        }
        let last = expected - 1;
        let key = unique("cluster-db");
        let mut c = conn(&id, last).await;
        cmd("SET")
            .arg(&key)
            .arg("x")
            .exec_async(&mut c)
            .await
            .expect("set on the last db");
        let found_on = |db: usize| {
            let id = id.clone();
            let pattern = format!("{key}*");
            async move {
                let client = get_connection_manager().get_client(&id, db).await.expect("client");
                let mut found = false;
                let mut cursors = None;
                loop {
                    let (next, page) = client.scan(cursors, &pattern, 10, false, None).await.expect("scan");
                    found |= !page.is_empty();
                    if next.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(next);
                }
                found
            }
        };
        assert!(
            found_on(last).await,
            "the scan of db {last} fans out with that db selected"
        );
        assert!(!found_on(0).await, "db 0 does not see a key written to db {last}");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// Same contract on a cluster, where the two keys land in different slots
/// and the helper fans out per key: the flags must still come back in the
/// caller's order (the folder batch aligns its TTL cache on that).
#[test]
#[ignore]
fn cluster_batch_ttl_conditions_keep_key_order() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-ttl-cond-cluster", addr)).await;
        check_batch_ttl_conditions(&id).await;
    });
}

/// A cluster connection sends `LATENCY` and `FUNCTION STATS` to every node
/// and answers a map of node to reply: the panels read that, and count a
/// library loaded on every node once.
#[test]
#[ignore]
fn cluster_latency_and_function_stats_read_every_node() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-per-node", addr)).await;
        let at = ServerDb::new(&*id, 0);
        let listing = latency_latest(&at).await.expect("latency latest");
        assert!(!listing.unsupported, "every node has LATENCY");
        latency_history(&at, "command").await.expect("latency history");

        if !supports(&id, floors::FUNCTIONS).await {
            eprintln!("skipped: functions are Redis 7.0+");
            return;
        }
        let library = format!("zedis_it_cluster_{}", std::process::id());
        let code = format!("#!lua name={library}\nredis.register_function('{library}_f', function() return 1 end)");
        let before = function_stats(&at).await.expect("stats").libraries_count;
        function_load(&at, &code, true).await.expect("load on every master");
        let stats = function_stats(&at).await.expect("stats");
        function_delete(&at, &library).await.expect("delete");
        assert_eq!(
            stats.libraries_count,
            before + 1,
            "one node's count, not none and not the sum over nodes: {stats:?}"
        );
    });
}

/// Pipelined benchmarks (`P=16`, `P=64`) on a multi-master Redis Cluster use
/// 16-way `{s0}`..`{s15}` hash tags so every batch routes cleanly without
/// `CROSSSLOT` errors, and `cleanup_touched_keys` (`unlike_keys_scattered`)
/// removes all throwaway keys across all cluster shards on completion and on
/// mid-run cancellation.
#[test]
#[ignore]
fn cluster_benchmark_pipelined_and_cancel_leave_zero_throwaway_keys() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-benchmark", addr)).await;
        let at = ServerDb::new(&id, 0);

        for (preset, pipeline) in [
            (BenchmarkPreset::Set, 16),
            (BenchmarkPreset::Get, 16),
            (BenchmarkPreset::Hset, 16),
        ] {
            let cancel = AtomicBool::new(false);
            let cfg = BenchmarkConfig {
                preset,
                concurrency: 2,
                payload_size: 64,
                keyspace_size: 64,
                duration_secs: 1,
                pipeline,
            };
            let prefix = new_benchmark_prefix();
            let result = run_benchmark_with_prefix(&at, &cfg, prefix.clone(), &cancel, |_| {})
                .await
                .expect("cluster pipelined benchmark");
            assert!(result.summary.total_ops > 0, "{preset:?}");
            assert_eq!(result.summary.errors, 0, "{preset:?} had CROSSSLOT or cluster errors");
            assert!(result.summary.cleaned_keys > 0, "{preset:?}");
            assert!(!result.summary.cancelled, "{preset:?}");

            let leftover = count_keys_matching(&at, &format!("{prefix}*"))
                .await
                .expect("count cluster leftover keys");
            assert_eq!(leftover.keys, 0, "{preset:?} left throwaway keys on cluster");
        }

        // Mid-run cancellation with P=64 across cluster shards
        let cancel = AtomicBool::new(false);
        let cfg = BenchmarkConfig {
            preset: BenchmarkPreset::Set,
            concurrency: 2,
            payload_size: 128,
            keyspace_size: 128,
            duration_secs: 10,
            pipeline: 64,
        };
        let prefix = new_benchmark_prefix();
        let result = run_benchmark_with_prefix(&at, &cfg, prefix.clone(), &cancel, |_| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .await
        .expect("cancelled cluster benchmark");
        assert!(result.summary.cancelled);
        assert_eq!(result.summary.errors, 0);
        let leftover = count_keys_matching(&at, &format!("{prefix}*"))
            .await
            .expect("count cluster leftover keys after cancel");
        assert_eq!(leftover.keys, 0, "cancelled P=64 run left throwaway keys on cluster");
    });
}
