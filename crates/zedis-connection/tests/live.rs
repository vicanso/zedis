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

//! Live integration tests against real Redis-compatible servers.
//!
//! Every test is `#[ignore]`: the unit suite (`make test`) never needs a
//! server. Start the topology with `make it-up` (local `redis-server`, or
//! `REDIS_IMAGE=redis:7.2 make it-up` for docker), then `make it`. Each
//! scenario reads its own `ZEDIS_IT_*` variable and skips — loudly — when
//! it is unset, so a partial topology (standalone only) still runs.
//!
//! CI runs the full matrix in `.github/workflows/integration.yml`.

use redis::{FromRedisValue, cmd};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Once;
use std::sync::atomic::AtomicBool;
use zedis_connection::error::ConnectionErrorKind;
use zedis_connection::floors::{self, Floor};
use zedis_connection::{
    AclDryRun, BitOpKind, ChannelSubscription, ClusterNode, CommandLogKind, CommandStatus, CompareOptions, CompareSide,
    ConflictMode, ExpireCondition, FAILOVER_TIMEOUT_MS, FieldTtl, FromEnd, GeoShape, HeatMetric, HeatProbe,
    HllEncoding, ImportFormat, KeyDifference, KeyOp, KeyOpOutcome, KillFilter, KillOutcome, KillTarget, PauseMode,
    ProbKind, ProbeOutcome, PubsubChannel, ReadLimits, ReadableValue, ReadableWriteStatus, RedisAsyncConn, RedisServer,
    ReplicationInfo, ReplicationRole, ReplyFormat, RestoreStatus, SERVER_TYPE_SENTINEL, SearchOptions, ServerCommand,
    ServerDb, ServerFlavor, SlotStatMetric, StreamGroup, StreamTail, StreamTrim, StringWrite, SubscribeKind,
    TerminalSession, TsAlter, TsMRange, VectorSimOptions, acl_del_user, acl_dryrun, acl_file, acl_genpass,
    acl_get_user, acl_log, acl_log_reset, acl_save, acl_set_user, acl_whoami, bgrewriteaof, bgsave, bit_field, bit_op,
    bitmap_info, client_kill_by, client_kill_id, client_list, client_pause, client_unpause,
    cluster_get_slot_migrations, cluster_migrate_slots, command_log_reset, command_logs, compare_prefix,
    config_get_all, config_get_named, config_get_one, config_load, config_resetstat, config_rewrite, config_set,
    consumer_create, consumer_delete, create_key, csv_header, dbsize, delete_key, delete_keys, delete_keys_matching,
    dump_key, dump_keys_chunk, entry_to_csv, entry_to_json, expire_key, expire_key_at, forget_client, ft_explain,
    ft_info, ft_search, ft_spellcheck, ft_tagvals, geo_add, geo_dist, geo_sample, geo_search, get_connection_manager,
    get_server, get_server_heat_probe, get_servers, group_create, group_destroy, group_set_id, hash_delete_fields,
    hash_field_ttls, hash_len, hash_scan, heartbeat_probe, hll_info, info_everything, key_bytes, key_memory_usage,
    key_object_meta, key_type_and_ttl, key_types, kill_filter_commands, kill_running, latency_history, latency_latest,
    latency_monitor_threshold, latency_reset, list_len, list_push, list_range, list_set_if_unchanged, master_addrs,
    master_infos, maxmemory_policy, node_add_slots, node_cancel_slot_migrations, node_failover, node_load,
    node_replicate, node_slot_migrations, node_stabilize_slot, open_monitor_feeds, open_single_connection,
    parse_readable_entries, pending_page, pf_add, pf_merge, plan_cluster_rebalance, preview_key_conflicts, prob_info,
    prob_probe, probe_server_features, read_readable_chunk, remove_list_indexes, rename_hash_field, rename_key,
    restore_key, restore_keys_chunk, run_key_op, run_script, save_servers, scan_page, script_exists, script_load,
    script_sha1, sentinel_ckquorum, sentinel_flushconfig, sentinel_master_names, sentinel_masters, sentinel_monitor,
    sentinel_remove, sentinel_set, server_summary, server_supports, set_add, set_bit, set_card, set_keys_ttl,
    set_remove, set_replace_member, set_scan, set_ttl_matching, slow_logs, snapshot_key, sniff_import_format,
    split_acl_rules, stream_ack, stream_add, stream_autoclaim, stream_claim, stream_delete, stream_info, stream_len,
    stream_page, stream_set_id, stream_trim, string_get, string_set, test_connection, ts_add, ts_alter, ts_create_rule,
    ts_delete_rule, ts_mrange, ts_window, unassigned_slot_ranges, value_preview, vset_info, vset_remove, vset_set_attr,
    vset_sim, write_hash_field, write_readable_chunk, zset_card, zset_count_by_score, zset_looks_geo, zset_put,
    zset_range, zset_range_by_score, zset_remove, zset_scan,
};
use zedis_core::json::JsonPathOp;
use zedis_core::keysizes::KeysizesUnit;
use zedis_core::search_params::{ParamKind, encode_param};

/// `host:port` from an env var, or `None` when that scenario wasn't started.
fn scenario(var: &str) -> Option<(String, u16)> {
    let value = env::var(var).ok()?;
    let (host, port) = value.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// Standalone is the one scenario every run has; a missing variable is a
/// harness mistake, not a skip.
fn standalone() -> (String, u16) {
    scenario("ZEDIS_IT_STANDALONE").expect("ZEDIS_IT_STANDALONE=host:port — run scripts/it/up.sh and `make it`")
}

macro_rules! skip_unless {
    ($var:literal) => {
        match scenario($var) {
            Some(addr) => addr,
            None => {
                eprintln!("skipped: {} not set", $var);
                return;
            }
        }
    };
}

/// One isolated config dir per test process: `save_servers` writes the
/// server list (and the encryption key file) there, never to the real one.
fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = env::temp_dir().join(format!("zedis-it-{}", std::process::id()));
        zedis_core::fs::override_config_dir(dir);
    });
}

/// `save_servers` replaces the whole list and tests run in parallel —
/// serialise the read-append-write (an async lock: it is held across the
/// save's await).
static REGISTER: smol::lock::Mutex<()> = smol::lock::Mutex::new(());

/// Held by every test that changes cluster slot state. They share one
/// cluster, and the states are mutually exclusive on the server: a node
/// that is importing a slot atomically refuses `SETSLOT … IMPORTING`
/// with "Slot import in progress".
static CLUSTER_SLOTS: smol::lock::Mutex<()> = smol::lock::Mutex::new(());

async fn register(server: RedisServer) -> String {
    isolate();
    let _guard = REGISTER.lock().await;
    let id = server.id.clone();
    let mut servers = get_servers().unwrap_or_default();
    servers.retain(|s| s.id != id);
    servers.push(server);
    save_servers(servers).await.expect("save servers");
    id
}

fn server(id: &str, (host, port): (String, u16)) -> RedisServer {
    RedisServer {
        id: id.to_string(),
        name: id.to_string(),
        host,
        port,
        ..Default::default()
    }
}

/// The password the harness put on the sentinel's data nodes and on every
/// cluster node. Absent only when those scenarios were not started, in
/// which case nothing that needs it runs either.
fn data_password() -> Option<String> {
    env::var("ZEDIS_IT_PASSWORD").ok().filter(|p| !p.is_empty())
}

/// An entry for a data node behind the harness's password: every cluster
/// node (including on a MOVED redirect) and the masters and replicas the
/// sentinel watches.
fn protected_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        password: data_password(),
        ..server(id, addr)
    }
}

/// A Sentinel entry: the data nodes' password *and* the sentinel's own,
/// which the harness deliberately makes different. `password` reaches the
/// master Sentinel points at, `sentinel_password` the sentinels themselves
/// — the split `sentinel_login()` exists for.
///
/// `server_type` is left on auto, which is what the connection dialog
/// produces by default, so discovery reaches the sentinel through
/// `open_seed_endpoint`'s retry rather than its declared-Sentinel
/// shortcut. `sentinel_declared_server` covers the other branch.
fn sentinel_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        password: data_password(),
        sentinel_password: env::var("ZEDIS_IT_SENTINEL_PASSWORD").ok().filter(|p| !p.is_empty()),
        ..server(id, addr)
    }
}

/// The same entry with the type declared, which takes the sentinel's
/// credentials straight to the seed instead of discovering them from a
/// refused AUTH.
fn sentinel_declared_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        server_type: Some(SERVER_TYPE_SENTINEL),
        ..sentinel_server(id, addr)
    }
}

async fn conn(id: &str, db: usize) -> RedisAsyncConn {
    get_connection_manager()
        .get_connection(id, db)
        .await
        .expect("connection")
}

/// Unique per process and call, so parallel tests (and re-runs against a
/// server that wasn't flushed) never share a key or ACL user name.
fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "zedis:it:{prefix}:{}:{nanos}:{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Version gate for a test: the same [`Floor`] the app uses, so a test
/// never carries its own (flavor-blind) version string.
async fn supports(id: &str, floor: Floor) -> bool {
    get_connection_manager()
        .get_client(id, 0)
        .await
        .expect("client")
        .supports(floor)
}

// ── standalone ───────────────────────────────────────────────────────────

/// The same connection code works under a tokio runtime, which is what the
/// HTTP bridge will run on (ADR 9).
///
/// redis is built with both runtime adapters and picks per connection: tokio
/// when `Handle::try_current()` finds an ambient runtime, smol otherwise. Every
/// other test in this file runs outside one and therefore only ever proves the
/// smol half. This one proves the other, so the bridge's premise cannot rot
/// silently — a regression here surfaces as "there is no reactor running",
/// not as a subtly different code path.
#[test]
#[ignore]
fn standalone_connects_from_inside_a_tokio_runtime() {
    let (host, port) = skip_unless!("ZEDIS_IT_STANDALONE");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let id = register(server("it-tokio-runtime", (host, port))).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("client built under tokio");
        client.ping().await.expect("ping under tokio");

        // A write and a read back, so this is a real round trip through the
        // tokio socket and not just a handshake.
        let key = unique("tokio-rt");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("through-tokio")
            .exec_async(&mut c)
            .await
            .expect("set under tokio");
        let value: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("get under tokio");
        assert_eq!(value, "through-tokio");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("cleanup");
    });
}

#[test]
#[ignore]
fn standalone_connect_reports_metadata() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping");
        assert!(!client.version().is_empty(), "version must be read from INFO server");
        if supports(&id, floors::CLIENT_SETINFO).await {
            let mut c = conn(&id, 0).await;
            let info: String = cmd("CLIENT")
                .arg("INFO")
                .query_async(&mut c)
                .await
                .expect("client info");
            assert!(
                info.contains("lib-name=zedis"),
                "CLIENT SETINFO must name the client: {info}"
            );
            assert!(info.contains("name=zedis:v"), "CLIENT SETNAME must still apply: {info}");
        }
        assert!(client.databases() >= 1);
        assert_eq!(client.nodes(), (1, 1), "standalone: one master, one node in total");
        assert_eq!(
            format!("{:?}", client.access_mode()),
            "ReadWrite",
            "the default user is not read-only"
        );
        client.dbsize().await.expect("dbsize");
    });
}

#[test]
#[ignore]
fn standalone_scan_sees_every_type_it_wrote() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;
        let prefix = unique("types");
        let keys = [
            (format!("{prefix}:s"), "string"),
            (format!("{prefix}:h"), "hash"),
            (format!("{prefix}:l"), "list"),
            (format!("{prefix}:z"), "zset"),
            (format!("{prefix}:set"), "set"),
        ];
        cmd("SET")
            .arg(&keys[0].0)
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("set");
        cmd("HSET")
            .arg(&keys[1].0)
            .arg("f")
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("RPUSH")
            .arg(&keys[2].0)
            .arg("a")
            .exec_async(&mut c)
            .await
            .expect("rpush");
        cmd("ZADD")
            .arg(&keys[3].0)
            .arg(1)
            .arg("m")
            .exec_async(&mut c)
            .await
            .expect("zadd");
        cmd("SADD")
            .arg(&keys[4].0)
            .arg("m")
            .exec_async(&mut c)
            .await
            .expect("sadd");
        cmd("EXPIRE")
            .arg(&keys[0].0)
            .arg(600)
            .exec_async(&mut c)
            .await
            .expect("expire");

        // Page until every cursor is 0 — the tree's own loop.
        let mut found: Vec<(String, String, i64)> = Vec::new();
        let mut cursors = None;
        loop {
            let (next, page) = client
                .scan(cursors, &format!("{prefix}:*"), 100, true, None)
                .await
                .expect("scan");
            found.extend(page);
            if next.iter().sum::<u64>() == 0 {
                break;
            }
            cursors = Some(next);
        }
        let by_name: HashSet<(String, String)> = found.iter().map(|(k, t, _)| (k.clone(), t.clone())).collect();
        for (key, kind) in &keys {
            assert!(
                by_name.contains(&(key.clone(), kind.to_string())),
                "missing {key} as {kind}: {found:?}"
            );
        }
        let ttl = found
            .iter()
            .find(|(k, _, _)| k == &keys[0].0)
            .map(|(_, _, ttl)| *ttl)
            .expect("ttl row");
        assert!((1..=600).contains(&ttl), "SCAN with_ttl must carry the TTL, got {ttl}");

        // The server-side TYPE filter (6.0+) and the client-side fallback agree.
        let (_, only_hashes) = client
            .first_scan(&format!("{prefix}:*"), 100, false, Some("hash"))
            .await
            .expect("scan hash");
        assert_eq!(only_hashes.len(), 1);
        assert_eq!(only_hashes[0].0, keys[1].0);

        assert_eq!(client.key_type(&keys[2].0).await.expect("type"), "list");
        assert_eq!(client.get_key_bytes(&keys[0].0).await.expect("get"), b"v");
        assert!(client.memory_usage(&keys[1].0, "hash").await.expect("memory usage") > 0);

        for (key, _) in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

#[test]
#[ignore]
fn standalone_bulk_delete_removes_every_key() {
    // The verb is picked per server (`UNLINK` from 4.0, `DEL` before —
    // floors::UNLINK); every CI server clears the floor, so this pins the
    // pipeline path and the unit test in floors.rs pins the fallback.
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let keys: Vec<String> = (0..3).map(|i| unique(&format!("bulk{i}"))).collect();
        for key in &keys {
            cmd("SET").arg(key).arg("x").exec_async(&mut c).await.expect("set");
        }
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.unlike_keys_scattered(keys.clone()).await.expect("bulk delete");
        let left: i64 = cmd("EXISTS").arg(&keys).query_async(&mut c).await.expect("exists");
        assert_eq!(left, 0, "bulk delete must remove every key");
    });
}

#[test]
#[ignore]
fn standalone_dump_restore_round_trips_a_key() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("dump");
        cmd("HSET")
            .arg(&key)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("EXPIRE")
            .arg(&key)
            .arg(300)
            .exec_async(&mut c)
            .await
            .expect("expire");
        let entries = dump_keys_chunk(&at, std::slice::from_ref(&key)).await.expect("dump");
        assert_eq!(entries.len(), 1);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");

        let statuses = restore_keys_chunk(&at, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Written), "{statuses:?}");
        let b: String = cmd("HGET").arg(&key).arg("b").query_async(&mut c).await.expect("hget");
        assert_eq!(b, "2");
        let ttl: i64 = cmd("TTL").arg(&key).query_async(&mut c).await.expect("ttl");
        assert!((1..=300).contains(&ttl), "RESTORE must carry the TTL over, got {ttl}");

        // Skip leaves an existing key alone; Overwrite replaces it.
        cmd("HSET")
            .arg(&key)
            .arg("b")
            .arg("changed")
            .exec_async(&mut c)
            .await
            .expect("hset");
        let statuses = restore_keys_chunk(&at, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Skipped), "{statuses:?}");
        let statuses = restore_keys_chunk(&at, &entries, ConflictMode::Overwrite)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Written), "{statuses:?}");
        let b: String = cmd("HGET").arg(&key).arg("b").query_async(&mut c).await.expect("hget");
        assert_eq!(b, "2");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// The readable (JSON/CSV) export must page oversized collections and cut
/// them at `max_elems` with the entry marked truncated — never one
/// unbounded `LRANGE 0 -1` / `SMEMBERS` / `HGETALL` / `XRANGE - +`.
#[test]
#[ignore]
fn standalone_readable_export_pages_and_caps_collections() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;

        let small_list = unique("rd-small");
        let big_list = unique("rd-list");
        let big_set = unique("rd-set");
        let big_hash = unique("rd-hash");
        let big_zset = unique("rd-zset");
        let stream = unique("rd-stream");

        let mut rpush = cmd("RPUSH");
        rpush.arg(&small_list);
        for i in 0..3 {
            rpush.arg(format!("s{i}"));
        }
        rpush.exec_async(&mut c).await.expect("rpush small");

        let mut rpush = cmd("RPUSH");
        rpush.arg(&big_list);
        for i in 0..25 {
            rpush.arg(format!("v{i}"));
        }
        rpush.exec_async(&mut c).await.expect("rpush big");

        // Members/values past *-max-listpack-value (64 bytes, a default
        // stable across server versions — the entry-count threshold is
        // not: Redis 8.6 raised hash-max-listpack-entries to 512), so the
        // set/hash are hashtable-encoded and SSCAN/HSCAN honor COUNT
        // instead of returning the whole listpack in one page.
        let pad = "x".repeat(80);
        let mut sadd = cmd("SADD");
        sadd.arg(&big_set);
        for i in 0..200 {
            sadd.arg(format!("m{i}:{pad}"));
        }
        sadd.exec_async(&mut c).await.expect("sadd");

        let mut hset = cmd("HSET");
        hset.arg(&big_hash);
        for i in 0..200 {
            hset.arg(format!("f{i}")).arg(format!("w{i}:{pad}"));
        }
        hset.exec_async(&mut c).await.expect("hset");

        let mut zadd = cmd("ZADD");
        zadd.arg(&big_zset);
        for i in 0..25 {
            zadd.arg(i).arg(format!("m{i}"));
        }
        zadd.exec_async(&mut c).await.expect("zadd");

        for i in 0..25 {
            cmd("XADD")
                .arg(&stream)
                .arg("*")
                .arg("n")
                .arg(i)
                .exec_async(&mut c)
                .await
                .expect("xadd");
        }

        let keys: Vec<String> = vec![
            small_list.clone(),
            big_list.clone(),
            big_set.clone(),
            big_hash.clone(),
            big_zset.clone(),
            stream.clone(),
        ];
        let limits = ReadLimits {
            page: 10,
            max_elems: 20,
        };
        let entries = read_readable_chunk(&at, &keys, limits).await.expect("read chunk");
        assert_eq!(entries.len(), keys.len());
        let entry = |key: &str| entries.iter().find(|e| e.key == key).expect("entry for key");

        // Under one page: the exact single-command path, complete.
        let small = entry(&small_list);
        assert!(!small.truncated);
        match small.value.as_ref().expect("small list value") {
            ReadableValue::List(items) => assert_eq!(items.as_slice(), ["s0", "s1", "s2"]),
            _ => panic!("expected a list value"),
        }

        // Index paging keeps list order; the cut lands exactly at the cap.
        let list = entry(&big_list);
        assert!(list.truncated);
        match list.value.as_ref().expect("list value") {
            ReadableValue::List(items) => {
                assert_eq!(items.len(), 20);
                assert_eq!(items.first().map(String::as_str), Some("v0"));
                assert_eq!(items.last().map(String::as_str), Some("v19"));
            }
            _ => panic!("expected a list value"),
        }

        // SSCAN paging: capped, and (no concurrent rehash here) unique.
        let set = entry(&big_set);
        assert!(set.truncated);
        match set.value.as_ref().expect("set value") {
            ReadableValue::Set(items) => {
                assert_eq!(items.len(), 20);
                let distinct: HashSet<&str> = items.iter().map(String::as_str).collect();
                assert_eq!(distinct.len(), 20);
                assert!(items.iter().all(|m| m.starts_with('m')));
            }
            _ => panic!("expected a set value"),
        }

        // HSCAN paging: capped, field/value pairing intact.
        let hash = entry(&big_hash);
        assert!(hash.truncated);
        match hash.value.as_ref().expect("hash value") {
            ReadableValue::Hash(pairs) => {
                assert_eq!(pairs.len(), 20);
                for (field, value) in pairs {
                    let index = field.strip_prefix('f').expect("field name");
                    assert_eq!(value.as_str(), format!("w{index}:{pad}"));
                }
            }
            _ => panic!("expected a hash value"),
        }

        // ZRANGE index paging keeps ascending score order exactly.
        let zset = entry(&big_zset);
        assert!(zset.truncated);
        match zset.value.as_ref().expect("zset value") {
            ReadableValue::Zset(pairs) => {
                assert_eq!(pairs.len(), 20);
                assert_eq!(pairs.first().map(|(m, s)| (m.as_str(), *s)), Some(("m0", 0.0)));
                assert_eq!(pairs.last().map(|(m, s)| (m.as_str(), *s)), Some(("m19", 19.0)));
            }
            _ => panic!("expected a zset value"),
        }

        // XRANGE id paging: capped, ids stay strictly ascending.
        let stream_entry = entry(&stream);
        assert!(stream_entry.truncated);
        match stream_entry.value.as_ref().expect("stream value") {
            ReadableValue::Stream(items) => {
                assert_eq!(items.len(), 20);
                // Compare ids numerically: within one millisecond,
                // "…-9" < "…-10" is false as a string.
                let id_parts = |id: &str| -> (u64, u64) {
                    let (ms, seq) = id.split_once('-').expect("ms-seq id");
                    (ms.parse().expect("ms"), seq.parse().expect("seq"))
                };
                assert!(items.windows(2).all(|w| id_parts(&w[0].0) < id_parts(&w[1].0)));
                assert_eq!(
                    items.first().and_then(|(_, f)| f.first().cloned()),
                    Some(("n".into(), "0".into()))
                );
                assert_eq!(
                    items.last().and_then(|(_, f)| f.first().cloned()),
                    Some(("n".into(), "19".into()))
                );
            }
            _ => panic!("expected a stream value"),
        }

        let mut del = cmd("DEL");
        for key in &keys {
            del.arg(key);
        }
        del.exec_async(&mut c).await.expect("del");
    });
}

/// A readable JSON/CSV export must import back: same values, order, TTL —
/// with Skip leaving existing keys alone (no list double-append) and
/// Overwrite replacing instead of appending.
#[test]
#[ignore]
fn standalone_readable_export_imports_back() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;

        let s_key = unique("ri-s");
        let l_key = unique("ri-l");
        let set_key = unique("ri-set");
        let h_key = unique("ri-h");
        let z_key = unique("ri-z");
        let x_key = unique("ri-x");
        let keys: Vec<String> = vec![
            s_key.clone(),
            l_key.clone(),
            set_key.clone(),
            h_key.clone(),
            z_key.clone(),
            x_key.clone(),
        ];

        cmd("SET")
            .arg(&s_key)
            .arg("hello \"world\",\nline2")
            .arg("PX")
            .arg(300_000)
            .exec_async(&mut c)
            .await
            .expect("set");
        cmd("RPUSH")
            .arg(&l_key)
            .arg("a")
            .arg("b")
            .arg("c")
            .exec_async(&mut c)
            .await
            .expect("rpush");
        cmd("SADD")
            .arg(&set_key)
            .arg("m1")
            .arg("m2")
            .exec_async(&mut c)
            .await
            .expect("sadd");
        cmd("HSET")
            .arg(&h_key)
            .arg("f1")
            .arg("v1")
            .arg("f2")
            .arg("v2")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("ZADD")
            .arg(&z_key)
            .arg(1.5)
            .arg("m1")
            .arg(2.5)
            .arg("m2")
            .exec_async(&mut c)
            .await
            .expect("zadd");
        for i in 0..3 {
            cmd("XADD")
                .arg(&x_key)
                .arg("*")
                .arg("n")
                .arg(i)
                .exec_async(&mut c)
                .await
                .expect("xadd");
        }

        let exported = read_readable_chunk(&at, &keys, ReadLimits::default())
            .await
            .expect("export");
        assert_eq!(exported.len(), keys.len());
        let original_stream_ids: Vec<String> = match &exported[5].value {
            Some(ReadableValue::Stream(items)) => items.iter().map(|(id, _)| id.clone()).collect(),
            other => panic!("expected stream, got {other:?}"),
        };

        let json_doc = serde_json::Value::Array(exported.iter().map(entry_to_json).collect()).to_string();
        let mut csv_doc = csv_header();
        for entry in &exported {
            csv_doc.push_str(&entry_to_csv(entry));
        }

        let mut del = cmd("DEL");
        for key in &keys {
            del.arg(key);
        }
        del.exec_async(&mut c).await.expect("del");

        // JSON import onto a clean db slice.
        assert_eq!(
            sniff_import_format(json_doc.as_bytes()).expect("sniff"),
            ImportFormat::Json
        );
        let entries = parse_readable_entries(&json_doc, ImportFormat::Json).expect("parse json");
        let statuses = write_readable_chunk(&at, &entries, ConflictMode::Skip)
            .await
            .expect("write");
        assert!(
            statuses.iter().all(|s| *s == ReadableWriteStatus::Written),
            "{statuses:?}"
        );

        let s: String = cmd("GET").arg(&s_key).query_async(&mut c).await.expect("get");
        assert_eq!(s, "hello \"world\",\nline2");
        let ttl: i64 = cmd("TTL").arg(&s_key).query_async(&mut c).await.expect("ttl");
        assert!((1..=300).contains(&ttl), "import must restore the TTL, got {ttl}");
        let l: Vec<String> = cmd("LRANGE")
            .arg(&l_key)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(l, ["a", "b", "c"]);
        let members: HashSet<String> = cmd("SMEMBERS")
            .arg(&set_key)
            .query_async(&mut c)
            .await
            .expect("smembers");
        assert_eq!(members.len(), 2);
        let v2: String = cmd("HGET")
            .arg(&h_key)
            .arg("f2")
            .query_async(&mut c)
            .await
            .expect("hget");
        assert_eq!(v2, "v2");
        let score: f64 = cmd("ZSCORE")
            .arg(&z_key)
            .arg("m2")
            .query_async(&mut c)
            .await
            .expect("zscore");
        assert_eq!(score, 2.5);
        let stream: Vec<(String, Vec<(String, String)>)> = cmd("XRANGE")
            .arg(&x_key)
            .arg("-")
            .arg("+")
            .query_async(&mut c)
            .await
            .expect("xrange");
        let imported_ids: Vec<String> = stream.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(imported_ids, original_stream_ids, "XADD must preserve original ids");

        // Skip must leave existing keys alone — especially no RPUSH append.
        let statuses = write_readable_chunk(&at, &entries, ConflictMode::Skip)
            .await
            .expect("write again");
        assert!(
            statuses.iter().all(|s| *s == ReadableWriteStatus::SkippedExists),
            "{statuses:?}"
        );
        let llen: i64 = cmd("LLEN").arg(&l_key).query_async(&mut c).await.expect("llen");
        assert_eq!(llen, 3, "Skip must not append to the existing list");

        // Overwrite replaces (DEL first), it must not append either.
        let statuses = write_readable_chunk(&at, &entries, ConflictMode::Overwrite)
            .await
            .expect("overwrite");
        assert!(
            statuses.iter().all(|s| *s == ReadableWriteStatus::Written),
            "{statuses:?}"
        );
        let llen: i64 = cmd("LLEN").arg(&l_key).query_async(&mut c).await.expect("llen");
        assert_eq!(llen, 3, "Overwrite must replace, not append");

        // CSV round-trip onto a clean slice again.
        let mut del = cmd("DEL");
        for key in &keys {
            del.arg(key);
        }
        del.exec_async(&mut c).await.expect("del");
        assert_eq!(
            sniff_import_format(csv_doc.as_bytes()).expect("sniff"),
            ImportFormat::Csv
        );
        let entries = parse_readable_entries(&csv_doc, ImportFormat::Csv).expect("parse csv");
        let statuses = write_readable_chunk(&at, &entries, ConflictMode::Skip)
            .await
            .expect("write csv");
        assert!(
            statuses.iter().all(|s| *s == ReadableWriteStatus::Written),
            "{statuses:?}"
        );
        let l: Vec<String> = cmd("LRANGE")
            .arg(&l_key)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(l, ["a", "b", "c"]);
        let v1: String = cmd("HGET")
            .arg(&h_key)
            .arg("f1")
            .query_async(&mut c)
            .await
            .expect("hget");
        assert_eq!(v1, "v1");

        let mut del = cmd("DEL");
        for key in &keys {
            del.arg(key);
        }
        del.exec_async(&mut c).await.expect("del");
    });
}

#[test]
#[ignore]
fn standalone_feature_probe_matches_the_server() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let features = probe_server_features(&id, 0).await.expect("probe");
        assert!(features.probed);
        for c in [
            ServerCommand::Info,
            ServerCommand::Scan,
            ServerCommand::Dbsize,
            ServerCommand::ConfigGet,
            ServerCommand::SlowlogGet,
            ServerCommand::ClientList,
            ServerCommand::Dump,
            // Exists on a standalone server too — "cluster support disabled"
            // is a server type, not a limitation.
            ServerCommand::ClusterInfo,
        ] {
            assert_eq!(features.status(c), CommandStatus::Available, "{c:?}");
        }
        let has_functions = supports(&id, floors::FUNCTIONS).await;
        let expect_functions = if has_functions {
            CommandStatus::Available
        } else {
            CommandStatus::Missing
        };
        assert_eq!(features.status(ServerCommand::FunctionList), expect_functions);
        // Mutating commands are never executed: on 7+ ACL DRYRUN says
        // Available, on 6.x COMMAND INFO proves existence and the status
        // stays optimistic — either way they must not read as unusable.
        for c in [ServerCommand::Monitor, ServerCommand::Bgsave, ServerCommand::FlushDb] {
            assert!(features.is_usable(c), "{c:?} → {:?}", features.status(c));
            if has_functions {
                assert_eq!(features.status(c), CommandStatus::Available, "{c:?}");
            }
        }
        if let Ok(expected) = env::var("ZEDIS_IT_FLAVOR") {
            assert_eq!(
                features.flavor.label().to_ascii_lowercase(),
                expected.to_ascii_lowercase()
            );
        } else {
            assert!(matches!(features.flavor, ServerFlavor::Redis | ServerFlavor::Valkey));
        }
    });
}

/// The writes the Geo / HLL / Bitmap viewers gained. All core Redis, so
/// every lane runs them.
///
/// `GEOADD` is the point: a geo key is a sorted set whose score is a
/// geohash, so this is the only way to put a member in one — `ZADD` with a
/// hand-written score lands somewhere nobody meant. `BYBOX` is asserted
/// against `BYRADIUS` on the same data, since a box that behaved like a
/// circle would look right in every screenshot.
#[test]
#[ignore]
fn standalone_geo_hll_and_bitmap_writes() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        // What the viewers hold instead of a connection.
        let at = ServerDb::new(&id, 0);

        // Three points along a line of latitude, ~110 km apart in longitude.
        let geo = unique("geo-write");
        for (lon, member) in [(0.0_f64, "origin"), (1.0, "east"), (2.0, "far-east")] {
            let added = geo_add(&at, &geo, lon, 0.0, member).await.expect("geoadd");
            assert_eq!(added, 1, "{member} is new");
        }
        // Re-adding a member moves it rather than counting as new.
        assert_eq!(geo_add(&at, &geo, 0.0, 0.0, "origin").await.expect("re-add"), 0);

        let metres = geo_dist(&at, &geo, "origin", "east")
            .await
            .expect("geodist")
            .expect("both members exist");
        assert!(
            (100_000.0..130_000.0).contains(&metres),
            "a degree of longitude at the equator: {metres} m"
        );
        // A member that is not there is absent, not an error.
        assert_eq!(geo_dist(&at, &geo, "origin", "nowhere").await.expect("geodist"), None);

        // What the map reads. Under the cap the sample is the whole set, in
        // score order, each member with the position GEOPOS decodes.
        let sample = geo_sample(&at, &geo, 100).await.expect("geo sample");
        assert_eq!(sample.total, 3);
        let names: Vec<&str> = sample.members.iter().map(|m| m.member.as_str()).collect();
        assert_eq!(names, ["origin", "east", "far-east"]);
        let (east_lon, east_lat) = sample.members[1].position.expect("east has a position");
        assert!(
            (east_lon - 1.0).abs() < 1e-4 && east_lat.abs() < 1e-4,
            "east at ({east_lon}, {east_lat})"
        );
        // Over the cap: a sample of exactly the cap, and the total still the
        // whole key's.
        let capped = geo_sample(&at, &geo, 2).await.expect("capped sample");
        assert_eq!((capped.total, capped.members.len()), (3, 2));
        assert!(capped.members.iter().all(|m| m.position.is_some()));
        // A missing key is an empty map, not an error.
        let none = geo_sample(&at, &unique("geo-none"), 100).await.expect("missing key");
        assert_eq!((none.total, none.members.len()), (0, 0));

        // The same two searches the raw commands below make, through the
        // operation the map calls: nearest first.
        assert_eq!(
            geo_search(&at, &geo, 0.0, 0.0, GeoShape::Radius(150_000.0), 10)
                .await
                .expect("by radius"),
            ["origin", "east"]
        );
        let tile = GeoShape::Box {
            width_m: 500_000.0,
            height_m: 10_000.0,
        };
        assert_eq!(
            geo_search(&at, &geo, 0.0, 0.0, tile, 10).await.expect("by box"),
            ["origin", "east", "far-east"]
        );
        assert_eq!(
            geo_search(&at, &geo, 0.0, 0.0, tile, 1).await.expect("count").len(),
            1,
            "COUNT caps it"
        );

        // "Does this sorted set hold GEO data?" — yes for GEOADD members, no
        // for plain ZADD scores (GEOPOS decodes those to one far corner), no
        // for a key that is not there.
        assert!(zset_looks_geo(&at, &geo).await);
        let plain = unique("geo-plain-zset");
        let _: () = cmd("ZADD")
            .arg(&plain)
            .arg(0)
            .arg("a")
            .arg(0)
            .arg("b")
            .query_async(&mut c)
            .await
            .expect("zadd");
        assert!(
            !zset_looks_geo(&at, &plain).await,
            "a plain sorted set was taken for GEO"
        );
        assert!(!zset_looks_geo(&at, &unique("geo-none")).await);
        let _: () = cmd("DEL").arg(&plain).query_async(&mut c).await.expect("cleanup plain");

        // A 150 km radius reaches the first neighbour but not the second.
        let by_radius: Vec<String> = cmd("GEOSEARCH")
            .arg(&geo)
            .arg("FROMLONLAT")
            .arg(0.0)
            .arg(0.0)
            .arg("BYRADIUS")
            .arg(150)
            .arg("km")
            .arg("ASC")
            .query_async(&mut c)
            .await
            .expect("byradius");
        assert_eq!(by_radius, vec!["origin", "east"]);

        // A box 500 km wide but only 10 km tall covers all three, which a
        // circle of either extent would not — the two really are different.
        let by_box: Vec<String> = cmd("GEOSEARCH")
            .arg(&geo)
            .arg("FROMLONLAT")
            .arg(0.0)
            .arg(0.0)
            .arg("BYBOX")
            .arg(500)
            .arg(10)
            .arg("km")
            .arg("ASC")
            .query_async(&mut c)
            .await
            .expect("bybox");
        assert_eq!(by_box, vec!["origin", "east", "far-east"]);

        // PFMERGE keeps what the destination already had and adds the rest.
        let hll_a = unique("hll-a");
        let hll_b = unique("hll-b");
        // Through the operations the HLL viewer calls.
        let strs = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(
            pf_add(&at, &hll_a, &strs(&["x", "y"])).await.expect("pfadd a"),
            "new elements change the estimate"
        );
        assert!(
            !pf_add(&at, &hll_a, &strs(&["x"])).await.expect("pfadd again"),
            "a known element does not"
        );
        pf_add(&at, &hll_b, &strs(&["y", "z"])).await.expect("pfadd b");
        // No elements: not sent — a bare `PFADD key` would create the key.
        let absent = unique("hll-absent");
        assert!(!pf_add(&at, &absent, &[]).await.expect("empty pfadd"));
        let exists: bool = cmd("EXISTS").arg(&absent).query_async(&mut c).await.expect("exists");
        assert!(!exists, "an empty PFADD created a sketch");

        pf_merge(&at, &hll_a, std::slice::from_ref(&hll_b))
            .await
            .expect("pfmerge");
        let info = hll_info(&at, &hll_a).await.expect("hll info");
        assert_eq!(
            info.cardinality, 3,
            "x, y and z — the destination was folded in, not replaced"
        );
        assert_eq!(
            info.encoding,
            Some(HllEncoding::Sparse),
            "three elements are nowhere near dense"
        );
        assert!(info.size > 0);
        // Nothing to merge is a no-op, not an error.
        pf_merge(&at, &hll_a, &[]).await.expect("empty merge");

        // BITOP over two known byte patterns.
        let bits_a = unique("bits-a");
        let bits_b = unique("bits-b");
        let dest = unique("bits-dest");
        let _: () = cmd("SET")
            .arg(&bits_a)
            .arg("\x0f")
            .query_async(&mut c)
            .await
            .expect("set a");
        let _: () = cmd("SET")
            .arg(&bits_b)
            .arg("\x33")
            .query_async(&mut c)
            .await
            .expect("set b");
        let len = bit_op(&at, BitOpKind::And, &dest, &[bits_a.clone(), bits_b.clone()])
            .await
            .expect("bitop and");
        assert_eq!(len, 1, "one byte in, one byte out");
        let anded: Vec<u8> = cmd("GET").arg(&dest).query_async(&mut c).await.expect("get dest");
        assert_eq!(anded, vec![0x0f & 0x33]);

        // NOT takes exactly one source, and the wrapper refuses more before
        // the server has to.
        assert!(
            bit_op(&at, BitOpKind::Not, &dest, &[bits_a.clone(), bits_b.clone()])
                .await
                .is_err()
        );
        // What the bitmap viewer reads and writes. `bits_a` is one byte, 0x0f.
        let info = bitmap_info(&at, &bits_a, 512).await.expect("bitmap info");
        assert_eq!(
            (info.bytes.as_slice(), info.total_bits, info.set_bits),
            (&[0x0f_u8][..], 8, 4)
        );
        assert_eq!((info.first_set, info.first_clear), (4, 0), "0000 1111");
        assert!(!info.truncated);
        assert_eq!(info.rendered_bits(), 8);
        // A window smaller than the key: truncated, and the statistics still
        // describe the whole key.
        let wide = unique("bits-wide");
        let _: () = cmd("SET")
            .arg(&wide)
            .arg(vec![0xff_u8; 4])
            .query_async(&mut c)
            .await
            .expect("set wide");
        let windowed = bitmap_info(&at, &wide, 1).await.expect("windowed");
        assert_eq!(
            (windowed.bytes.len(), windowed.total_bits, windowed.set_bits),
            (1, 32, 32)
        );
        assert!(windowed.truncated);
        // A missing key is an empty bitmap, not an error.
        let missing = bitmap_info(&at, &unique("bits-none"), 512).await.expect("missing key");
        assert_eq!((missing.total_bits, missing.rendered_bits()), (0, 0));
        // SETBIT reports the bit it replaced.
        assert!(
            !set_bit(&at, &bits_a, 0, true).await.expect("setbit"),
            "bit 0 was clear"
        );
        assert!(
            set_bit(&at, &bits_a, 0, false).await.expect("setbit back"),
            "and then set"
        );
        // BITFIELD, as typed by the user.
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            bit_field(&at, &bits_a, &args("GET u8 0")).await.expect("bitfield get"),
            vec![0x0f]
        );
        assert_eq!(
            bit_field(&at, &bits_a, &args("SET u4 0 15 GET u8 0"))
                .await
                .expect("bitfield set"),
            vec![0, 0xff],
            "one integer per sub-command: the old u4, then the new u8"
        );
        let _: () = cmd("SET")
            .arg(&bits_a)
            .arg("\x0f")
            .query_async(&mut c)
            .await
            .expect("restore bits_a");
        let _: () = cmd("DEL").arg(&wide).query_async(&mut c).await.expect("cleanup wide");

        bit_op(&at, BitOpKind::Not, &dest, std::slice::from_ref(&bits_a))
            .await
            .expect("bitop not");
        let negated: Vec<u8> = cmd("GET").arg(&dest).query_async(&mut c).await.expect("get dest");
        assert_eq!(negated, vec![!0x0f_u8]);

        let _: () = cmd("DEL")
            .arg(&[&geo, &hll_a, &hll_b, &bits_a, &bits_b, &dest])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// The TimeSeries writes the chart panel gained. RedisTimeSeries only, so
/// this runs on the stack lane and skips loudly everywhere else.
/// `CONFIG` through the operations the config editor calls. It changes one
/// parameter nothing else depends on, and puts it back.
#[test]
#[ignore]
fn standalone_config_is_read_set_and_compared() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);

        let loaded = config_load(&at).await.expect("config load");
        assert!(
            loaded.params.len() > 50,
            "CONFIG GET * returned {} parameters",
            loaded.params.len()
        );
        assert!(loaded.params.windows(2).all(|w| w[0].0 <= w[1].0), "sorted by name");
        let map = config_get_all(&at).await.expect("config get all");
        assert_eq!(
            map.len(),
            loaded.params.len(),
            "the list and the map are the same reply"
        );

        // `slowlog-max-len` is a plain integer no other test reads.
        let name = "slowlog-max-len";
        let before = map.get(name).cloned().expect("slowlog-max-len exists");
        let changed = if before == "137" { "138" } else { "137" };
        config_set(&at, name, changed).await.expect("config set");
        assert_eq!(
            config_get_all(&at).await.expect("reread").get(name).map(String::as_str),
            Some(changed)
        );
        config_set(&at, name, &before).await.expect("restore");
        // A value the server refuses is an error, not a silent no-op.
        assert!(config_set(&at, name, "not-a-number").await.is_err());
        assert!(config_set(&at, "zedis-no-such-parameter", "1").await.is_err());

        // REWRITE needs a config file: with one it succeeds, without one the
        // server says so — and `config_file` is what told the editor which.
        let rewritten = config_rewrite(&at).await;
        assert_eq!(
            rewritten.is_ok(),
            !loaded.config_file.is_empty(),
            "config_file = {:?}, rewrite = {rewritten:?}",
            loaded.config_file
        );
    });
}

/// The small reads and resets behind the persistence, load, INFO and latency
/// panels — each was one hand-built command inside its view.
#[test]
#[ignore]
fn standalone_admin_panels_read_and_reset_through_their_operations() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);

        // CONFIG GET by name: a value for what exists, `None` for what does
        // not, in the order asked.
        let values = config_get_named(&at, &["appendonly", "zedis-no-such-parameter", "dbfilename"])
            .await
            .expect("config get named");
        assert_eq!(values.len(), 3);
        assert!(
            matches!(values[0].as_deref(), Some("yes" | "no")),
            "appendonly = {:?}",
            values[0]
        );
        assert_eq!(values[1], None);
        assert!(
            values[2].as_deref().is_some_and(|name| !name.is_empty()),
            "dbfilename = {:?}",
            values[2]
        );

        // The fullest INFO the server gives, one entry per master.
        let info = info_everything(&at).await.expect("info everything");
        assert_eq!(info.len(), 1);
        assert!(info[0].0.contains(':'), "labelled host:port, got {:?}", info[0].0);
        assert!(info[0].1.contains("redis_version:"));
        // `everything` / `all` carry what the plain INFO leaves out.
        assert!(info[0].1.contains("# Commandstats"), "the full listing was not reached");

        config_resetstat(&at).await.expect("config resetstat");

        // LATENCY: the listing parses whether or not monitoring is on, a
        // reset answers how many events it cleared, and the threshold is what
        // CONFIG says.
        let listing = latency_latest(&at).await.expect("latency latest");
        assert!(!listing.unsupported, "a real Redis has LATENCY");
        let threshold = latency_monitor_threshold(&at).await.expect("threshold");
        assert_eq!(
            config_get_named(&at, &["latency-monitor-threshold"])
                .await
                .expect("named")[0],
            Some(threshold.to_string())
        );
        latency_reset(&at, &[]).await.expect("latency reset");
        assert!(
            latency_latest(&at).await.expect("after reset").events.is_empty(),
            "reset left events behind"
        );
        assert!(latency_history(&at, "command").await.expect("history").is_empty());

        // The slow log through the panel's operation, and its reset.
        command_log_reset(&at, CommandLogKind::Slow)
            .await
            .expect("slowlog reset");
        let logs = command_logs(&at, CommandLogKind::Slow).await.expect("slow log");
        assert!(logs.len() <= 1, "just reset — at most the reset itself: {}", logs.len());
    });
}

/// Vector sets through the operations their viewer calls. Gated on the
/// command, not on a version: `VADD` exists wherever the module is loaded.
#[test]
#[ignore]
fn standalone_vector_set_describes_itself_and_finds_neighbours() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let key = unique("vset");
        let add = |element: &'static str, x: f64, y: f64| {
            let mut command = cmd("VADD");
            command.arg(&key).arg("VALUES").arg(2).arg(x).arg(y).arg(element);
            command
        };
        if let Err(e) = add("east", 1.0, 0.0).query_async::<i64>(&mut c).await {
            eprintln!("skipped: no vector sets on this server ({e})");
            return;
        }
        let _: i64 = add("north", 0.0, 1.0).query_async(&mut c).await.expect("vadd north");
        let _: i64 = add("north-east", 1.0, 1.0)
            .query_async(&mut c)
            .await
            .expect("vadd north-east");

        let at = ServerDb::new(&id, 0);
        let sim = VectorSimOptions {
            count: 10,
            ..Default::default()
        };
        let loaded = vset_info(&at, &key, 10, &sim).await.expect("vset info");
        assert_eq!((loaded.card, loaded.dim), (3, 2));
        assert!(!loaded.info.is_empty(), "VINFO rows");
        let mut sample = loaded.sample.clone();
        sample.sort();
        assert_eq!(sample, ["east", "north", "north-east"]);
        // The neighbour panel is seeded from the first sampled element, which
        // is its own nearest neighbour.
        let seeded = loaded.first.expect("a search around the first sample");
        assert_eq!(
            seeded.neighbours.first().map(|n| n.element.as_str()),
            loaded.sample.first().map(String::as_str)
        );

        // Around `east`: itself at 1.0, then the diagonal, then the orthogonal one.
        let found = vset_sim(&at, &key, "east", &sim).await.expect("vsim");
        let order: Vec<&str> = found.neighbours.iter().map(|n| n.element.as_str()).collect();
        assert_eq!(order, ["east", "north-east", "north"]);
        assert!(
            (found.neighbours[0].score - 1.0).abs() < 1e-6,
            "an element is its own best match"
        );
        assert_eq!(found.attrs, None, "no attributes yet");
        let vector = found.vector.expect("VEMB");
        assert_eq!(vector.len(), 2);

        // Attributes: set, read back with the search, cleared by an empty string.
        assert!(
            vset_set_attr(&at, &key, "east", r#"{"side":"right"}"#)
                .await
                .expect("vsetattr")
        );
        assert_eq!(
            vset_sim(&at, &key, "east", &sim).await.expect("vsim").attrs.as_deref(),
            Some(r#"{"side":"right"}"#)
        );
        assert!(
            !vset_set_attr(&at, &key, "no-such-element", "{}")
                .await
                .expect("vsetattr on a missing element")
        );
        assert!(vset_set_attr(&at, &key, "east", "").await.expect("clear attrs"));
        assert_eq!(vset_sim(&at, &key, "east", &sim).await.expect("vsim").attrs, None);

        // Remove: says whether there was anything to remove.
        assert!(vset_remove(&at, &key, "north").await.expect("vrem"));
        assert!(!vset_remove(&at, &key, "north").await.expect("vrem again"));
        assert_eq!(vset_info(&at, &key, 10, &sim).await.expect("vset info").card, 2);
        // A key that is not there: `VINFO` answers nil rather than failing,
        // and the rest is empty.
        let missing = vset_info(&at, &unique("vset-none"), 10, &sim)
            .await
            .expect("missing key");
        assert_eq!((missing.card, missing.sample.len(), missing.first), (0, 0, None));

        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// RedisBloom through the operations the probabilistic viewer calls: every
/// structure's add and query, the `*.INFO` rows, the Top-K list and the
/// t-digest quantiles. None of these commands had a test while the view built
/// them itself.
#[test]
#[ignore]
fn stack_probabilistic_structures_probe_and_describe_themselves() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack", standalone())).await;
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let probe = |key: &str, kind: ProbKind, item: &str, add: bool| {
            let (at, key, item) = (at.clone(), key.to_string(), item.to_string());
            async move { prob_probe(&at, &key, kind, &item, add).await.expect("probe") }
        };

        // Bloom: a negative is definitive, a positive is a maybe, and a second
        // add of the same item says "already".
        let bf = unique("prob-bf");
        assert_eq!(probe(&bf, ProbKind::Bloom, "a", true).await, ProbeOutcome::Added);
        assert_eq!(probe(&bf, ProbKind::Bloom, "a", true).await, ProbeOutcome::AlreadyMaybe);
        assert_eq!(probe(&bf, ProbKind::Bloom, "a", false).await, ProbeOutcome::MaybeExists);
        assert_eq!(
            probe(&bf, ProbKind::Bloom, "zz", false).await,
            ProbeOutcome::DefinitelyNot
        );
        let info = prob_info(&at, &bf, ProbKind::Bloom).await.expect("bf info");
        assert!(
            info.info.iter().any(|(k, _)| k == "Capacity"),
            "BF.INFO rows: {:?}",
            info.info
        );
        assert!(
            info.top_items.is_empty() && info.quantiles.is_empty(),
            "extras are per kind"
        );

        // Cuckoo shares the EXISTS path by prefix.
        let cf = unique("prob-cf");
        assert_eq!(probe(&cf, ProbKind::Cuckoo, "a", true).await, ProbeOutcome::Added);
        assert_eq!(
            probe(&cf, ProbKind::Cuckoo, "a", false).await,
            ProbeOutcome::MaybeExists
        );

        // Count-Min Sketch: "add" is INCRBY 1, and both directions report the
        // estimate.
        let cms = unique("prob-cms");
        let _: () = cmd("CMS.INITBYDIM")
            .arg(&cms)
            .arg(2000)
            .arg(5)
            .query_async(&mut c)
            .await
            .expect("cms init");
        assert_eq!(
            probe(&cms, ProbKind::CountMinSketch, "a", true).await,
            ProbeOutcome::Count(1)
        );
        assert_eq!(
            probe(&cms, ProbKind::CountMinSketch, "a", true).await,
            ProbeOutcome::Count(2)
        );
        assert_eq!(
            probe(&cms, ProbKind::CountMinSketch, "a", false).await,
            ProbeOutcome::Count(2)
        );

        // Top-K of one: the list carries counts.
        let topk = unique("prob-topk");
        let _: () = cmd("TOPK.RESERVE")
            .arg(&topk)
            .arg(1)
            .query_async(&mut c)
            .await
            .expect("topk reserve");
        assert_eq!(probe(&topk, ProbKind::TopK, "a", true).await, ProbeOutcome::Added);
        assert_eq!(probe(&topk, ProbKind::TopK, "a", false).await, ProbeOutcome::InTopK);
        assert_eq!(probe(&topk, ProbKind::TopK, "b", false).await, ProbeOutcome::NotInTopK);
        let listed = prob_info(&at, &topk, ProbKind::TopK).await.expect("topk info");
        assert_eq!(listed.top_items, vec![("a".to_string(), 1)]);

        // t-digest: an empty one answers nan, which is left out rather than drawn.
        let td = unique("prob-td");
        let _: () = cmd("TDIGEST.CREATE")
            .arg(&td)
            .query_async(&mut c)
            .await
            .expect("tdigest create");
        let empty = prob_info(&at, &td, ProbKind::TDigest).await.expect("tdigest info");
        assert!(
            empty.quantiles.is_empty(),
            "nan quantiles were kept: {:?}",
            empty.quantiles
        );
        for value in ["1", "2", "3", "4"] {
            assert_eq!(probe(&td, ProbKind::TDigest, value, true).await, ProbeOutcome::Added);
        }
        let filled = prob_info(&at, &td, ProbKind::TDigest).await.expect("tdigest info");
        let labels: Vec<&str> = filled.quantiles.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["min", "max", "p50", "p90", "p99"]);
        assert_eq!((filled.quantiles[0].1, filled.quantiles[1].1), (1.0, 4.0));
        match probe(&td, ProbKind::TDigest, "4", false).await {
            ProbeOutcome::Cdf(fraction) => assert!(fraction > 0.5 && fraction <= 1.0, "cdf(4) = {fraction}"),
            other => panic!("expected a CDF, got {other:?}"),
        }

        let _: () = cmd("DEL")
            .arg(&[&bf, &cf, &cms, &topk, &td])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

#[test]
#[ignore]
fn stack_timeseries_write_operations() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack", standalone())).await;
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let key = unique("ts-write");
        let compacted = unique("ts-write-1m");

        let _: () = cmd("TS.CREATE")
            .arg(&key)
            .arg("RETENTION")
            .arg(0)
            .arg("LABELS")
            .arg("env")
            .arg("test")
            .query_async(&mut c)
            .await
            .expect("ts.create");

        // A sample at an explicit timestamp, and one at "now".
        assert_eq!(ts_add(&at, &key, Some(1000), 1.5).await.expect("ts.add"), 1000);
        let now_ts = ts_add(&at, &key, None, 2.5).await.expect("ts.add now");
        // What the series viewer reads: the metadata and the samples of the
        // window — all of it here, and too short a span to be bucketed.
        let window = ts_window(&at, &key, None, 240).await.expect("ts window");
        assert_eq!(window.info.total_samples, 2);
        assert_eq!((window.info.first_ts, window.info.last_ts), (1000, now_ts));
        assert_eq!(window.samples.first(), Some(&(1000, 1.5)));
        assert_eq!(window.samples.len(), 2);
        // A window that ends at the last sample and is one millisecond long
        // holds that sample only.
        let last_only = ts_window(&at, &key, Some(1), 240).await.expect("narrow window");
        assert_eq!(last_only.samples, vec![(now_ts, 2.5)]);
        assert!(now_ts > 1000, "wall clock follows the backfilled sample: {now_ts}");

        // TS.ALTER touches only what it is given: retention alone leaves the
        // labels, which is the distinction `Option` carries in `TsAlter`.
        ts_alter(
            &at,
            &key,
            &TsAlter {
                retention_ms: Some(86_400_000),
                labels: None,
            },
        )
        .await
        .expect("ts.alter retention");
        let info = ts_info_map(&mut c, &key).await;
        let retention = info
            .get("retentionTime")
            .cloned()
            .and_then(|v| i64::from_redis_value(v).ok());
        assert_eq!(retention, Some(86_400_000));
        let labels = info.get("labels").cloned().expect("TS.INFO reports labels");
        assert!(
            matches!(&labels, redis::Value::Array(items) if !items.is_empty()),
            "labels survived a retention-only alter: {labels:?}"
        );

        // An empty label list clears them — the reason "leave alone" and
        // "clear" are different states rather than an empty vector.
        ts_alter(
            &at,
            &key,
            &TsAlter {
                retention_ms: None,
                labels: Some(Vec::new()),
            },
        )
        .await
        .expect("ts.alter clear labels");

        // A compaction rule, and the destination filling from it.
        let _: () = cmd("TS.CREATE")
            .arg(&compacted)
            .query_async(&mut c)
            .await
            .expect("ts.create dst");
        ts_create_rule(&at, &key, &compacted, "avg", 60_000)
            .await
            .expect("ts.createrule");
        let rules = ts_window(&at, &key, None, 240).await.expect("ts window").info.rules;
        assert_eq!(rules.len(), 1, "the rule as the viewer lists it: {rules:?}");
        assert_eq!(
            (rules[0].destination.as_str(), rules[0].bucket_ms),
            (compacted.as_str(), 60_000)
        );
        assert_eq!(
            ts_window(&at, &compacted, None, 240)
                .await
                .expect("compacted")
                .info
                .source_key
                .as_deref(),
            Some(key.as_str()),
            "a destination series names the series it is compacted from"
        );
        let info = ts_info_map(&mut c, &key).await;
        let rules = info.get("rules").cloned().expect("TS.INFO reports rules");
        assert!(
            matches!(&rules, redis::Value::Array(items) if items.len() == 1),
            "one rule, the one the panel lists: {rules:?}"
        );
        ts_delete_rule(&at, &key, &compacted).await.expect("ts.deleterule");

        let _: () = cmd("DEL")
            .arg(&[&key, &compacted])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// `TS.MRANGE` behind the multi-series explorer.
///
/// Two things are asserted that a single-series read cannot show: label
/// matchers select across keys, and `AGGREGATION` puts every matched series
/// on the *same* bucket boundaries — which is the only reason overlaying
/// their lines on one axis is honest.
#[test]
#[ignore]
fn stack_timeseries_mrange_selects_by_label_and_aligns_buckets() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack", standalone())).await;
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let tag = unique("mrange");
        let a = format!("{tag}:a");
        let b = format!("{tag}:b");
        let other = format!("{tag}:other");

        for (key, host) in [(&a, "a"), (&b, "b")] {
            let _: () = cmd("TS.CREATE")
                .arg(key)
                .arg("LABELS")
                .arg("suite")
                .arg(&tag)
                .arg("host")
                .arg(host)
                .query_async(&mut c)
                .await
                .expect("ts.create");
        }
        // A third series that the filter must *not* pick up.
        let _: () = cmd("TS.CREATE")
            .arg(&other)
            .arg("LABELS")
            .arg("suite")
            .arg("someone-else")
            .query_async(&mut c)
            .await
            .expect("ts.create other");

        // Samples at different offsets inside the same 60s bucket, so
        // aggregation is what makes the two series line up.
        for (key, base) in [(&a, 1_000_000_000_000_i64), (&b, 1_000_000_000_000)] {
            for (offset, value) in [(0_i64, 1.0_f64), (10_000, 2.0), (61_000, 3.0)] {
                let _: () = cmd("TS.ADD")
                    .arg(key)
                    .arg(base + offset + if key == &b { 5_000 } else { 0 })
                    .arg(value)
                    .query_async(&mut c)
                    .await
                    .expect("ts.add");
            }
        }

        // A filter that only excludes is refused before it is sent.
        let refused = ts_mrange(
            &at,
            &TsMRange {
                filters: vec!["host!=a".to_string()],
                ..Default::default()
            },
        )
        .await;
        assert!(refused.is_err(), "a query with no positive matcher must not be sent");

        let query = TsMRange {
            from_ms: Some(0),
            to_ms: Some(2_000_000_000_000),
            filters: vec![format!("suite={tag}")],
            aggregation: Some(("avg".to_string(), 60_000)),
            count: Some(100),
        };
        let mut series = ts_mrange(&at, &query).await.expect("ts.mrange");
        series.sort_by(|x, y| x.key.cmp(&y.key));
        assert_eq!(
            series.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            vec![a.as_str(), b.as_str()],
            "the label picked exactly the two, not the third"
        );
        assert!(
            series.iter().all(|s| s.labels.iter().any(|(k, _)| k == "host")),
            "WITHLABELS carried the labels the table shows"
        );

        // Aggregated buckets are identical across series — the property the
        // shared x-axis depends on.
        let a_stamps: Vec<i64> = series[0].samples.iter().map(|(ts, _)| *ts).collect();
        let b_stamps: Vec<i64> = series[1].samples.iter().map(|(ts, _)| *ts).collect();
        assert_eq!(a_stamps, b_stamps, "aggregation aligned the buckets");
        assert!(
            a_stamps.iter().all(|ts| ts % 60_000 == 0),
            "buckets snap to the duration"
        );
        assert_eq!(a_stamps.len(), 2, "two 60s buckets over the samples written");

        let _: () = cmd("DEL")
            .arg(&[&a, &b, &other])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// `TS.INFO` flattened to name → value, for the assertions above.
///
/// Values stay `redis::Value` so each assertion converts the way it means
/// to — a Debug rendering would make the test depend on how redis-rs
/// happens to print an integer.
async fn ts_info_map(c: &mut RedisAsyncConn, key: &str) -> HashMap<String, redis::Value> {
    let raw: Vec<redis::Value> = cmd("TS.INFO").arg(key).query_async(c).await.expect("ts.info");
    raw.chunks(2)
        .filter_map(|pair| {
            let name = String::from_redis_value(pair.first()?.clone()).ok()?;
            Some((name, pair.get(1)?.clone()))
        })
        .collect()
}

/// The two stream operations the editor gained: removing a consumer, and
/// moving the stream's own last-generated id.
///
/// `XGROUP DELCONSUMER` answers with the pending entries that went with the
/// consumer — the number the dialog shows before the click. `XSETID` is the
/// stream's id, not a group's: `last-generated-id` stays put when the newest
/// entry is deleted, which is exactly why the two are separate fields and
/// why lowering it lets `XADD` mint an id that already existed.
#[test]
#[ignore]
fn standalone_stream_consumer_and_id_administration() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let key = unique("stream-admin");
        let group = "g1";

        let mut ids = Vec::new();
        for n in 0..3 {
            let entry: String = cmd("XADD")
                .arg(&key)
                .arg("*")
                .arg("n")
                .arg(n)
                .query_async(&mut c)
                .await
                .expect("xadd");
            ids.push(entry);
        }
        let _: () = cmd("XGROUP")
            .arg("CREATE")
            .arg(&key)
            .arg(group)
            .arg("0")
            .query_async(&mut c)
            .await
            .expect("xgroup create");
        // Read without acknowledging, so the consumer owns pending entries.
        let _: redis::Value = cmd("XREADGROUP")
            .arg("GROUP")
            .arg(group)
            .arg("worker")
            .arg("COUNT")
            .arg(2)
            .arg("STREAMS")
            .arg(&key)
            .arg(">")
            .query_async(&mut c)
            .await
            .expect("xreadgroup");

        let pending: u64 = cmd("XGROUP")
            .arg("DELCONSUMER")
            .arg(&key)
            .arg(group)
            .arg("worker")
            .query_async(&mut c)
            .await
            .expect("delconsumer");
        assert_eq!(pending, 2, "the pending entries that went with the consumer");
        // Deleting a consumer that is not there is not an error, just zero.
        let pending: u64 = cmd("XGROUP")
            .arg("DELCONSUMER")
            .arg(&key)
            .arg(group)
            .arg("nobody")
            .query_async(&mut c)
            .await
            .expect("delconsumer on an unknown name");
        assert_eq!(pending, 0);

        // `last-generated-id` outlives the entry that produced it.
        let last = ids.last().expect("three entries").clone();
        let _: u64 = cmd("XDEL")
            .arg(&key)
            .arg(&last)
            .query_async(&mut c)
            .await
            .expect("xdel");
        let generated = stream_info_field(&mut c, &key, "last-generated-id").await;
        assert_eq!(generated, last, "deleting the newest entry does not rewind the id");

        // The floor nobody expects: XSETID refuses anything below
        // `max-deleted-entry-id`, which the XDEL above just set. Without
        // this assertion the dialog would promise a rewind it cannot always
        // deliver — the hint text says so because of this.
        let refused: redis::RedisResult<()> = cmd("XSETID").arg(&key).arg("5-5").query_async(&mut c).await;
        let error = refused.expect_err("below max-deleted-entry-id");
        assert!(
            error.to_string().contains("smaller than"),
            "unexpected refusal: {error}"
        );

        // Above that floor it moves, and the next XADD carries on from there.
        let raised = format!("{}-9", generated.split('-').next().unwrap_or("1"));
        let _: () = cmd("XSETID")
            .arg(&key)
            .arg(&raised)
            .query_async(&mut c)
            .await
            .expect("xsetid above the floor");
        assert_eq!(stream_info_field(&mut c, &key, "last-generated-id").await, raised);
        let next: String = cmd("XADD")
            .arg(&key)
            .arg("*")
            .arg("n")
            .arg(9)
            .query_async(&mut c)
            .await
            .expect("xadd after xsetid");
        assert!(next > raised, "the next id follows the one just set: {next}");

        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// One `XINFO STREAM` field, by name.
async fn stream_info_field(c: &mut RedisAsyncConn, key: &str, field: &str) -> String {
    let raw: Vec<redis::Value> = cmd("XINFO")
        .arg("STREAM")
        .arg(key)
        .query_async(c)
        .await
        .expect("xinfo stream");
    let mut pairs = raw.chunks(2);
    pairs
        .find(|pair| {
            pair.first()
                .map(|name| String::from_redis_value(name.clone()).unwrap_or_default() == field)
                .unwrap_or(false)
        })
        .and_then(|pair| pair.get(1))
        .map(|value| String::from_redis_value(value.clone()).unwrap_or_default())
        .unwrap_or_default()
}

/// The sorted set's score window, and the direction trap in it.
///
/// `ZREVRANGEBYSCORE` takes **max first**; passing the bounds in the
/// ascending order silently returns nothing rather than erroring, so the
/// descending half is what this test is really for. `ZCOUNT` supplies the
/// total the footer pages against, and the exclusive `(` form is the other
/// half of the syntax the filter accepts.
#[test]
#[ignore]
fn standalone_score_window_reads_both_directions() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let key = unique("score-window");
        for (member, score) in [("a", 1), ("b", 5), ("c", 10), ("d", 15), ("e", 20)] {
            let _: () = cmd("ZADD")
                .arg(&key)
                .arg(score)
                .arg(member)
                .query_async(&mut c)
                .await
                .expect("zadd");
        }

        let count: u64 = cmd("ZCOUNT")
            .arg(&key)
            .arg(5)
            .arg(15)
            .query_async(&mut c)
            .await
            .expect("zcount");
        assert_eq!(count, 3, "the window's total, which the footer counts against");

        let ascending: Vec<String> = cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg(5)
            .arg(15)
            .query_async(&mut c)
            .await
            .expect("zrangebyscore");
        assert_eq!(ascending, vec!["b", "c", "d"]);

        // Max first. The same call with the arguments the ascending form
        // takes would answer with an empty list, not an error.
        let descending: Vec<String> = cmd("ZREVRANGEBYSCORE")
            .arg(&key)
            .arg(15)
            .arg(5)
            .query_async(&mut c)
            .await
            .expect("zrevrangebyscore");
        assert_eq!(descending, vec!["d", "c", "b"]);
        let reversed_bounds: Vec<String> = cmd("ZREVRANGEBYSCORE")
            .arg(&key)
            .arg(5)
            .arg(15)
            .query_async(&mut c)
            .await
            .expect("zrevrangebyscore with the bounds swapped");
        assert!(
            reversed_bounds.is_empty(),
            "swapping the bounds fails silently — hence the assertion above"
        );

        // `(` excludes the endpoint, and LIMIT is how the window pages.
        let exclusive: Vec<String> = cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg("(5")
            .arg("+inf")
            .query_async(&mut c)
            .await
            .expect("exclusive lower bound");
        assert_eq!(exclusive, vec!["c", "d", "e"]);
        let page: Vec<String> = cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg("-inf")
            .arg("+inf")
            .arg("LIMIT")
            .arg(2)
            .arg(2)
            .query_async(&mut c)
            .await
            .expect("limit page");
        assert_eq!(page, vec!["c", "d"]);

        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// The prefix compare behind the compare window, across two dbs of one
/// server: each side's missing keys, a value difference, a type
/// difference, and a set that only differs in member order counting as
/// the same. Then the copy dry run over the same keys.
#[test]
#[ignore]
fn standalone_compare_prefix_reports_each_side() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut a = conn(&id, 0).await;
        let mut b = conn(&id, 1).await;
        let prefix = unique("cmp");
        let key = |name: &str| format!("{prefix}:{name}");

        for c in [&mut a, &mut b] {
            cmd("SET").arg(key("same")).arg("v").exec_async(c).await.expect("set");
        }
        cmd("SADD")
            .arg(key("set"))
            .arg("a")
            .arg("b")
            .exec_async(&mut a)
            .await
            .expect("sadd");
        cmd("SADD")
            .arg(key("set"))
            .arg("b")
            .arg("a")
            .exec_async(&mut b)
            .await
            .expect("sadd");
        cmd("SET")
            .arg(key("diff"))
            .arg("1")
            .exec_async(&mut a)
            .await
            .expect("set");
        cmd("SET")
            .arg(key("diff"))
            .arg("2")
            .exec_async(&mut b)
            .await
            .expect("set");
        cmd("SET")
            .arg(key("type"))
            .arg("s")
            .exec_async(&mut a)
            .await
            .expect("set");
        cmd("HSET")
            .arg(key("type"))
            .arg("f")
            .arg("v")
            .exec_async(&mut b)
            .await
            .expect("hset");
        cmd("SET")
            .arg(key("only0"))
            .arg("x")
            .exec_async(&mut a)
            .await
            .expect("set");
        cmd("SET")
            .arg(key("only1"))
            .arg("y")
            .exec_async(&mut b)
            .await
            .expect("set");

        let source = CompareSide {
            server_id: id.clone(),
            db: 0,
        };
        let target = CompareSide {
            server_id: id.clone(),
            db: 1,
        };
        let options = CompareOptions {
            prefix: format!("{prefix}:"),
            limit: 100,
        };
        let cancel = AtomicBool::new(false);
        let report = compare_prefix(&source, &target, &options, &cancel, |_| {})
            .await
            .expect("compare");
        assert_eq!(report.same, 2, "the string and the reordered set: {report:?}");
        assert_eq!(report.only_source, vec![(key("only0"), "string".to_string())]);
        assert_eq!(report.only_target, vec![(key("only1"), "string".to_string())]);
        assert_eq!(report.differing.len(), 2, "{report:?}");
        let diff = report
            .differing
            .iter()
            .find(|entry| entry.key == key("diff"))
            .expect("the value difference");
        assert_eq!(diff.difference, KeyDifference::Value);
        let typed = report
            .differing
            .iter()
            .find(|entry| entry.key == key("type"))
            .expect("the type difference");
        assert_eq!(
            typed.difference,
            KeyDifference::Type {
                source: "string".to_string(),
                target: "hash".to_string()
            }
        );
        assert!(!report.source_capped && !report.target_capped && !report.cancelled);

        // A limit below the key count marks the side partial.
        let capped = compare_prefix(
            &source,
            &target,
            &CompareOptions {
                prefix: format!("{prefix}:"),
                limit: 2,
            },
            &cancel,
            |_| {},
        )
        .await
        .expect("compare");
        assert!(capped.source_capped, "{capped:?}");

        // The copy dry run: what the target already has of the source's keys.
        let preview = preview_key_conflicts(&id, 1, &[key("same"), key("only0")], 10, &cancel)
            .await
            .expect("preview");
        assert_eq!((preview.total, preview.conflicting, preview.free), (2, 1, 1));
        assert_eq!(preview.sample_keys, vec![key("same")]);

        for name in ["same", "set", "diff", "type", "only0", "only1"] {
            for c in [&mut a, &mut b] {
                cmd("DEL").arg(key(name)).exec_async(c).await.expect("del");
            }
        }
    });
}

/// The type-native operations the editors offer beyond add / edit / delete.
///
/// Each one is a command the terminal could run; what is asserted here is
/// that `run_key_op` builds the right one and reports back what the panel
/// shows — including the two shapes that are easy to get subtly wrong: an
/// integer counter that must stay an integer, and a `ZPOPMIN` reply that
/// interleaves members with scores.
#[test]
#[ignore]
fn standalone_key_ops_run_and_report_their_result() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;

        // LTRIM answers with the length that is left.
        let list = unique("op-list");
        let _: () = cmd("RPUSH")
            .arg(&list)
            .arg(&["a", "b", "c", "d", "e"])
            .query_async(&mut c)
            .await
            .expect("rpush");
        let outcome = run_key_op(&at, &list, KeyOp::ListTrim { start: 1, stop: 3 })
            .await
            .expect("ltrim");
        assert_eq!(outcome, KeyOpOutcome::Count(3));
        let rest: Vec<String> = cmd("LRANGE")
            .arg(&list)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(rest, vec!["b", "c", "d"]);

        // One pop uses the countless form every supported server has.
        let outcome = run_key_op(
            &at,
            &list,
            KeyOp::ListPop {
                end: FromEnd::Head,
                count: 1,
            },
        )
        .await
        .expect("lpop");
        assert_eq!(outcome, KeyOpOutcome::Removed(vec!["b".to_string()]));
        // Several needs the 6.2 count argument.
        if supports(&id, floors::POP_COUNT).await {
            let outcome = run_key_op(
                &at,
                &list,
                KeyOp::ListPop {
                    end: FromEnd::Tail,
                    count: 2,
                },
            )
            .await
            .expect("rpop count");
            assert_eq!(outcome, KeyOpOutcome::Removed(vec!["d".to_string(), "c".to_string()]));
        } else {
            eprintln!("skipped the multi-pop half: the server predates 6.2");
        }
        // Popping an empty list is not an error, just nothing.
        let outcome = run_key_op(
            &at,
            &unique("op-list-absent"),
            KeyOp::ListPop {
                end: FromEnd::Head,
                count: 1,
            },
        )
        .await
        .expect("lpop on a missing key");
        assert_eq!(outcome, KeyOpOutcome::Removed(vec![]));

        // ZINCRBY reports the new score, without a trailing `.0`.
        let zset = unique("op-zset");
        let _: () = cmd("ZADD")
            .arg(&zset)
            .arg(1)
            .arg("m1")
            .arg(2)
            .arg("m2")
            .query_async(&mut c)
            .await
            .expect("zadd");
        let outcome = run_key_op(
            &at,
            &zset,
            KeyOp::ZsetIncrBy {
                member: "m1".to_string(),
                delta: 4.0,
            },
        )
        .await
        .expect("zincrby");
        assert_eq!(outcome, KeyOpOutcome::Number("5".to_string()));

        // ZPOPMIN answers member, score, member, score — only the members
        // are named back.
        let outcome = run_key_op(
            &at,
            &zset,
            KeyOp::ZsetPop {
                end: FromEnd::Head,
                count: 2,
            },
        )
        .await
        .expect("zpopmin");
        assert_eq!(
            outcome,
            KeyOpOutcome::Removed(vec!["m2".to_string(), "m1".to_string()]),
            "lowest score first"
        );

        // HINCRBY on a field that does not exist yet starts from zero.
        let hash = unique("op-hash");
        let outcome = run_key_op(
            &at,
            &hash,
            KeyOp::HashIncrBy {
                field: "hits".to_string(),
                delta: 3,
            },
        )
        .await
        .expect("hincrby");
        assert_eq!(outcome, KeyOpOutcome::Number("3".to_string()));

        // A whole delta must keep the value an integer, so the *next*
        // integer increment still works — INCRBYFLOAT would not.
        let counter = unique("op-counter");
        let _: () = cmd("SET").arg(&counter).arg(5).query_async(&mut c).await.expect("set");
        let outcome = run_key_op(&at, &counter, KeyOp::StringIncrBy { delta: 2.0 })
            .await
            .expect("incrby");
        assert_eq!(outcome, KeyOpOutcome::Number("7".to_string()));
        let outcome = run_key_op(&at, &counter, KeyOp::StringIncrBy { delta: 1.0 })
            .await
            .expect("a second integer increment");
        assert_eq!(outcome, KeyOpOutcome::Number("8".to_string()));
        // A fractional delta switches command and the value stops being an int.
        let outcome = run_key_op(&at, &counter, KeyOp::StringIncrBy { delta: 0.5 })
            .await
            .expect("incrbyfloat");
        assert_eq!(outcome, KeyOpOutcome::Number("8.5".to_string()));

        // APPEND answers with the new length.
        let text = unique("op-text");
        let _: () = cmd("SET").arg(&text).arg("ab").query_async(&mut c).await.expect("set");
        let outcome = run_key_op(
            &at,
            &text,
            KeyOp::StringAppend {
                text: "cde".to_string(),
            },
        )
        .await
        .expect("append");
        assert_eq!(outcome, KeyOpOutcome::Count(5));

        // GETEX sets and clears the expiry without touching the value.
        if supports(&id, floors::GETEX).await {
            run_key_op(&at, &text, KeyOp::StringGetEx { ttl: Some(120) })
                .await
                .expect("getex ex");
            let ttl: i64 = cmd("TTL").arg(&text).query_async(&mut c).await.expect("ttl");
            assert!((100..=120).contains(&ttl), "ttl after GETEX EX: {ttl}");
            run_key_op(&at, &text, KeyOp::StringGetEx { ttl: None })
                .await
                .expect("getex persist");
            let ttl: i64 = cmd("TTL").arg(&text).query_async(&mut c).await.expect("ttl");
            assert_eq!(ttl, -1, "PERSIST clears the expiry");
            let value: String = cmd("GET").arg(&text).query_async(&mut c).await.expect("get");
            assert_eq!(value, "abcde", "GETEX leaves the value alone");
        } else {
            eprintln!("skipped the GETEX half: the server predates 6.2");
        }

        let _: () = cmd("DEL")
            .arg(&[&list, &zset, &hash, &counter, &text])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// Multi-select delete, per collection type.
///
/// Four of the five are a plain variadic command; the List is not, because
/// Redis cannot delete by position at all. `remove_list_indexes` stamps every
/// selected position with one marker inside a MULTI and removes the marker
/// once — the assertion that matters is that the *survivors* are the ones the
/// caller did not pick, which a naive "delete index 1, then index 3" would
/// get wrong the moment the first removal renumbers the rest.
#[test]
#[ignore]
fn standalone_batch_delete_removes_exactly_the_selected_entries() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;

        // List: delete positions 1, 3 and 4 of six. Every index is taken from
        // the same snapshot, which is what the marker makes safe.
        let list = unique("batch-list");
        let _: () = cmd("RPUSH")
            .arg(&list)
            .arg(&["a", "b", "c", "d", "e", "f"])
            .query_async(&mut c)
            .await
            .expect("rpush");
        let removed = remove_list_indexes(&at, &list, &[1, 3, 4]).await.expect("batch list");
        assert_eq!(removed, 3);
        let rest: Vec<String> = cmd("LRANGE")
            .arg(&list)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(rest, vec!["a", "c", "f"], "only the unselected positions survive");

        // A repeated index must not remove a second element, and an empty
        // selection must not touch the list.
        let removed = remove_list_indexes(&at, &list, &[0, 0]).await.expect("duplicate index");
        assert_eq!(removed, 1);
        assert_eq!(remove_list_indexes(&at, &list, &[]).await.expect("empty"), 0);
        let rest: Vec<String> = cmd("LRANGE")
            .arg(&list)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(rest, vec!["c", "f"]);

        // Hash / Set / ZSet / Stream: the variadic forms the batch delete
        // sends, asserted together so a server that lost one is caught here.
        let hash = unique("batch-hash");
        let _: () = cmd("HSET")
            .arg(&hash)
            .arg(&["f1", "v1", "f2", "v2", "f3", "v3"])
            .query_async(&mut c)
            .await
            .expect("hset");
        let gone: u64 = cmd("HDEL")
            .arg(&hash)
            .arg(&["f1", "f3"])
            .query_async(&mut c)
            .await
            .expect("hdel");
        assert_eq!(gone, 2);
        let fields: Vec<String> = cmd("HKEYS").arg(&hash).query_async(&mut c).await.expect("hkeys");
        assert_eq!(fields, vec!["f2"]);

        let set = unique("batch-set");
        let _: () = cmd("SADD")
            .arg(&set)
            .arg(&["m1", "m2", "m3"])
            .query_async(&mut c)
            .await
            .expect("sadd");
        let gone: u64 = cmd("SREM")
            .arg(&set)
            .arg(&["m1", "m2"])
            .query_async(&mut c)
            .await
            .expect("srem");
        assert_eq!(gone, 2);
        let card: u64 = cmd("SCARD").arg(&set).query_async(&mut c).await.expect("scard");
        assert_eq!(card, 1);

        let zset = unique("batch-zset");
        let _: () = cmd("ZADD")
            .arg(&zset)
            .arg(1)
            .arg("z1")
            .arg(2)
            .arg("z2")
            .arg(3)
            .arg("z3")
            .query_async(&mut c)
            .await
            .expect("zadd");
        let gone: u64 = cmd("ZREM")
            .arg(&zset)
            .arg(&["z1", "z3"])
            .query_async(&mut c)
            .await
            .expect("zrem");
        assert_eq!(gone, 2);
        let members: Vec<String> = cmd("ZRANGE")
            .arg(&zset)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("zrange");
        assert_eq!(members, vec!["z2"]);

        let stream = unique("batch-stream");
        let mut ids = Vec::new();
        for n in 0..3 {
            let entry: String = cmd("XADD")
                .arg(&stream)
                .arg("*")
                .arg("n")
                .arg(n)
                .query_async(&mut c)
                .await
                .expect("xadd");
            ids.push(entry);
        }
        let gone: u64 = cmd("XDEL")
            .arg(&stream)
            .arg(&[ids[0].as_str(), ids[2].as_str()])
            .query_async(&mut c)
            .await
            .expect("xdel");
        assert_eq!(gone, 2);
        let len: u64 = cmd("XLEN").arg(&stream).query_async(&mut c).await.expect("xlen");
        assert_eq!(len, 1);

        let _: () = cmd("DEL")
            .arg(&[&list, &hash, &set, &zset, &stream])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// The key editor's header chip: `OBJECT ENCODING` plus whichever of FREQ /
/// IDLETIME the server's `maxmemory-policy` makes answerable. The two heat
/// subcommands are mutually exclusive — asking for the wrong one is a
/// guaranteed error — so the probe has to pick, and this test pins both the
/// picking and the tolerance the chip depends on.
#[test]
#[ignore]
fn standalone_object_meta_reports_encoding_and_the_policy_s_heat() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let features = probe_server_features(&id, 0).await.expect("probe");
        assert_eq!(features.status(ServerCommand::ObjectEncoding), CommandStatus::Available);
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;

        // A short string is `embstr`; a small hash is a listpack (`ziplist`
        // on the oldest servers in the matrix) — the exact word is the
        // server's business, so assert the shape, not a spelling.
        let key = unique("object-meta");
        let _: () = cmd("SET").arg(&key).arg("v").query_async(&mut c).await.expect("set");
        let heat = get_server_heat_probe(&id);
        let (encoding, metric) = client.object_meta(&key, true, heat).await;
        assert_eq!(encoding, "embstr", "string encoding");

        // Whether FREQ or IDLETIME comes back is the policy's decision, and
        // the one the probe made must be the one that answers.
        let policy = client.maxmemory_policy().await.expect("policy");
        match HeatProbe::from_policy(&policy) {
            HeatProbe::Freq => {
                assert!(matches!(metric, HeatMetric::Freq(_)), "{policy} → {metric:?}");
                assert_eq!(heat, HeatProbe::Freq);
            }
            HeatProbe::IdleTime => {
                assert!(matches!(metric, HeatMetric::IdleTime(_)), "{policy} → {metric:?}");
                assert_eq!(heat, HeatProbe::IdleTime);
            }
            // Only when CONFIG GET is denied, which it is not here.
            HeatProbe::None => panic!("standalone reported no maxmemory-policy"),
        }

        let hash = unique("object-meta-hash");
        let _: () = cmd("HSET")
            .arg(&hash)
            .arg("f")
            .arg("v")
            .query_async(&mut c)
            .await
            .expect("hset");
        let (hash_encoding, _) = client.object_meta(&hash, true, heat).await;
        assert!(
            hash_encoding == "listpack" || hash_encoding == "ziplist",
            "small hash encoding: {hash_encoding}"
        );

        // The tolerance the chip is built on: a key that is gone, and a
        // caller that asked for nothing, both answer "nothing to show"
        // rather than failing the value load they decorate.
        let (missing, missing_heat) = client.object_meta(&unique("object-meta-absent"), true, heat).await;
        assert_eq!(missing, "");
        assert_eq!(missing_heat, HeatMetric::None);
        let (none, none_heat) = client.object_meta(&key, false, HeatProbe::None).await;
        assert_eq!(none, "");
        assert_eq!(none_heat, HeatMetric::None);

        let _: () = cmd("DEL")
            .arg(&key)
            .arg(&hash)
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// `EXPIREAT` is the key editor's other TTL mode: the absolute instant the
/// duration field cannot express without arithmetic that goes stale while
/// it is typed. The UI refuses a past instant (that is the delete button's
/// job), so what has to hold here is only that a future one lands as a
/// readable countdown.
#[test]
#[ignore]
fn standalone_expireat_sets_an_absolute_deadline() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let key = unique("expireat");
        let _: () = cmd("SET").arg(&key).arg("v").query_async(&mut c).await.expect("set");

        let now: i64 = cmd("TIME").query_async::<(i64, i64)>(&mut c).await.expect("time").0;
        let at = now + 3600;
        let applied: i64 = cmd("EXPIREAT")
            .arg(&key)
            .arg(at)
            .query_async(&mut c)
            .await
            .expect("expireat");
        assert_eq!(applied, 1);
        let ttl: i64 = cmd("TTL").arg(&key).query_async(&mut c).await.expect("ttl");
        // The server rounds; anything inside the hour proves the deadline
        // was taken as an instant and not as a duration.
        assert!((3500..=3600).contains(&ttl), "ttl after EXPIREAT: {ttl}");

        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// `CONFIG SET` only changes the running configuration; `CONFIG REWRITE`
/// is what makes it survive a restart, and it needs the server to have been
/// started with a config file. The harness starts this one from command-line
/// arguments alone, which is exactly the case the config editor has to
/// detect — it reads `config_file` from `INFO server` and, when empty, says
/// the edits are runtime-only instead of offering a button that can only
/// fail.
#[test]
#[ignore]
fn standalone_without_a_config_file_cannot_rewrite_it() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let info: String = cmd("INFO")
            .arg("server")
            .query_async(&mut c)
            .await
            .expect("info server");
        let config_file = info
            .lines()
            .find_map(|line| line.trim().strip_prefix("config_file:"))
            .expect("INFO server reports config_file")
            .trim();
        assert!(
            config_file.is_empty(),
            "the harness starts this server without a config file: {config_file}"
        );
        let rewritten: Result<String, _> = cmd("CONFIG").arg("REWRITE").query_async(&mut c).await;
        assert!(
            rewritten.is_err(),
            "CONFIG REWRITE needs a config file — that is why the button is hidden"
        );
    });
}

#[test]
#[ignore]
fn standalone_acl_users_are_classified() {
    smol::block_on(async {
        let admin_id = register(server("it-standalone", standalone())).await;
        let mut admin = conn(&admin_id, 0).await;
        let acl_ok: Result<String, _> = cmd("ACL").arg("WHOAMI").query_async(&mut admin).await;
        if acl_ok.is_err() {
            eprintln!("skipped: server has no ACL (Redis < 6)");
            return;
        }
        let suffix = unique("acl").rsplit(':').take(3).collect::<Vec<_>>().join("_");
        let ro_user = format!("zedis_it_ro_{suffix}");
        let limited_user = format!("zedis_it_limited_{suffix}");
        // Read-only: every read, no writes. Container-level denials keep
        // the rule valid on Redis 6 (subcommand denials are 7.0+). `SELECT`
        // is granted by name: it joined `@connection` only in 7.0 — on 6.2 it
        // sits in `@keyspace @fast`, and `@keyspace` also holds writes — and
        // the probe db below is 15, so the connect itself would be denied.
        cmd("ACL")
            .arg("SETUSER")
            .arg(&ro_user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("&*")
            .arg("+@read")
            .arg("+@connection")
            .arg("+select")
            .arg("+acl")
            .arg("+info")
            .arg("+scan")
            .exec_async(&mut admin)
            .await
            .expect("setuser ro");
        cmd("ACL")
            .arg("SETUSER")
            .arg(&limited_user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("&*")
            .arg("+@all")
            .arg("-config")
            .arg("-slowlog")
            .arg("-latency")
            .arg("-flushdb")
            .arg("-flushall")
            .arg("-bgsave")
            .exec_async(&mut admin)
            .await
            .expect("setuser limited");

        // Three more shapes the probe has to get right without writing:
        // a plain read-only user (reads, `INFO`, the connection commands —
        // but no `acl`, so not even `ACL WHOAMI`: the check used to give up
        // on it and report writable); a user scoped to `app:*` who *can*
        // write (the fixed-key probe used to lock the UI for them); and an
        // app user without `@admin`, who cannot run `ACL DRYRUN` and so
        // exercises the no-op write on 7+.
        let plain_user = format!("zedis_it_plain_{suffix}");
        let scoped_user = format!("zedis_it_scoped_{suffix}");
        let app_user = format!("zedis_it_app_{suffix}");
        for (user, rules) in [
            (
                &plain_user,
                vec!["~*", "&*", "+@read", "+@connection", "+select", "+info"],
            ),
            (&scoped_user, vec!["~app:*", "&*", "+@all"]),
            (&app_user, vec!["~*", "&*", "+@all", "-@admin"]),
        ] {
            let mut c = cmd("ACL");
            c.arg("SETUSER").arg(user).arg("on").arg(">pw");
            for rule in rules {
                c.arg(rule);
            }
            c.exec_async(&mut admin).await.expect("setuser");
        }

        // The probes run on a db of their own, so a concurrent test's writes
        // can't blur the "nothing was written" check.
        let admin_client = get_connection_manager()
            .get_client(&admin_id, 0)
            .await
            .expect("admin client");
        let probe_db = if admin_client.databases() > 15 { 15 } else { 0 };
        let mut probe_admin = conn(&admin_id, probe_db).await;
        let dbsize_before: i64 = cmd("DBSIZE").query_async(&mut probe_admin).await.expect("dbsize");

        let connect = |name: &str, user: &str| {
            let mut s = server(&format!("it-{name}-{suffix}"), standalone());
            s.username = Some(user.to_string());
            s.password = Some("pw".into());
            s
        };
        let ro_id = register(connect("ro", &ro_user)).await;
        let plain_id = register(connect("plain", &plain_user)).await;
        let scoped_id = register(connect("scoped", &scoped_user)).await;
        let app_id = register(connect("app", &app_user)).await;
        async fn mode(id: &str, db: usize) -> String {
            let client = get_connection_manager().get_client(id, db).await.expect("client");
            format!("{:?}", client.access_mode())
        }
        assert_eq!(
            mode(&ro_id, probe_db).await,
            "StrictReadOnly",
            "a read-only ACL user must be detected (DRYRUN on 7+, the no-op SET before)"
        );
        assert_eq!(
            mode(&plain_id, probe_db).await,
            "StrictReadOnly",
            "a read-only user who cannot even run ACL WHOAMI must still be detected"
        );
        assert_eq!(
            mode(&scoped_id, probe_db).await,
            "ReadWrite",
            "a user scoped to a key pattern writes within it — not read-only"
        );
        assert_eq!(
            mode(&app_id, probe_db).await,
            "ReadWrite",
            "an app user without @admin writes"
        );

        // None of those four connects left a trace in the dataset.
        let dbsize_after: i64 = cmd("DBSIZE").query_async(&mut probe_admin).await.expect("dbsize");
        assert_eq!(dbsize_before, dbsize_after, "the read-only probe must not write a key");
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("zedis:acl-probe:*")
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut probe_admin)
                .await
                .expect("scan");
            assert!(keys.is_empty(), "probe keys left behind: {keys:?}");
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        let legacy: bool = cmd("EXISTS")
            .arg("_zedis_auth_test_")
            .query_async(&mut probe_admin)
            .await
            .expect("exists");
        assert!(!legacy, "the old throwaway key must be gone for good");

        let mut limited = server(&format!("it-limited-{suffix}"), standalone());
        limited.username = Some(limited_user.clone());
        limited.password = Some("pw".into());
        let limited_id = register(limited).await;
        let features = probe_server_features(&limited_id, 0).await.expect("probe");
        for c in [
            ServerCommand::ConfigGet,
            ServerCommand::SlowlogGet,
            ServerCommand::LatencyLatest,
        ] {
            assert_eq!(features.status(c), CommandStatus::Denied, "{c:?}");
        }
        assert_eq!(features.status(ServerCommand::Scan), CommandStatus::Available);
        if supports(&admin_id, floors::ACL_V2).await {
            for c in [ServerCommand::ConfigSet, ServerCommand::Bgsave, ServerCommand::FlushDb] {
                assert_eq!(features.status(c), CommandStatus::Denied, "{c:?} (ACL DRYRUN)");
            }
        }

        for user in [&ro_user, &limited_user, &plain_user, &scoped_user, &app_user] {
            cmd("ACL")
                .arg("DELUSER")
                .arg(user)
                .exec_async(&mut admin)
                .await
                .expect("deluser");
        }
    });
}

#[test]
#[ignore]
fn standalone_recovers_after_the_server_drops_the_link() {
    smol::block_on(async {
        // Its own server id, so the pooled client it kills is not the one
        // the other (parallel) standalone tests share.
        let id = register(server("it-standalone-linkloss", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping before");
        let mut pooled = conn(&id, 0).await;
        let victim: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut pooled)
            .await
            .expect("client id");

        // Kill exactly that link from a throwaway connection — the pooled
        // multiplexed connection dies the way it does on laptop wake / VPN
        // flip / server restart.
        let mut killer = open_single_connection(&server("it-killer", standalone()), 0, false)
            .await
            .expect("killer connection");
        let killed: i64 = cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(victim)
            .query_async(&mut killer)
            .await
            .expect("client kill");
        assert_eq!(killed, 1);

        let err = client.ping().await.expect_err("the cached link must be dead now");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Network, "{err}");

        // What the app does on that error (note_link_error / heartbeat):
        // drop the cached client and let the next call rebuild it.
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("rebuilt client");
        client.ping().await.expect("ping after rebuild");
    });
}

/// The terminal's connection (`open_dedicated_connection`) shares nothing
/// with the pooled one: a `SELECT` typed there must not move the db the key
/// tree scans on. Before this existed the terminal ran on the pooled
/// connection, and `SELECT 3` silently redirected every later `SCAN`.
#[test]
#[ignore]
fn standalone_dedicated_connection_keeps_select_to_itself() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut pooled = conn(&id, 0).await;
        let mut dedicated = get_connection_manager()
            .open_dedicated_connection(&id, 0)
            .await
            .expect("dedicated connection");

        // Two sockets, not two handles onto one.
        let pooled_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut pooled)
            .await
            .expect("client id");
        let dedicated_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut dedicated)
            .await
            .expect("client id");
        assert_ne!(
            pooled_id, dedicated_id,
            "the dedicated connection must be its own client"
        );

        let _: () = cmd("SELECT")
            .arg(1)
            .query_async(&mut dedicated)
            .await
            .expect("select 1");
        let key = unique("dedicated");
        let _: () = cmd("SET")
            .arg(&key)
            .arg("1")
            .arg("EX")
            .arg(60)
            .query_async(&mut dedicated)
            .await
            .expect("set on db 1");

        // The dedicated connection stayed on db 1 …
        let on_dedicated: bool = cmd("EXISTS")
            .arg(&key)
            .query_async(&mut dedicated)
            .await
            .expect("exists");
        assert!(
            on_dedicated,
            "the SELECT must hold for later commands on the same connection"
        );
        // … and the pooled one never left db 0.
        let on_pooled: bool = cmd("EXISTS").arg(&key).query_async(&mut pooled).await.expect("exists");
        assert!(
            !on_pooled,
            "a SELECT on the dedicated connection leaked into the pooled one"
        );
        let mut pooled_again = conn(&id, 0).await;
        let on_pooled_again: bool = cmd("EXISTS")
            .arg(&key)
            .query_async(&mut pooled_again)
            .await
            .expect("exists");
        assert!(
            !on_pooled_again,
            "the cached client must still hand out a db-0 connection"
        );

        let _: () = cmd("DEL").arg(&key).query_async(&mut dedicated).await.expect("cleanup");
    });
}

/// The collection editors' paging and writes (ADR 10): every one of these
/// sat inline in `src/states` and so had no test at all until it moved.
/// Bytes are kept as the server answered them — a member that is not UTF-8
/// is a row to draw, not a row to drop.
#[test]
#[ignore]
fn standalone_collections_page_and_write_through_their_operations() {
    smol::block_on(async {
        let id = register(server("it-collections", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let prefix = unique("coll");
        let binary = b"\xffb".as_slice();

        // ── list ───────────────────────────────────────────────────────────
        let list = format!("{prefix}:list");
        for (n, item) in ["a", "b", "c"].iter().enumerate() {
            assert_eq!(
                list_push(&at, &list, item.as_bytes(), false).await.expect("rpush"),
                n + 1
            );
        }
        assert_eq!(list_push(&at, &list, binary, true).await.expect("lpush"), 4);
        assert_eq!(list_len(&at, &list).await.expect("llen"), 4);
        assert_eq!(
            list_range(&at, &list, 0, 1).await.expect("lrange"),
            vec![binary.to_vec(), b"a".to_vec()],
            "the page is inclusive on both ends, bytes as answered"
        );
        // The row is written only while it still holds what was loaded.
        assert!(
            list_set_if_unchanged(&at, &list, 1, b"a", b"A").await.expect("lset"),
            "unchanged: written"
        );
        assert!(
            !list_set_if_unchanged(&at, &list, 1, b"a", b"Z").await.expect("lset"),
            "somebody else changed it: refused"
        );
        assert_eq!(list_range(&at, &list, 1, 1).await.expect("lrange"), vec![b"A".to_vec()]);
        assert_eq!(remove_list_indexes(&at, &list, &[0, 2]).await.expect("remove"), 2);
        assert_eq!(list_len(&at, &list).await.expect("llen"), 2);

        // ── set ────────────────────────────────────────────────────────────
        let set = format!("{prefix}:set");
        assert!(set_add(&at, &set, b"one").await.expect("sadd"), "new member");
        assert!(!set_add(&at, &set, b"one").await.expect("sadd"), "already there");
        for member in [b"two".as_slice(), b"three".as_slice(), binary] {
            set_add(&at, &set, member).await.expect("sadd");
        }
        assert_eq!(set_card(&at, &set).await.expect("scard"), 4);
        let (cursor, members) = set_scan(&at, &set, None, 0, 100).await.expect("sscan");
        assert_eq!((cursor, members.len()), (0, 4), "one round covers a small set");
        assert!(members.contains(&binary.to_vec()), "bytes as answered");
        let (_, filtered) = set_scan(&at, &set, Some("t"), 0, 100).await.expect("sscan");
        let mut filtered: Vec<Vec<u8>> = filtered;
        filtered.sort();
        assert_eq!(
            filtered,
            [b"three".to_vec(), b"two".to_vec()],
            "the keyword is a substring"
        );
        assert!(
            set_replace_member(&at, &set, b"one", b"uno").await.expect("edit"),
            "the new member is new"
        );
        assert!(
            !set_replace_member(&at, &set, b"uno", b"two").await.expect("edit"),
            "edited onto an existing member: the two merged"
        );
        assert_eq!(set_remove(&at, &set, &[b"two", binary]).await.expect("srem"), 2);
        assert_eq!(set_remove(&at, &set, &[]).await.expect("nothing"), 0);

        // ── hash ───────────────────────────────────────────────────────────
        let hash = format!("{prefix}:hash");
        for (field, value) in [("f1", "v1"), ("f2", "v2"), ("other", "v3")] {
            write_hash_field(&at, &hash, field.as_bytes(), value.as_bytes(), FieldTtl::Persist, false)
                .await
                .expect("hset");
        }
        assert_eq!(hash_len(&at, &hash).await.expect("hlen"), 3);
        let (cursor, pairs) = hash_scan(&at, &hash, Some("f"), 0, 100).await.expect("hscan");
        assert_eq!(cursor, 0);
        let mut names: Vec<Vec<u8>> = pairs.iter().map(|(field, _)| field.clone()).collect();
        names.sort();
        assert_eq!(names, [b"f1".to_vec(), b"f2".to_vec()]);
        if supports(&id, floors::HASH_FIELD_TTL).await {
            write_hash_field(&at, &hash, b"f1", b"v1", FieldTtl::Expire(120), false)
                .await
                .expect("hset with ttl");
            let ttls = hash_field_ttls(&at, &hash, &[b"f1", b"f2", b"gone"])
                .await
                .expect("httl");
            assert!((1..=120).contains(&ttls[0]), "{ttls:?}");
            assert_eq!((ttls[1], ttls[2]), (-1, -2), "no TTL, and no field");
        }
        assert!(hash_field_ttls(&at, &hash, &[]).await.expect("no fields").is_empty());
        assert_eq!(
            hash_delete_fields(&at, &hash, &[b"f2", b"gone"]).await.expect("hdel"),
            1
        );
        assert_eq!(hash_delete_fields(&at, &hash, &[]).await.expect("nothing"), 0);

        // ── sorted set ─────────────────────────────────────────────────────
        let zset = format!("{prefix}:zset");
        for (member, score) in [("a", 1.0), ("b", 2.0), ("c", 3.0)] {
            assert!(
                zset_put(&at, &zset, member.as_bytes(), score, None)
                    .await
                    .expect("zadd"),
                "new member"
            );
        }
        assert_eq!(zset_card(&at, &zset).await.expect("zcard"), 3);
        assert_eq!(
            zset_range(&at, &zset, false, 0, 1).await.expect("zrange"),
            vec![(b"a".to_vec(), 1.0), (b"b".to_vec(), 2.0)]
        );
        assert_eq!(
            zset_range(&at, &zset, true, 0, 0).await.expect("zrevrange"),
            vec![(b"c".to_vec(), 3.0)],
            "descending starts at the top score"
        );
        assert_eq!(zset_count_by_score(&at, &zset, "2", "+inf").await.expect("zcount"), 2);
        // A score window pages by LIMIT; its min and max are passed the same
        // way round in both directions.
        assert_eq!(
            zset_range_by_score(&at, &zset, false, ("2", "+inf"), 0, 1)
                .await
                .expect("window"),
            vec![(b"b".to_vec(), 2.0)]
        );
        assert_eq!(
            zset_range_by_score(&at, &zset, true, ("2", "+inf"), 0, 1)
                .await
                .expect("window"),
            vec![(b"c".to_vec(), 3.0)]
        );
        let (cursor, scanned) = zset_scan(&at, &zset, 0, "[ab]", 100).await.expect("zscan");
        assert_eq!(cursor, 0);
        assert_eq!(scanned.len(), 2, "the pattern is a glob, as typed: {scanned:?}");
        // An edit that renames a member adds the new one and drops the old;
        // re-scoring the same member adds nothing.
        assert!(
            zset_put(&at, &zset, b"A", 9.0, Some(b"a")).await.expect("rename"),
            "the renamed-to member is new to the set"
        );
        assert_eq!(zset_card(&at, &zset).await.expect("zcard"), 3, "renamed, not added");
        assert!(
            !zset_put(&at, &zset, b"A", 0.5, Some(b"A")).await.expect("rescore"),
            "the same member, a new score"
        );
        assert_eq!(
            zset_range(&at, &zset, false, 0, 0).await.expect("zrange"),
            vec![(b"A".to_vec(), 0.5)],
            "the new score put it first"
        );
        assert_eq!(zset_remove(&at, &zset, &[b"A", b"gone"]).await.expect("zrem"), 1);
        assert_eq!(zset_remove(&at, &zset, &[]).await.expect("nothing"), 0);

        // ── string ─────────────────────────────────────────────────────────
        let string = format!("{prefix}:string");
        assert!(matches!(
            string_set(&at, &string, binary, 0, None).await.expect("set"),
            StringWrite::Saved(Some(size)) if size > 0
        ));
        assert_eq!(string_get(&at, &string).await.expect("get"), binary, "bytes, not text");
        // The TTL survives a save: KEEPTTL where the server has it, a
        // re-applied PX where it does not.
        let _: () = cmd("EXPIRE")
            .arg(&string)
            .arg(120)
            .query_async(&mut c)
            .await
            .expect("expire");
        string_set(&at, &string, b"second", 120_000, None).await.expect("set");
        let ttl: i64 = cmd("TTL").arg(&string).query_async(&mut c).await.expect("ttl");
        assert!((1..=120).contains(&ttl), "the save must not drop the expiry: {ttl}");
        if supports(&id, floors::SET_IFEQ).await {
            assert!(matches!(
                string_set(&at, &string, b"third", 0, Some(b"second"))
                    .await
                    .expect("cas"),
                StringWrite::Saved(_)
            ));
            assert_eq!(
                string_set(&at, &string, b"fourth", 0, Some(b"second"))
                    .await
                    .expect("cas"),
                StringWrite::Conflict,
                "the value moved under us: refused, not clobbered"
            );
            assert_eq!(string_get(&at, &string).await.expect("get"), b"third");
        }

        let _: () = cmd("DEL")
            .arg(&[&list, &set, &hash, &zset, &string])
            .query_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// What the key tree and the key header ask of a key: scanned, typed, sized,
/// renamed, expired, created, deleted.
#[test]
#[ignore]
fn standalone_keyspace_operations_answer_for_one_key_and_for_a_prefix() {
    smol::block_on(async {
        let id = register(server("it-keyspace", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let prefix = unique("ks");

        // Created by its type's first write, and never over an existing key.
        assert!(
            create_key(&at, &format!("{prefix}:s"), "SET", &["v".to_string()], Some(120))
                .await
                .expect("create"),
            "a name nobody has"
        );
        assert!(
            !create_key(&at, &format!("{prefix}:s"), "SET", &["other".to_string()], None)
                .await
                .expect("create"),
            "taken: nothing sent"
        );
        assert_eq!(
            string_get(&at, &format!("{prefix}:s")).await.expect("get"),
            b"v",
            "the refused create wrote nothing"
        );
        create_key(&at, &format!("{prefix}:h"), "HSET", &["f".into(), "v".into()], None)
            .await
            .expect("create");

        let (t, ttl) = key_type_and_ttl(&at, &format!("{prefix}:s")).await.expect("type + ttl");
        assert_eq!(t, "string");
        assert!((1..=120).contains(&ttl), "the create applied the TTL: {ttl}");
        assert_eq!(
            key_type_and_ttl(&at, &format!("{prefix}:gone")).await.expect("missing"),
            ("none".to_string(), -2),
            "-2 is the server saying the key is not there"
        );
        assert_eq!(
            key_types(
                &at,
                vec![format!("{prefix}:s"), format!("{prefix}:h"), "no-such".into()]
            )
            .await
            .expect("types"),
            ["string", "hash", "none"],
            "one answer per key, in order"
        );
        assert!(key_types(&at, Vec::new()).await.expect("no keys").is_empty());
        assert!(
            key_memory_usage(&at, &format!("{prefix}:s"), "string")
                .await
                .expect("size")
                > 0
        );
        let (encoding, _heat) = key_object_meta(&at, &format!("{prefix}:s"), true, HeatProbe::None).await;
        assert!(!encoding.is_empty(), "OBJECT ENCODING answers on a standalone");

        // The scan is what the key tree pages with.
        let (cursors, rows) = scan_page(&at, None, &format!("{prefix}:*"), 100, true, None)
            .await
            .expect("scan");
        assert_eq!(cursors.iter().sum::<u64>(), 0, "one round covered two keys");
        let mut names: Vec<&str> = rows.iter().map(|(key, _, _)| key.as_str()).collect();
        names.sort();
        assert_eq!(names, [format!("{prefix}:h"), format!("{prefix}:s")]);
        let typed = scan_page(&at, None, &format!("{prefix}:*"), 100, false, Some("hash"))
            .await
            .expect("scan")
            .1;
        assert_eq!(typed.len(), 1, "the TYPE filter is the server's: {typed:?}");

        // Renamed, with and without the overwrite the dialog offers.
        assert!(
            rename_key(&at, &format!("{prefix}:h"), &format!("{prefix}:hash"), false)
                .await
                .expect("renamenx")
        );
        assert!(
            !rename_key(&at, &format!("{prefix}:hash"), &format!("{prefix}:s"), false)
                .await
                .expect("renamenx"),
            "the new name is taken: nothing moved"
        );
        assert!(
            rename_key(&at, &format!("{prefix}:hash"), &format!("{prefix}:s"), true)
                .await
                .expect("rename"),
            "overwrite says so"
        );
        assert_eq!(
            key_type_and_ttl(&at, &format!("{prefix}:s")).await.expect("type").0,
            "hash"
        );

        // TTLs: one key, then a whole prefix with its condition.
        expire_key(&at, &format!("{prefix}:s"), 300).await.expect("expire");
        assert!((1..=300).contains(&key_type_and_ttl(&at, &format!("{prefix}:s")).await.expect("ttl").1));
        let deadline = chrono::Utc::now().timestamp() + 600;
        expire_key_at(&at, &format!("{prefix}:s"), deadline)
            .await
            .expect("expireat");
        assert!((300..=600).contains(&key_type_and_ttl(&at, &format!("{prefix}:s")).await.expect("ttl").1));
        for n in 0..3 {
            create_key(&at, &format!("{prefix}:b{n}"), "SET", &["v".to_string()], None)
                .await
                .expect("create");
        }
        let applied = set_keys_ttl(
            &at,
            vec![format!("{prefix}:b0"), format!("{prefix}:b1")],
            Some(60),
            None,
        )
        .await
        .expect("batch ttl");
        assert_eq!(applied, [true, true]);
        if supports(&id, floors::EXPIRE_CONDITIONS).await {
            let (changed, skipped) =
                set_ttl_matching(&at, &format!("{prefix}:b*"), Some(90), Some(ExpireCondition::Nx))
                    .await
                    .expect("prefix ttl");
            assert_eq!(
                (changed.len(), skipped),
                (1, 2),
                "NX only touches the one key without a TTL: {changed:?}"
            );
        }

        // Deleted: one key, a list of them, and a whole prefix.
        delete_key(&at, &format!("{prefix}:b0")).await.expect("del");
        delete_keys(&at, vec![format!("{prefix}:b1"), format!("{prefix}:gone")])
            .await
            .expect("del keys");
        delete_keys_matching(&at, &format!("{prefix}:*"))
            .await
            .expect("del prefix");
        assert!(
            scan_page(&at, None, &format!("{prefix}:*"), 1000, false, None)
                .await
                .expect("scan")
                .1
                .is_empty(),
            "the prefix is gone"
        );

        // The recycle bin's snapshot, and what it refuses to keep.
        let bin = format!("{prefix}:bin");
        let _: () = cmd("SET")
            .arg(&bin)
            .arg("keepme")
            .arg("EX")
            .arg(120)
            .query_async(&mut c)
            .await
            .expect("set");
        let snapshot = snapshot_key(&at, &bin, 1 << 20, 1 << 20).await.expect("a small value");
        assert!((1..=120_000).contains(&snapshot.pttl_ms), "{}", snapshot.pttl_ms);
        assert!(
            snapshot_key(&at, &bin, 1, 1 << 20).await.is_none(),
            "over the memory cap"
        );
        assert!(
            snapshot_key(&at, &bin, 1 << 20, 1).await.is_none(),
            "over the payload cap"
        );
        assert!(
            snapshot_key(&at, &format!("{prefix}:never"), 1 << 20, 1 << 20)
                .await
                .is_none()
        );
        // What it kept is what RESTORE takes back.
        delete_key(&at, &bin).await.expect("del");
        restore_key(&at, &bin, snapshot.pttl_ms, &snapshot.payload)
            .await
            .expect("restore");
        assert_eq!(string_get(&at, &bin).await.expect("get"), b"keepme");
        assert_eq!(dump_key(&at, &bin).await.expect("dump"), snapshot.payload);
        delete_key(&at, &bin).await.expect("cleanup");
    });
}

/// The stream editor's page, its `XINFO` panel and its writes.
#[test]
#[ignore]
fn standalone_stream_operations_page_describe_and_administer() {
    smol::block_on(async {
        let id = register(server("it-stream-ops", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("stream-ops");

        for n in 1..=3 {
            let entry = stream_add(&at, &key, "*", &[("n".to_string(), n.to_string())])
                .await
                .expect("xadd");
            assert!(entry.contains('-'), "the server minted an id: {entry}");
        }
        assert_eq!(stream_len(&at, &key).await.expect("xlen"), 3);

        // Paging: a full page hands back a cursor, the last one does not.
        let (cursor, first) = stream_page(&at, &key, None, 2, false).await.expect("xrange");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].1, vec![("n".to_string(), "1".to_string())]);
        assert!(!cursor.is_empty(), "more to come");
        let (next, rest) = stream_page(&at, &key, Some(&cursor), 2, false).await.expect("xrange");
        assert_eq!(rest.len(), 1, "the cursor is exclusive");
        assert!(next.is_empty(), "the end of the stream");
        let (_, newest) = stream_page(&at, &key, None, 1, true).await.expect("xrevrange");
        assert_eq!(
            newest[0].1,
            vec![("n".to_string(), "3".to_string())],
            "reverse starts at the top"
        );

        // The info panel: the stream, its group, its consumer, its PEL.
        let group = "g1";
        group_create(&at, &key, group, "0").await.expect("xgroup create");
        assert!(consumer_create(&at, &key, group, "c1").await.expect("createconsumer"));
        assert!(
            !consumer_create(&at, &key, group, "c1").await.expect("again"),
            "already there"
        );
        // Read two entries so the group has a pending list to describe.
        let _: redis::Value = cmd("XREADGROUP")
            .arg("GROUP")
            .arg(group)
            .arg("c1")
            .arg("COUNT")
            .arg(2)
            .arg("STREAMS")
            .arg(&key)
            .arg(">")
            .query_async(&mut c)
            .await
            .expect("xreadgroup");

        let info = stream_info(&at, &key).await.expect("xinfo");
        let summary = info.summary.expect("XINFO STREAM answers on a standalone");
        assert_eq!(summary.groups_count, 1);
        assert!(summary.first_entry_id.contains('-') && summary.last_entry_id.contains('-'));
        assert_eq!(
            summary.last_generated_id, summary.last_entry_id,
            "nothing has been deleted from the top yet"
        );
        assert!(summary.radix_tree_keys > 0);
        let [described]: [StreamGroup; 1] = info.groups.try_into().expect("one group");
        assert_eq!((described.name.as_str(), described.pending_count), (group, 2));
        assert_eq!(described.lag, 1, "one entry nobody has been delivered");
        assert_eq!(described.consumers.len(), 1);
        assert_eq!(described.consumers[0].name, "c1");
        assert_eq!(described.consumers[0].pending, 2);
        assert_eq!(described.pending_entries.len(), 2);
        assert!(described.pending_done, "a PEL of two is the whole page");
        let pending = &described.pending_entries[0];
        assert_eq!((pending.consumer.as_str(), pending.delivery_count), ("c1", 1));

        // A page of the PEL on its own, which is what "load more" asks for.
        let page = pending_page(&at, &key, group, "-").await.expect("xpending");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, pending.id);

        // Claiming, acknowledging, releasing.
        consumer_create(&at, &key, group, "c2").await.expect("createconsumer");
        stream_claim(&at, &key, group, "c2", &page[0].id).await.expect("xclaim");
        let claimed = stream_info(&at, &key).await.expect("xinfo").groups.remove(0);
        let owner = claimed
            .pending_entries
            .iter()
            .find(|entry| entry.id == page[0].id)
            .expect("still pending");
        assert_eq!(owner.consumer, "c2", "the claim moved it");
        stream_ack(&at, &key, group, &page[0].id).await.expect("xack");
        assert_eq!(
            stream_info(&at, &key).await.expect("xinfo").groups[0].pending_count,
            1,
            "the acknowledged entry left the PEL"
        );
        if supports(&id, floors::XAUTOCLAIM).await {
            let count = stream_autoclaim(&at, &key, group, "c2", 0, 10)
                .await
                .expect("xautoclaim");
            assert_eq!(count, 1, "min-idle 0 claims what is left");
        }
        assert_eq!(consumer_delete(&at, &key, group, "c2").await.expect("delconsumer"), 1);
        assert_eq!(consumer_delete(&at, &key, group, "c1").await.expect("delconsumer"), 0);

        // The group's read position, and dropping it.
        group_set_id(&at, &key, group, "0").await.expect("xgroup setid");
        assert_eq!(
            stream_info(&at, &key).await.expect("xinfo").groups[0].last_delivered_id,
            "0-0",
            "rewound to the start"
        );
        group_destroy(&at, &key, group).await.expect("xgroup destroy");
        assert!(stream_info(&at, &key).await.expect("xinfo").groups.is_empty());

        // Deleting and trimming entries.
        let ids: Vec<String> = stream_page(&at, &key, None, 10, false)
            .await
            .expect("page")
            .1
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            stream_delete(&at, &key, &[ids[0].as_str(), "9999999-0"])
                .await
                .expect("xdel"),
            1
        );
        assert_eq!(stream_delete(&at, &key, &[]).await.expect("nothing"), 0);
        assert_eq!(
            stream_trim(&at, &key, &StreamTrim::MaxLen(1), None)
                .await
                .expect("xtrim"),
            1
        );
        assert_eq!(stream_len(&at, &key).await.expect("xlen"), 1);
        // XSETID moves last-generated-id, which is not the last entry's.
        stream_set_id(&at, &key, "9999999999999-0").await.expect("xsetid");
        let summary = stream_info(&at, &key).await.expect("xinfo").summary.expect("summary");
        assert_eq!(summary.last_generated_id, "9999999999999-0");
        assert_ne!(
            summary.last_entry_id, "9999999999999-0",
            "the entry itself did not move"
        );
        assert_eq!(
            stream_trim(&at, &key, &StreamTrim::MinId("9999999999999-0".to_string()), None)
                .await
                .expect("xtrim minid"),
            1
        );

        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// The server-wide operations behind the status bar and the admin panels:
/// what a connect learns, what a heartbeat asks, and the forks.
#[test]
#[ignore]
fn standalone_server_operations_describe_and_administer_the_server() {
    smol::block_on(async {
        let id = register(server("it-server-ops", standalone())).await;
        let at = ServerDb::new(&id, 0);

        let summary = server_summary(&at).await.expect("summary");
        assert!(!summary.version.is_empty());
        // Replica count is not asserted: `replication_pair_…` attaches one to
        // a server of its own, and the suite runs in parallel.
        assert_eq!(summary.nodes.0, 1, "a standalone is one master");
        assert_eq!(summary.description.server_type, "Standalone");
        assert!(summary.databases >= 1);
        // Not compared against `summary.dbsize`: the suite runs in parallel
        // against this server, so two reads of a live counter differ.
        dbsize(&at).await.expect("dbsize");
        assert!(server_supports(&at, floors::MEMORY_USAGE).await.expect("floor"));

        // The heartbeat: with one master the probe *is* the INFO.
        let probed = heartbeat_probe(&at).await.expect("probe");
        let info = probed.clone().expect("a standalone answers the INFO itself");
        assert!(
            info.contains("redis_version:") || info.contains("valkey_version:"),
            "{info:.60}"
        );
        // Which is then not asked for a second time.
        let infos = master_infos(&at, probed).await.expect("infos");
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].1, info);
        // Without one, it is fetched per master.
        let fetched = master_infos(&at, None).await.expect("infos");
        assert_eq!(fetched.len(), 1);
        assert!(fetched[0].1.contains("# Server"));
        assert!(slow_logs(&at).await.is_ok(), "the slow log is sampled with the beat");

        // The forks. The reply is a status line that differs between forks
        // and is not read; what is checked is that the server took it. In
        // this order: an AOF rewrite asked for while a BGSAVE runs is
        // *scheduled*, while a BGSAVE asked for during a rewrite is refused.
        bgsave(&at).await.expect("bgsave");
        bgrewriteaof(&at).await.expect("bgrewriteaof");
        let saved = master_infos(&at, None).await.expect("infos");
        assert!(
            saved[0].1.contains("rdb_last_bgsave_status"),
            "the persistence panel reads this back"
        );

        // Dropping the pooled client sends nothing and costs nothing: the
        // next call rebuilds it, which is how a failover is followed.
        forget_client(&at);
        assert_eq!(server_summary(&at).await.expect("reconnect").nodes.0, 1);
    });
}

/// The terminal's session (ADR 10): one connection of its own, opened by the
/// first line and shared by its clones, holding what was typed into it — the
/// db a `SELECT` picked, a `MULTI` still open — and a fresh session is how
/// all of that is forgotten.
#[test]
#[ignore]
fn standalone_terminal_session_keeps_its_own_connection_state() {
    smol::block_on(async {
        let id = register(server("it-terminal-session", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let session = TerminalSession::default();
        let key = unique("terminal");

        let moved = session.run(&at, "SELECT", &args(&["1"])).await.expect("select");
        assert_eq!(moved.selected_db(), Some(1));
        // A clone is the same connection: it is still on db 1.
        let set = session
            .clone()
            .run(&at, "SET", &args(&[&key, "v", "EX", "60"]))
            .await
            .expect("set");
        assert!(set.is_ok());
        let mut pooled = conn(&id, 0).await;
        let on_db0: bool = cmd("EXISTS").arg(&key).query_async(&mut pooled).await.expect("exists");
        assert!(!on_db0, "the terminal's SELECT leaked into the pooled connection");

        // MULTI … EXEC across separate calls: the transaction lives on the
        // session's connection.
        assert!(session.run(&at, "MULTI", &[]).await.expect("multi").is_ok());
        assert!(
            session
                .run(&at, "INCR", &args(&[&format!("{key}:n")]))
                .await
                .expect("incr")
                .is_queued()
        );
        assert!(session.run(&at, "GET", &args(&[&key])).await.expect("get").is_queued());
        let exec = session.run(&at, "EXEC", &[]).await.expect("exec");
        let replies = exec.exec_replies().expect("EXEC answers an array");
        assert_eq!(replies.len(), 2);
        let text = replies.render(&args(&["INCR n", "GET k"]), ReplyFormat::Text);
        assert!(text.contains("INCR n") && text.contains('v'), "{text}");

        // A reply is rendered on demand, in any format, from the same value.
        let _ = session
            .run(&at, "HSET", &args(&[&format!("{key}:h"), "f", "1"]))
            .await
            .expect("hset");
        let hash = session
            .run(&at, "HGETALL", &args(&[&format!("{key}:h")]))
            .await
            .expect("hgetall");
        assert!(
            hash.render(ReplyFormat::Json).contains("\"f\": \"1\""),
            "pairs become an object"
        );
        assert!(hash.render(ReplyFormat::Table).contains("field"));
        assert!(
            session
                .run(&at, "GET", &args(&["zedis:it:no-such-key"]))
                .await
                .expect("get")
                .is_nil()
        );

        // A server error is an `Err` and does not cost the connection.
        let err = session
            .run(&at, "NOSUCHCOMMAND", &[])
            .await
            .expect_err("unknown command");
        assert!(!TerminalSession::drops_link(err.connection_kind()), "{err}");
        assert!(
            session.run(&at, "EXISTS", &args(&[&key])).await.is_ok(),
            "still on db 1"
        );

        // A fresh session starts over on the panel's db.
        let fresh = TerminalSession::default();
        let seen = fresh.run(&at, "EXISTS", &args(&[&key])).await.expect("exists");
        assert_eq!(
            seen.render(ReplyFormat::Text).trim(),
            "0",
            "a new session is back on db 0"
        );

        let _ = session
            .run(&at, "DEL", &args(&[&key, &format!("{key}:n"), &format!("{key}:h")]))
            .await
            .expect("cleanup");
    });
}

/// The stream editor's live tail: from `$`, so what was there before is never
/// returned; each round continues after the last id it saw; and a block that
/// times out is an empty batch, not an error.
#[test]
#[ignore]
fn standalone_stream_tail_returns_only_what_arrives_after_it_opened() {
    smol::block_on(async {
        let id = register(server("it-stream-tail", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("tail");
        let _: String = cmd("XADD")
            .arg(&key)
            .arg("*")
            .arg("before")
            .arg("1")
            .query_async(&mut c)
            .await
            .expect("xadd");

        let mut tail = StreamTail::open(&at, &key).await.expect("open the tail");
        let quiet = tail.next_batch(50, 10).await.expect("a timed-out block");
        assert!(quiet.is_empty(), "nothing arrived yet: {quiet:?}");

        // The entries are written while the tail is blocked, as in the app.
        let writer = smol::spawn({
            let key = key.clone();
            let mut c = c.clone();
            async move {
                smol::Timer::after(std::time::Duration::from_millis(100)).await;
                for n in 1..=2 {
                    let _: String = cmd("XADD")
                        .arg(&key)
                        .arg("*")
                        .arg("n")
                        .arg(n)
                        .arg("name")
                        .arg("zedis")
                        .query_async(&mut c)
                        .await
                        .expect("xadd");
                }
            }
        });
        let mut seen = Vec::new();
        for _ in 0..10 {
            seen.extend(tail.next_batch(500, 10).await.expect("next batch"));
            if seen.len() >= 2 {
                break;
            }
        }
        writer.await;
        let fields: Vec<_> = seen.iter().map(|(_, fields)| fields.clone()).collect();
        let pair = |n: &str| {
            vec![
                ("n".to_string(), n.to_string()),
                ("name".to_string(), "zedis".to_string()),
            ]
        };
        assert_eq!(
            fields,
            vec![pair("1"), pair("2")],
            "only the new entries, in order, each once"
        );
        assert!(seen.iter().all(|(id, _)| id.contains('-')), "{seen:?}");

        let again = tail.next_batch(50, 10).await.expect("caught up");
        assert!(again.is_empty(), "the cursor moved past what was returned: {again:?}");

        let missing = ServerDb::new("it-no-such-server", 0);
        assert!(StreamTail::open(&missing, &key).await.is_err());
        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");
    });
}

/// What the Pub/Sub and keyspace panels subscribe through: by pattern on the
/// classic transport, by exact name on the sharded one (Redis 7+), and either
/// way a message is the channel it was published to plus its bytes.
#[test]
#[ignore]
fn standalone_channel_subscription_delivers_both_kinds_of_pubsub() {
    smol::block_on(async {
        let id = register(server("it-channel-subscription", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        let mut c = conn(&id, 0).await;
        let channel = unique("sub");
        let pattern = format!("{channel}:*");

        let mut subscription = ChannelSubscription::open(&at, SubscribeKind::Patterns, &[pattern.as_str()])
            .await
            .expect("psubscribe");
        let receivers: i64 = cmd("PUBLISH")
            .arg(format!("{channel}:a"))
            .arg(b"\xffbytes".as_slice())
            .query_async(&mut c)
            .await
            .expect("publish");
        assert_eq!(receivers, 1, "the subscription is live once `open` returns");
        let _: i64 = cmd("PUBLISH")
            .arg(format!("{channel}:b"))
            .arg("second")
            .query_async(&mut c)
            .await
            .expect("publish");
        let first = subscription.next_message().await.expect("first message");
        assert_eq!(first.channel, format!("{channel}:a"), "the channel, not the pattern");
        assert_eq!(first.payload, b"\xffbytes", "the payload is handed over undecoded");
        let second = subscription.next_message().await.expect("second message");
        assert_eq!(
            (second.channel.as_str(), second.payload.as_slice()),
            (format!("{channel}:b").as_str(), b"second".as_slice())
        );

        // Dropping it is the unsubscribe.
        drop(subscription);
        let mut gone = false;
        for _ in 0..50 {
            let receivers: i64 = cmd("PUBLISH")
                .arg(format!("{channel}:a"))
                .arg("x")
                .query_async(&mut c)
                .await
                .expect("publish");
            if receivers == 0 {
                gone = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "a dropped subscription must close its connection");

        if !supports(&id, floors::SHARDED_PUBSUB).await {
            eprintln!("skipped the sharded half: SSUBSCRIBE needs Redis 7");
            return;
        }
        let mut sharded = ChannelSubscription::open(&at, SubscribeKind::Sharded, &[channel.as_str()])
            .await
            .expect("ssubscribe");
        // The push connection acknowledges asynchronously: publish until the
        // server counts the subscriber.
        let mut delivered = false;
        for _ in 0..50 {
            let receivers: i64 = cmd("SPUBLISH")
                .arg(&channel)
                .arg("shard")
                .query_async(&mut c)
                .await
                .expect("spublish");
            if receivers > 0 {
                delivered = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(20)).await;
        }
        assert!(delivered, "the sharded subscriber never showed up");
        let msg = sharded.next_message().await.expect("sharded message");
        assert_eq!(
            (msg.channel.as_str(), msg.payload.as_slice()),
            (channel.as_str(), b"shard".as_slice())
        );
    });
}

/// The Monitor panel's feeds: one per master, each naming its node, carrying
/// the server's own lines — and a server it cannot reach at all is the `Err`.
#[test]
#[ignore]
fn standalone_monitor_feed_carries_the_commands_the_server_receives() {
    smol::block_on(async {
        let (host, port) = standalone();
        let id = register(server("it-monitor-feed", (host.clone(), port))).await;
        let at = ServerDb::new(&*id, 0);
        let opened = open_monitor_feeds(&at).await.expect("feeds");
        assert!(opened.failures.is_empty(), "{:?}", opened.failures);
        let [mut feed] = <[_; 1]>::try_from(opened.feeds)
            .ok()
            .expect("a standalone has one master");
        assert_eq!(feed.node(), format!("{host}:{port}"));

        let marker = unique("monitored");
        let mut c = conn(&id, 0).await;
        let _: Option<String> = cmd("GET").arg(&marker).query_async(&mut c).await.expect("get");
        // The suite runs in parallel against this server, so the feed carries
        // everyone's commands; ours is in there.
        let mut found = None;
        for _ in 0..5_000 {
            let line = feed.next_line().await.expect("the feed stays open");
            if line.contains(&marker) {
                found = Some(line);
                break;
            }
        }
        let line = found.expect("the GET never appeared in the feed");
        assert!(line.to_ascii_uppercase().contains("\"GET\""), "{line}");

        let nowhere = ServerDb::new("it-no-such-server", 0);
        assert!(open_monitor_feeds(&nowhere).await.is_err());
    });
}

/// The operations the server form and the recycle bin got when their commands
/// left the view: a sentinel lists the masters it watches, Test means the
/// data node was reached, one CONFIG parameter is read by name, and RESTORE
/// puts a payload back with its TTL.
#[test]
#[ignore]
fn entry_checks_and_single_key_restore_work_through_their_operations() {
    smol::block_on(async {
        let id = register(server("it-entry-check", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        test_connection(&server("it-entry-check-probe", standalone()))
            .await
            .expect("a reachable standalone passes the test");
        let (host, _) = standalone();
        let err = test_connection(&server("it-entry-check-closed", (host, 1)))
            .await
            .expect_err("nothing listens on port 1");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Network, "{err}");

        assert_eq!(
            config_get_one(&at, "maxmemory-policy").await.expect("config get"),
            Some(maxmemory_policy(&at).await.expect("maxmemory policy"))
        );
        assert_eq!(
            config_get_one(&at, "zedis-no-such-parameter")
                .await
                .expect("config get"),
            None
        );

        let mut c = conn(&id, 0).await;
        let key = unique("restore");
        let _: () = cmd("RPUSH")
            .arg(&key)
            .arg("a")
            .arg("b")
            .query_async(&mut c)
            .await
            .expect("rpush");
        let payload: Vec<u8> = cmd("DUMP").arg(&key).query_async(&mut c).await.expect("dump");
        let raw = format!("{key}:raw");
        let _: () = cmd("SET")
            .arg(&raw)
            .arg(b"\xffraw".as_slice())
            .arg("EX")
            .arg(60)
            .query_async(&mut c)
            .await
            .expect("set");
        assert_eq!(
            key_bytes(&at, &raw).await.expect("key bytes"),
            b"\xffraw",
            "bytes, not text"
        );
        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("del");
        restore_key(&at, &key, 60_000, &payload).await.expect("restore");
        let items: Vec<String> = cmd("LRANGE")
            .arg(&key)
            .arg(0)
            .arg(-1)
            .query_async(&mut c)
            .await
            .expect("lrange");
        assert_eq!(items, ["a", "b"]);
        let pttl: i64 = cmd("PTTL").arg(&key).query_async(&mut c).await.expect("pttl");
        assert!((1..=60_000).contains(&pttl), "the TTL came back with the value: {pttl}");
        // No REPLACE: a key that is there again is not overwritten.
        assert!(restore_key(&at, &key, 0, &payload).await.is_err(), "BUSYKEY");
        assert!(value_preview(&at, &key).await.expect("preview").contains('a'));
        let _: () = cmd("DEL").arg(&key).query_async(&mut c).await.expect("cleanup");

        let Some(sentinel) = scenario("ZEDIS_IT_SENTINEL") else {
            eprintln!("skipped the sentinel half: ZEDIS_IT_SENTINEL not set");
            return;
        };
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        // The form has one password field when the button is pressed. Holding
        // the sentinel's own password, the listing is one dial …
        let form = RedisServer {
            password: env::var("ZEDIS_IT_SENTINEL_PASSWORD").ok().filter(|p| !p.is_empty()),
            ..server("it-entry-check-names", sentinel.clone())
        };
        let names = sentinel_master_names(&form).await.expect("sentinel masters");
        assert!(names.contains(&master_name), "{names:?}");
        // … and holding the data nodes' password — which a sentinel commonly
        // does not share — it is refused and retried without one. This
        // topology's sentinel has a password of its own, so the retry is
        // refused as well, and that is reported as what it is.
        if form.password.is_some() {
            let err = sentinel_master_names(&protected_server("it-entry-check-data-pw", sentinel.clone()))
                .await
                .expect_err("neither the data password nor none opens this sentinel");
            assert_eq!(err.connection_kind(), ConnectionErrorKind::Auth, "{err}");
        }
        test_connection(&sentinel_declared_server("it-entry-check-sentinel", sentinel))
            .await
            .expect("the test reaches the master the sentinel names");
    });
}

/// A runaway script makes the server answer BUSY to everything — the pooled
/// connection included — so the kill travels on a fresh connection that
/// sends only what a busy server still takes. With nothing running every
/// node says NOTBUSY, which is a result, not a failure. Runs on the `busy`
/// scenario's server, which nothing else uses.
#[test]
#[ignore]
fn busy_script_kill_stops_a_runaway_script() {
    smol::block_on(async {
        // Its own server: while the script runs, every command there gets
        // BUSY, which the other tests must never see.
        let addr = skip_unless!("ZEDIS_IT_BUSY");
        let id = register(server("it-busy", addr)).await;
        let entry = get_server(&id).expect("saved entry");
        let mut c = conn(&id, 0).await;

        let replies = kill_running(&entry, KillTarget::Script).await.expect("kill (idle)");
        assert!(
            replies.iter().all(|r| r.outcome == KillOutcome::NothingRunning),
            "{replies:?}"
        );

        // Answer BUSY after 100ms instead of 5s, then park a read-only
        // script on a dedicated connection.
        let previous: Vec<String> = cmd("CONFIG")
            .arg("GET")
            .arg("lua-time-limit")
            .query_async(&mut c)
            .await
            .expect("config get");
        cmd("CONFIG")
            .arg("SET")
            .arg("lua-time-limit")
            .arg("100")
            .exec_async(&mut c)
            .await
            .expect("config set");
        let mut runaway = open_single_connection(&entry, 0, false)
            .await
            .expect("dedicated connection");
        let script = smol::spawn(async move {
            cmd("EVAL")
                .arg("while true do end")
                .arg(0)
                .query_async::<redis::Value>(&mut runaway)
                .await
        });
        smol::Timer::after(std::time::Duration::from_millis(500)).await;

        let mut killed = false;
        for _ in 0..20 {
            let replies = kill_running(&entry, KillTarget::Script).await.expect("kill");
            if replies.iter().any(|r| r.outcome == KillOutcome::Killed) {
                killed = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
        }
        assert!(killed, "SCRIPT KILL never reached the busy server");
        let outcome = script.await;
        assert!(outcome.is_err(), "the script must have been stopped: {outcome:?}");

        // Back to what the server had; a plain command works again.
        cmd("CONFIG")
            .arg("SET")
            .arg("lua-time-limit")
            .arg(previous.get(1).cloned().unwrap_or_else(|| "5000".to_string()))
            .exec_async(&mut c)
            .await
            .expect("config restore");
        let pong: String = cmd("PING").query_async(&mut c).await.expect("ping after kill");
        assert_eq!(pong, "PONG");
    });
}

/// `CLIENT PAUSE` spelled per version (a mode word from 6.2), lifted by
/// `UNPAUSE` where it exists; a filtered `CLIENT KILL` composed the way
/// the panel composes it takes exactly the connection it names, and a
/// `MAXAGE` no client reaches takes none.
#[test]
#[ignore]
fn standalone_client_pause_and_filtered_kill() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let entry = get_server(&id).expect("saved entry");
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;

        // Through the operations the Clients panel calls.
        let at = ServerDb::new(&id, 0);
        let mode_supported = client.supports(floors::CLIENT_PAUSE_WRITE);
        // Without UNPAUSE (pre-6.2) the pause has to run out on its own —
        // keep it short so the other tests on this server barely notice.
        let ms = if mode_supported { 5000 } else { 50 };
        client_pause(&at, ms, PauseMode::Write, mode_supported)
            .await
            .expect("pause");
        if mode_supported {
            client_unpause(&at).await.expect("unpause");
        } else {
            smol::Timer::after(std::time::Duration::from_millis(100)).await;
        }
        let key = unique("pause");
        cmd("SET")
            .arg(&key)
            .arg("after")
            .exec_async(&mut c)
            .await
            .expect("a write after the pause");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");

        // A victim connection, found by its id in the listing, killed by ADDR.
        let mut victim = open_single_connection(&entry, 0, false)
            .await
            .expect("victim connection");
        let victim_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut victim)
            .await
            .expect("client id");
        let listing = client_list(&at).await.expect("client list");
        assert_eq!(listing.len(), 1, "a standalone is one node");
        let (node, clients) = &listing[0];
        assert_eq!((node.host.as_str(), node.port), (entry.host.as_str(), entry.port));
        let addr = clients
            .lines()
            .find(|line| line.split_whitespace().any(|f| f == format!("id={victim_id}")))
            .and_then(|line| line.split_whitespace().find_map(|f| f.strip_prefix("addr=")))
            .expect("the victim is listed with an address")
            .to_string();
        let filter = KillFilter {
            addr: Some(addr),
            skipme: true,
            ..Default::default()
        };
        let commands = kill_filter_commands(&filter);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            client_kill_by(&at, &commands).await.expect("kill by addr"),
            1,
            "exactly the victim"
        );
        let after: Result<String, redis::RedisError> = cmd("PING").query_async(&mut victim).await;
        assert!(after.is_err(), "the victim's connection is gone");

        // The row's own Kill button: by id, on the node that listed it.
        let mut second = open_single_connection(&entry, 0, false).await.expect("second victim");
        let second_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut second)
            .await
            .expect("client id");
        let second_id = second_id.to_string();
        assert!(client_kill_id(node, 0, &second_id).await.expect("kill by id"));
        let after: Result<String, redis::RedisError> = cmd("PING").query_async(&mut second).await;
        assert!(after.is_err(), "the second victim's connection is gone");
        // Already gone: the filter form of CLIENT KILL answers 0, not an error.
        assert!(
            !client_kill_id(node, 0, &second_id)
                .await
                .expect("kill a client that left")
        );

        if client.supports(floors::CLIENT_KILL_MAXAGE) {
            let filter = KillFilter {
                maxage_secs: Some(100_000_000),
                skipme: true,
                ..Default::default()
            };
            let killed = client_kill_by(&at, &kill_filter_commands(&filter))
                .await
                .expect("kill by maxage");
            assert_eq!(killed, 0, "no client is that old");
        }
    });
}

// ── tls ──────────────────────────────────────────────────────────────────

/// The channel browser: a channel is listed with its subscriber count
/// only while someone is subscribed to it, filtered by the glob, and the
/// sharded pair (`SHARDCHANNELS` / `SHARDNUMSUB`) sees shard subscriptions
/// — on every server that has sharded Pub/Sub at all.
#[test]
#[ignore]
fn standalone_pubsub_channels_lists_the_subscribed_ones() {
    smol::block_on(async {
        let id = register(server("it-pubsub-channels", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let channel = unique("pubsub");

        let none = client
            .pubsub_channels("zedis:it:pubsub:*", false)
            .await
            .expect("pubsub channels");
        assert!(
            !none.channels.iter().any(|c| c.name == channel),
            "nobody is subscribed yet: {none:?}"
        );

        let mut subscriber = get_connection_manager()
            .get_pubsub_connection(&id)
            .await
            .expect("pubsub connection");
        subscriber.subscribe(&channel).await.expect("subscribe");
        subscriber
            .psubscribe("zedis:it:pubsub:pattern:*")
            .await
            .expect("psubscribe");

        let listed = client
            .pubsub_channels("zedis:it:pubsub:*", false)
            .await
            .expect("pubsub channels");
        assert_eq!(listed.nodes, 1);
        assert_eq!(
            listed.channels.iter().find(|c| c.name == channel),
            Some(&PubsubChannel {
                name: channel.clone(),
                subscribers: 1
            }),
            "the subscribed channel is listed once with one subscriber: {listed:?}"
        );
        assert!(
            listed.pattern_subscriptions.expect("NUMPAT in classic mode") >= 1,
            "the pattern subscription counts: {listed:?}"
        );
        let other = client
            .pubsub_channels("zedis:it:elsewhere:*", false)
            .await
            .expect("pubsub channels");
        assert!(other.channels.is_empty(), "the glob filters: {other:?}");

        if supports(&id, floors::SHARDED_PUBSUB).await {
            let shard_channel = unique("spubsub");
            let mut shard_subscriber = get_connection_manager()
                .get_sharded_pubsub(&id)
                .await
                .expect("sharded pubsub");
            shard_subscriber
                .ssubscribe(&[shard_channel.as_str()])
                .await
                .expect("ssubscribe");
            let shards = client
                .pubsub_channels("zedis:it:spubsub:*", true)
                .await
                .expect("shard channels");
            assert_eq!(
                shards.channels,
                vec![PubsubChannel {
                    name: shard_channel,
                    subscribers: 1
                }],
                "the shard subscription is listed: {shards:?}"
            );
            assert!(shards.pattern_subscriptions.is_none(), "no NUMPAT in sharded mode");
            let classic = client
                .pubsub_channels("zedis:it:spubsub:*", false)
                .await
                .expect("pubsub channels");
            assert!(
                classic.channels.is_empty(),
                "a shard channel is not a classic one: {classic:?}"
            );
            drop(shard_subscriber);
        }
        drop(subscriber);
    });
}

// ── commandlog ───────────────────────────────────────────────────────────

/// `CONFIG GET name` → the value.
async fn config_value(c: &mut RedisAsyncConn, name: &str) -> String {
    let pair: Vec<String> = cmd("CONFIG")
        .arg("GET")
        .arg(name)
        .query_async(c)
        .await
        .expect("CONFIG GET");
    pair.get(1).cloned().expect("CONFIG GET answers name, value")
}

async fn set_config_raw(c: &mut RedisAsyncConn, name: &str, value: &str) {
    let _: String = cmd("CONFIG")
        .arg("SET")
        .arg(name)
        .arg(value)
        .query_async(c)
        .await
        .expect("CONFIG SET");
}

/// The slow log through the COMMANDLOG door on every server, and — on
/// Valkey 8.1+ — the two size logs: a request and a reply over lowered
/// thresholds land in their logs with their sizes, and a reset clears one
/// log without touching the other.
#[test]
#[ignore]
fn commandlog_lists_slow_and_oversized_commands() {
    smol::block_on(async {
        let id = register(server("it-commandlog", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;
        let key = unique("commandlog");
        let is_set_of_key = |entry: &zedis_connection::SlowLogEntry, command: &str| {
            entry.args.first().is_some_and(|a| a.eq_ignore_ascii_case(command)) && entry.args.get(1) == Some(&key)
        };

        // The slow log, through the same door, on every server: log every
        // command for a moment.
        let slower_than = config_value(&mut c, "slowlog-log-slower-than").await;
        set_config_raw(&mut c, "slowlog-log-slower-than", "0").await;
        let _: String = cmd("SET").arg(&key).arg("v").query_async(&mut c).await.expect("SET");
        let slow = client.get_command_logs(CommandLogKind::Slow).await.expect("slow log");
        set_config_raw(&mut c, "slowlog-log-slower-than", &slower_than).await;
        assert!(
            slow.iter().any(|e| is_set_of_key(e, "SET")),
            "the SET was logged: {slow:?}"
        );

        if !supports(&id, floors::COMMANDLOG).await {
            eprintln!("skipped the size logs: COMMANDLOG is Valkey 8.1+");
            return;
        }
        // Thresholds down to 1 KB, then one 2 KB request and one 2 KB reply.
        let request_threshold = config_value(&mut c, "commandlog-request-larger-than").await;
        let reply_threshold = config_value(&mut c, "commandlog-reply-larger-than").await;
        set_config_raw(&mut c, "commandlog-request-larger-than", "1024").await;
        set_config_raw(&mut c, "commandlog-reply-larger-than", "1024").await;
        let payload = "x".repeat(2048);
        let _: String = cmd("SET")
            .arg(&key)
            .arg(&payload)
            .query_async(&mut c)
            .await
            .expect("SET");
        let _: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("GET");

        let requests = client
            .get_command_logs(CommandLogKind::LargeRequest)
            .await
            .expect("large-request log");
        let hit = requests
            .iter()
            .find(|e| is_set_of_key(e, "SET"))
            .unwrap_or_else(|| panic!("the 2 KB SET is a large request: {requests:?}"));
        assert!(hit.amount >= 2048, "the entry carries the request size: {hit:?}");
        let replies = client
            .get_command_logs(CommandLogKind::LargeReply)
            .await
            .expect("large-reply log");
        let hit = replies
            .iter()
            .find(|e| is_set_of_key(e, "GET"))
            .unwrap_or_else(|| panic!("the 2 KB GET is a large reply: {replies:?}"));
        assert!(hit.amount >= 2048, "the entry carries the reply size: {hit:?}");

        client
            .commandlog_reset(CommandLogKind::LargeRequest)
            .await
            .expect("COMMANDLOG RESET");
        let requests = client
            .get_command_logs(CommandLogKind::LargeRequest)
            .await
            .expect("large-request log");
        assert!(requests.is_empty(), "reset cleared the large-request log: {requests:?}");
        let replies = client
            .get_command_logs(CommandLogKind::LargeReply)
            .await
            .expect("large-reply log");
        assert!(
            replies.iter().any(|e| is_set_of_key(e, "GET")),
            "the other log is untouched: {replies:?}"
        );

        set_config_raw(&mut c, "commandlog-request-larger-than", &request_threshold).await;
        set_config_raw(&mut c, "commandlog-reply-larger-than", &reply_threshold).await;
    });
}

// ── replication ──────────────────────────────────────────────────────────

/// `INFO replication` of `id` polled until `ok` holds — a role change or a
/// link coming up takes the pair a moment.
async fn wait_replication(id: &str, what: &str, ok: impl Fn(&ReplicationInfo) -> bool) -> ReplicationInfo {
    let mut last = ReplicationInfo::default();
    for _ in 0..80 {
        let client = get_connection_manager().get_client(id, 0).await.expect("client");
        last = client.replication_info().await.expect("INFO replication");
        if ok(&last) {
            return last;
        }
        smol::Timer::after(std::time::Duration::from_millis(250)).await;
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

#[test]
#[ignore]
fn tls_connects_with_the_root_cert_and_in_insecure_mode() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");

        let mut trusted = server("it-tls", addr.clone());
        trusted.tls = Some(true);
        trusted.root_cert = Some(ca);
        let id = register(trusted).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (root cert)");
        client.ping().await.expect("ping over tls");

        let mut insecure = server("it-tls-insecure", addr.clone());
        insecure.tls = Some(true);
        insecure.insecure = Some(true);
        let id = register(insecure).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (insecure)");
        client.ping().await.expect("ping over tls (insecure)");

        // Plaintext against a TLS-only port must fail, not hang.
        let id = register(server("it-tls-plain", addr)).await;
        assert!(get_connection_manager().get_client(&id, 0).await.is_err());
    });
}

/// Mutual TLS: the server verifies the client too (`tls-auth-clients yes`).
///
/// Three things the plain-TLS test cannot reach — the `client_cert` /
/// `client_key` pair actually being presented, a PKCS#8-encrypted key being
/// decrypted with `client_key_passphrase`, and a certificate-less client
/// being *refused* rather than quietly connecting. The last one is what
/// makes the first two mean anything.
#[test]
#[ignore]
fn mtls_requires_the_client_certificate() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_MTLS");
        let ca = env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA");
        let cert = env::var("ZEDIS_IT_TLS_CLIENT_CERT").expect("ZEDIS_IT_TLS_CLIENT_CERT");
        let key = env::var("ZEDIS_IT_TLS_CLIENT_KEY").expect("ZEDIS_IT_TLS_CLIENT_KEY");
        let encrypted_key = env::var("ZEDIS_IT_TLS_CLIENT_KEY_ENC").expect("ZEDIS_IT_TLS_CLIENT_KEY_ENC");
        let passphrase = env::var("ZEDIS_IT_TLS_CLIENT_KEY_PASSPHRASE").expect("ZEDIS_IT_TLS_CLIENT_KEY_PASSPHRASE");

        // Paths, not pasted PEM: `tls_material` takes either, and the file
        // form is the one the connection dialog writes.
        let mut client = server("it-mtls", addr.clone());
        client.tls = Some(true);
        client.root_cert = Some(ca.clone());
        client.client_cert = Some(cert.clone());
        client.client_key = Some(key);
        let id = register(client).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("mtls client")
            .ping()
            .await
            .expect("ping over mtls");

        // The same certificate with its key encrypted: only the passphrase
        // path can open it.
        let mut encrypted = server("it-mtls-encrypted-key", addr.clone());
        encrypted.tls = Some(true);
        encrypted.root_cert = Some(ca.clone());
        encrypted.client_cert = Some(cert);
        encrypted.client_key = Some(encrypted_key.clone());
        encrypted.client_key_passphrase = Some(passphrase);
        let id = register(encrypted).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("mtls client (encrypted key)")
            .ping()
            .await
            .expect("ping over mtls with an encrypted key");

        // …and without it the key is unreadable, so the connection never
        // even gets to the handshake.
        let mut no_passphrase = server("it-mtls-no-passphrase", addr.clone());
        no_passphrase.tls = Some(true);
        no_passphrase.root_cert = Some(ca.clone());
        no_passphrase.client_key = Some(encrypted_key);
        let id = register(no_passphrase).await;
        assert!(get_connection_manager().get_client(&id, 0).await.is_err());

        // Trusting the server is not enough when the server also wants to
        // trust you.
        let mut anonymous = server("it-mtls-anonymous", addr);
        anonymous.tls = Some(true);
        anonymous.root_cert = Some(ca);
        let id = register(anonymous).await;
        let refused = match get_connection_manager().get_client(&id, 0).await {
            Err(e) => Err(e),
            // Some TLS stacks only surface the peer's alert on first use,
            // so a handshake that "succeeded" still has to fail here.
            Ok(client) => client.ping().await,
        };
        assert!(refused.is_err(), "a client without a certificate was accepted");
    });
}

/// The SSH tunnel, against a real sshd: an encrypted key that needs its
/// passphrase, a plain key, and a login that must be refused.
///
/// **The order inside this test matters.** SSH sessions are cached globally
/// by `user@addr` ([`SshTarget::cache_id`]), so the second entry with the
/// same user and host reuses the first one's session instead of
/// authenticating again. The cases that have to *authenticate* therefore run
/// before any session exists, and the one negative case that must not be
/// answered from the cache uses a different user.
#[test]
#[ignore]
fn ssh_tunnel_carries_the_connection_to_the_standalone_server() {
    smol::block_on(async {
        let (ssh_host, ssh_port) = skip_unless!("ZEDIS_IT_SSH");
        let user = env::var("ZEDIS_IT_SSH_USER").expect("ZEDIS_IT_SSH_USER");
        let key = env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY");
        let encrypted_key = env::var("ZEDIS_IT_SSH_KEY_ENC").expect("ZEDIS_IT_SSH_KEY_ENC");
        let passphrase = env::var("ZEDIS_IT_SSH_KEY_PASSPHRASE").expect("ZEDIS_IT_SSH_KEY_PASSPHRASE");
        let ssh_addr = format!("{ssh_host}:{ssh_port}");

        let tunnelled = |id: &str| {
            let mut s = server(id, standalone());
            s.ssh_tunnel = Some(true);
            s.ssh_addr = Some(ssh_addr.clone());
            s.ssh_username = Some(user.clone());
            s
        };

        // 1. An encrypted key with no passphrase cannot be read at all —
        //    and this runs first, so no cached session can mask it.
        let mut no_passphrase = tunnelled("it-ssh-no-passphrase");
        no_passphrase.ssh_key = Some(encrypted_key.clone());
        let id = register(no_passphrase).await;
        assert!(
            get_connection_manager().get_client(&id, 0).await.is_err(),
            "an encrypted key was accepted without its passphrase"
        );

        // 2. The same key with the passphrase: a real authentication, since
        //    nothing has succeeded yet.
        let mut encrypted = tunnelled("it-ssh-encrypted-key");
        encrypted.ssh_key = Some(encrypted_key);
        encrypted.ssh_key_passphrase = Some(passphrase);
        let id = register(encrypted).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client (encrypted key)")
            .ping()
            .await
            .expect("ping through the tunnel");

        // 3. A plain key, and a round trip that proves the forwarded stream
        //    really reaches the standalone server: the value is written
        //    through the tunnel and read back on a direct connection.
        let mut plain = tunnelled("it-ssh");
        plain.ssh_key = Some(key);
        let id = register(plain).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client");
        assert_eq!(client.nodes_description().server_type, "Standalone");
        let name = unique("ssh");
        let mut tunnel_conn = conn(&id, 0).await;
        cmd("SET")
            .arg(&name)
            .arg("through-the-tunnel")
            .exec_async(&mut tunnel_conn)
            .await
            .expect("set through the tunnel");

        let direct = register(server("it-ssh-direct", standalone())).await;
        let mut direct_conn = conn(&direct, 0).await;
        let value: String = cmd("GET")
            .arg(&name)
            .query_async(&mut direct_conn)
            .await
            .expect("get directly");
        assert_eq!(value, "through-the-tunnel", "the tunnel reached the real server");
        cmd("DEL")
            .arg(&name)
            .exec_async(&mut direct_conn)
            .await
            .expect("cleanup");

        // 4. A login the sshd cannot grant. Its own `user@addr`, so the
        //    successful session above is not reused for it.
        let mut refused = tunnelled("it-ssh-wrong-user");
        refused.ssh_username = Some("zedis-it-no-such-user".to_string());
        refused.ssh_key = Some(env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY"));
        let id = register(refused).await;
        assert!(
            get_connection_manager().get_client(&id, 0).await.is_err(),
            "an unknown ssh user was let in"
        );

        // 5. An RSA key. It reaches the same sshd on its own port, so this is
        //    a real handshake and not the cached session from case 2 — the
        //    point of the case, because RSA is the one algorithm whose
        //    signature is negotiated: signed as the legacy ssh-rsa (SHA-1) it
        //    is refused by every OpenSSH since 8.8, and the fixture's sshd
        //    pins SHA-2 so an older host cannot let it pass either.
        let rsa_addr = env::var("ZEDIS_IT_SSH_RSA").expect("ZEDIS_IT_SSH_RSA");
        let rsa_key = env::var("ZEDIS_IT_SSH_KEY_RSA").expect("ZEDIS_IT_SSH_KEY_RSA");
        let mut rsa = server("it-ssh-rsa", standalone());
        rsa.ssh_tunnel = Some(true);
        rsa.ssh_addr = Some(rsa_addr);
        rsa.ssh_username = Some(user.clone());
        rsa.ssh_key = Some(rsa_key);
        let id = register(rsa).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client (rsa key)")
            .ping()
            .await
            .expect("ping through the rsa-authenticated tunnel");
    });
}

/// The heartbeat's probe: with one master the `INFO` is the probe, on the
/// client's own connection, so a beat is one command; a cluster still probes
/// with `PING` and leaves the per-master `INFO` to the fan-out.
#[test]
#[ignore]
fn heartbeat_probe_is_the_info_itself_where_there_is_one_master() {
    smol::block_on(async {
        let id = register(server("it-heartbeat-standalone", standalone())).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("standalone client");
        let info = client
            .heartbeat_probe()
            .await
            .expect("probe")
            .expect("one master: the probe carries the INFO");
        assert!(info.contains("redis_version:"), "not an INFO reply: {info:.60}");
        // The same text the fan-out would have fetched, so the parser behind
        // the status bar is fed what it always was.
        let (_, fanned): (_, Vec<String>) = client.query_async_masters(vec![cmd("INFO")]).await.expect("fan-out");
        assert_eq!(fanned.len(), 1);
        let section = |text: &str| {
            text.lines()
                .filter(|l| l.starts_with('#'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(section(&info), section(&fanned[0]), "the two INFOs differ in shape");

        // A Sentinel entry's client is the data master: one master as well.
        if let Some(addr) = scenario("ZEDIS_IT_SENTINEL") {
            let id = register(sentinel_server("it-heartbeat-sentinel", addr)).await;
            let client = get_connection_manager()
                .get_client(&id, 0)
                .await
                .expect("sentinel client");
            let info = client
                .heartbeat_probe()
                .await
                .expect("probe")
                .expect("the master's INFO");
            assert!(info.contains("role:master"), "the probe did not reach the master");
        }

        let Some(addr) = scenario("ZEDIS_IT_CLUSTER") else {
            eprintln!("skipped the cluster half: ZEDIS_IT_CLUSTER not set");
            return;
        };
        let id = register(protected_server("it-heartbeat-cluster", addr)).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("cluster client");
        assert!(
            client.heartbeat_probe().await.expect("probe").is_none(),
            "a cluster's bare INFO answers for one arbitrary node; the probe has to stay a PING"
        );
    });
}

/// TLS *inside* the tunnel: the forwarded stream carries the handshake, so
/// the certificate is checked against the endpoint the sshd dials, not the
/// sshd. Its own test (and the RSA port, so its own `user@addr`) because
/// neither the `tls` nor the `ssh` test ever combined the two.
#[test]
#[ignore]
fn ssh_tunnel_carries_a_tls_connection() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let Ok(ssh_addr) = env::var("ZEDIS_IT_SSH_RSA") else {
            eprintln!("skipped: ZEDIS_IT_SSH_RSA not set");
            return;
        };
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");

        let mut tunnelled = server("it-ssh-tls", addr);
        tunnelled.tls = Some(true);
        tunnelled.root_cert = Some(ca);
        tunnelled.ssh_tunnel = Some(true);
        tunnelled.ssh_addr = Some(ssh_addr);
        tunnelled.ssh_username = Some(env::var("ZEDIS_IT_SSH_USER").expect("ZEDIS_IT_SSH_USER"));
        tunnelled.ssh_key = Some(env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY"));
        let mut unreachable = tunnelled.clone();
        let id = register(tunnelled).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client through the tunnel");
        client.ping().await.expect("ping over tls through the tunnel");

        // An endpoint the sshd cannot reach: the session is fine and the
        // forward is refused, so the error has to say *which* address the
        // SSH server was asked for — `ConnectFailed` alone names nothing. A
        // port that was just bound and released is closed.
        let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        unreachable.id = "it-ssh-tls-unreachable".to_string();
        unreachable.name = unreachable.id.clone();
        unreachable.port = closed_port;
        let id = register(unreachable).await;
        let Err(e) = get_connection_manager().get_client(&id, 0).await else {
            panic!("a closed port answered through the tunnel");
        };
        let message = e.to_string();
        assert!(
            message.contains(&format!("127.0.0.1:{closed_port}")),
            "the refused forward does not name its destination: {message}"
        );
    });
}

// ── sentinel ─────────────────────────────────────────────────────────────

/// `SENTINEL GET-MASTER-ADDR-BY-NAME` straight from the sentinel.
async fn sentinel_master_port(sentinel: &mut RedisAsyncConn, master_name: &str) -> u16 {
    let (_, port): (String, String) = cmd("SENTINEL")
        .arg("GET-MASTER-ADDR-BY-NAME")
        .arg(master_name)
        .query_async(sentinel)
        .await
        .expect("get-master-addr-by-name");
    port.parse().expect("master port")
}

#[test]
#[ignore]
fn sentinel_resolves_the_master_and_follows_a_failover() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        // A direct dial of the sentinel: `sentinel_login()` swaps in the
        // credentials the sentinel itself wants, which are not the data
        // nodes'.
        let raw = sentinel_server("it-sentinel-raw", addr.clone()).sentinel_login();
        let mut sentinel = open_single_connection(&raw, 0, false)
            .await
            .expect("sentinel connection");
        let before = sentinel_master_port(&mut sentinel, &master_name).await;

        // Seeds are walked in order: a dead first address must not stop
        // discovery — the real sentinel is the second entry.
        let (sentinel_host, sentinel_port) = addr.clone();
        let mut s = sentinel_server(
            "it-sentinel",
            (format!("127.0.0.1:1, {sentinel_host}:{sentinel_port}"), sentinel_port),
        );
        s.master_name = Some(master_name.clone());
        assert_eq!(s.seed_endpoints().len(), 2);
        let id = register(s).await;
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("sentinel client");
        let masters = client.master_servers();
        assert_eq!(masters.len(), 1, "one monitored master");
        assert_eq!(masters[0].port, before, "the master the sentinel announces");
        assert_eq!(client.nodes_description().server_type, "Sentinel");

        let key = unique("sentinel");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("via-sentinel")
            .exec_async(&mut c)
            .await
            .expect("set on master");
        let value: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        assert_eq!(value, "via-sentinel");

        // Force a failover and wait for the sentinel to promote the replica.
        // Right after the topology starts the sentinel may not have rated
        // the replica yet (`NOGOODSLAVE`): retry for a while.
        let mut started = false;
        for _ in 0..60 {
            let reply: Result<String, redis::RedisError> = cmd("SENTINEL")
                .arg("FAILOVER")
                .arg(&master_name)
                .query_async(&mut sentinel)
                .await;
            match reply {
                Ok(_) => {
                    started = true;
                    break;
                }
                Err(e) if e.to_string().contains("NOGOODSLAVE") => {
                    smol::Timer::after(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("sentinel failover: {e}"),
            }
        }
        assert!(started, "sentinel never accepted the failover");
        let mut after = before;
        for _ in 0..60 {
            smol::Timer::after(std::time::Duration::from_millis(500)).await;
            after = sentinel_master_port(&mut sentinel, &master_name).await;
            if after != before {
                break;
            }
        }
        assert_ne!(after, before, "sentinel never promoted the replica");
        // The sentinel announces the new master before it has reconfigured
        // the old one; wait until the demoted node itself reports `slave`
        // (replica-read-only is the default, so writes bounce from then on).
        // Sentinel kills the demoted node's clients as part of the
        // reconfiguration, so poll on a fresh connection each time.
        // A data node, so the data password — not the sentinel's.
        let demoted_server = protected_server("it-demoted", ("127.0.0.1".into(), before));
        let mut is_replica = false;
        for _ in 0..60 {
            if let Ok(mut demoted) = open_single_connection(&demoted_server, 0, false).await
                && let Ok(info) = cmd("INFO").arg("replication").query_async::<String>(&mut demoted).await
                && info.contains("role:slave")
            {
                is_replica = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(500)).await;
        }
        assert!(is_replica, "the old master was never reconfigured as a replica");

        // The cached client still talks to the demoted node: a write now
        // bounces with READONLY — the signal `note_link_error` acts on.
        let write: Result<(), redis::RedisError> = cmd("SET").arg(&key).arg("after-failover").exec_async(&mut c).await;
        match write {
            Ok(()) => panic!("write on the demoted master should have been refused"),
            Err(e) => {
                let kind = zedis_connection::error::Error::from(e).connection_kind();
                assert!(
                    matches!(kind, ConnectionErrorKind::ReadOnly | ConnectionErrorKind::Network),
                    "{kind:?}"
                );
            }
        }
        // …which is to drop the client; discovery then lands on the new master.
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("re-resolved client");
        assert_eq!(client.master_servers()[0].port, after, "resolved the promoted master");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("after-failover")
            .exec_async(&mut c)
            .await
            .expect("write on the new master");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// The admin commands go to the sentinels, never to the pooled data master
/// (where `SENTINEL` is unknown): MASTERS lists both monitored masters,
/// CKQUORUM answers per sentinel, SET changes what MASTERS reports next,
/// MONITOR / REMOVE add and drop a master, FLUSHCONFIG rewrites the file.
#[test]
#[ignore]
fn sentinel_admin_commands_reach_the_sentinels() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let mut s = sentinel_server("it-sentinel-admin", addr);
        s.master_name = Some(master_name.clone());
        let id = register(s).await;
        let server = get_server(&id).expect("saved entry");

        let masters = sentinel_masters(&server).await.expect("masters");
        assert!(masters.len() >= 2, "two monitored masters: {masters:?}");
        let before = masters
            .iter()
            .find(|m| m.name == master_name)
            .cloned()
            .expect("the named master is listed");
        assert_eq!(before.quorum, 1);

        // One sentinel with quorum 1: from where it stands, a failover is possible.
        let replies = sentinel_ckquorum(&server, &master_name).await.expect("ckquorum");
        assert_eq!(replies.len(), 1, "one sentinel in the topology: {replies:?}");
        let reply = replies[0].result.as_ref().expect("ckquorum answers");
        assert!(reply.starts_with("OK"), "{reply}");

        // SET is visible in the next MASTERS; put the value back afterwards.
        let option = "down-after-milliseconds".to_string();
        let replies = sentinel_set(&server, &master_name, &[(option.clone(), "6000".to_string())])
            .await
            .expect("set");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        let after = sentinel_masters(&server)
            .await
            .expect("masters")
            .into_iter()
            .find(|m| m.name == master_name)
            .expect("still listed");
        assert_eq!(after.down_after_ms, 6000);
        sentinel_set(&server, &master_name, &[(option, before.down_after_ms.to_string())])
            .await
            .expect("set back");

        // MONITOR a throwaway master (an address nothing answers on is fine:
        // the command only records it), then REMOVE it again.
        let tmp = unique("snt").replace(':', "-");
        let replies = sentinel_monitor(&server, &tmp, "127.0.0.1", 1, 1)
            .await
            .expect("monitor");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        assert!(
            sentinel_masters(&server)
                .await
                .expect("masters")
                .iter()
                .any(|m| m.name == tmp)
        );
        let replies = sentinel_remove(&server, &tmp).await.expect("remove");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        assert!(
            !sentinel_masters(&server)
                .await
                .expect("masters")
                .iter()
                .any(|m| m.name == tmp)
        );

        let replies = sentinel_flushconfig(&server).await.expect("flushconfig");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
    });
}

/// An entry that names no master on a sentinel with several connects to
/// the first by name and carries the whole list for the Topology switcher;
/// naming one the sentinel does not monitor fails and says what it does.
/// The sentinel's own credentials, which the harness deliberately makes
/// different from the data nodes'. Three shapes, all of them real:
///
/// * type declared as Sentinel → `open_seed_endpoint` takes
///   `sentinel_login()` straight to the seed;
/// * type on auto (what the connection dialog writes by default) → the
///   data password is refused by the sentinel and the retry finds the
///   sentinel's;
/// * no sentinel credentials at all → nothing to retry with, and the
///   failure has to arrive as an auth error rather than a hang or a
///   "seed unreachable".
#[test]
#[ignore]
fn sentinel_uses_its_own_credentials_for_the_sentinels() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let Some(sentinel_password) = env::var("ZEDIS_IT_SENTINEL_PASSWORD").ok().filter(|p| !p.is_empty()) else {
            eprintln!("skipped: ZEDIS_IT_SENTINEL_PASSWORD not set");
            return;
        };
        assert_ne!(
            Some(&sentinel_password),
            data_password().as_ref(),
            "the harness must give the sentinel a different password, or this proves nothing"
        );

        for mut entry in [
            sentinel_declared_server("it-sentinel-auth-declared", addr.clone()),
            sentinel_server("it-sentinel-auth-auto", addr.clone()),
        ] {
            entry.master_name = Some(master_name.clone());
            assert!(entry.has_sentinel_credentials());
            let id = register(entry).await;
            get_connection_manager().remove_client(&id, 0);
            let client = get_connection_manager()
                .get_client(&id, 0)
                .await
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(client.nodes_description().server_type, "Sentinel");
            // The data password is the one that has to reach the master.
            client.ping().await.unwrap_or_else(|e| panic!("{id} ping: {e}"));
        }

        // Only the data credentials: the sentinel refuses them, and the
        // legacy "retry without a password" fallback cannot help against a
        // sentinel that wants one.
        let mut blind = server("it-sentinel-auth-missing", addr);
        blind.master_name = Some(master_name);
        blind.password = data_password();
        assert!(!blind.has_sentinel_credentials());
        let id = register(blind).await;
        get_connection_manager().remove_client(&id, 0);
        let err = get_connection_manager()
            .get_client(&id, 0)
            .await
            .err()
            .expect("a sentinel password is required here");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Auth, "{err}");
    });
}

#[test]
#[ignore]
fn sentinel_without_a_master_name_takes_the_first_and_lists_all() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let Ok(second) = env::var("ZEDIS_IT_MASTER_NAME2") else {
            eprintln!("skipped: ZEDIS_IT_MASTER_NAME2 not set");
            return;
        };
        let id = register(sentinel_server("it-sentinel-unnamed", addr.clone())).await;
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let desc = client.nodes_description();
        assert_eq!(desc.server_type, "Sentinel");
        let mut expected = vec![master_name, second];
        expected.sort();
        // `sentinel_admin_commands_reach_the_sentinels` MONITORs a throwaway
        // `zedis-it-…` master for a moment; the suite runs in parallel, so it
        // may be listed here too — it sorts after the real ones either way.
        let listed: Vec<String> = desc
            .sentinel_master_names
            .iter()
            .filter(|name| !name.starts_with("zedis-it-"))
            .cloned()
            .collect();
        assert_eq!(listed, expected);
        assert_eq!(client.master_servers().len(), 1, "connected to one master");
        assert_eq!(desc.topology[0].master.master_name, expected[0], "the first by name");

        let mut s = sentinel_server("it-sentinel-unknown", addr);
        s.master_name = Some("no-such-master".into());
        let id = register(s).await;
        get_connection_manager().remove_client(&id, 0);
        let err = get_connection_manager()
            .get_client(&id, 0)
            .await
            .err()
            .expect("an unmonitored master name must fail the connect");
        assert!(err.to_string().contains("no-such-master"), "{err}");
    });
}

// ── cluster ──────────────────────────────────────────────────────────────

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
        let total = client.dbsize().await.expect("dbsize");
        assert!(total >= keys.len() as u64, "dbsize sums the masters: {total}");
        // The routed DBSIZE (redis-rs fans it out and sums) must agree with
        // asking each master ourselves — the count the status bar shows
        // against the scanned keys. The other cluster tests run in parallel
        // and write keys between the two snapshots, so on a mismatch take
        // both again rather than fail on a one-key race.
        let mut agreed = false;
        for _ in 0..10 {
            let routed = client.dbsize().await.expect("dbsize");
            let (_, per_master): (_, Vec<u64>) = client
                .query_async_masters(vec![cmd("DBSIZE")])
                .await
                .expect("per-master dbsize");
            if routed == per_master.iter().sum::<u64>() {
                agreed = true;
                break;
            }
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

/// `HOTKEYS` (8.6): the full lifecycle — start with both metrics, hammer a
/// key, read the live report, stop (report stays), reset (report gone).
#[test]
#[ignore]
fn standalone_hotkeys_collects_a_report() {
    smol::block_on(async {
        let id = register(server("it-hotkeys", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        // A Valkey 9.x version number clears the 8.6 floor, but HOTKEYS is
        // a Redis-only command — the floor table knows.
        if !client.supports(floors::HOTKEYS) {
            eprintln!("skipped: HOTKEYS is Redis 8.6+ only");
            return;
        }
        // A stale collection from an earlier run: stop (idempotent) + reset.
        client.hotkeys_stop().await.expect("stop stale");
        client.hotkeys_reset().await.expect("reset stale");
        let empty = client.hotkeys_report().await.expect("empty report");
        assert!(empty.is_empty() && !empty.tracking_active);

        client.hotkeys_start(true, true, 10).await.expect("start");
        let key = unique("hot");
        let mut c = conn(&id, 0).await;
        cmd("SET").arg(&key).arg("v").exec_async(&mut c).await.expect("set");
        for _ in 0..40 {
            let _: Option<String> = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        }
        assert!(
            client.hotkeys_report().await.expect("live report").tracking_active,
            "tracking shows active while collecting"
        );

        client.hotkeys_stop().await.expect("stop");
        let report = client.hotkeys_report().await.expect("stopped report");
        assert!(!report.tracking_active);
        assert!(
            report.by_cpu.iter().any(|e| e.key == key),
            "the hammered key ranks by CPU time: {:?}",
            report.by_cpu
        );
        assert!(
            report.by_net.iter().any(|e| e.key == key),
            "…and by network bytes: {:?}",
            report.by_net
        );
        assert!(report.total_cpu_us > 0 && report.total_net_bytes > 0);
        assert!(
            report.by_cpu.windows(2).all(|w| w[0].value >= w[1].value),
            "merged list is descending"
        );

        client.hotkeys_reset().await.expect("reset");
        assert!(client.hotkeys_report().await.expect("cleared").is_empty());
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// ACL v2 selectors (7.0+): a `( … )` group survives the whole round trip —
/// tokenized as one SETUSER argument, parsed back out of GETUSER, and
/// re-emitted verbatim by `to_rules_text`, which must itself re-apply
/// cleanly (the editor's save path).
/// The rest of the ACL page: `ACL LOG` records what the server refused,
/// `ACL DRYRUN` answers the same question before it happens, `ACL GENPASS`
/// makes a password, and `ACL SAVE` is refused on a server without an
/// `aclfile` — which is exactly why the page hides that button there.
#[test]
#[ignore]
fn acl_log_dryrun_genpass_and_the_aclfile_gate() {
    smol::block_on(async {
        let admin_id = register(server("it-standalone", standalone())).await;
        let at_admin = ServerDb::new(&admin_id, 0);
        if acl_whoami(&at_admin).await.expect("whoami").is_empty() {
            eprintln!("skipped: server has no ACL (Redis < 6)");
            return;
        }

        // A user who may connect and PING, and nothing else. `+select` by
        // name: it only joined `@connection` in 7.0.
        let suffix = unique("acllog").rsplit(':').take(3).collect::<Vec<_>>().join("_");
        let username = format!("zedis_it_log_{suffix}");
        acl_set_user(
            &at_admin,
            &username,
            &split_acl_rules("on >pw ~* &* -@all +@connection +select"),
        )
        .await
        .expect("setuser");

        // Trip one denial on a connection of that user's own.
        let mut user_server = server(&format!("it-acllog-{suffix}"), standalone());
        user_server.username = Some(username.clone());
        user_server.password = Some("pw".into());
        let mut denied = open_single_connection(&user_server, 0, false)
            .await
            .expect("connect as the restricted user");
        let refused: Result<String, _> = cmd("GET").arg("zedis:it:nope").query_async(&mut denied).await;
        assert!(refused.is_err(), "the restricted user must not be able to GET");

        let entries = acl_log(&at_admin, 128).await.expect("acl log");
        let hit = entries
            .iter()
            .find(|entry| entry.username == username)
            .unwrap_or_else(|| panic!("the denial was logged: {entries:?}"));
        assert_eq!(hit.reason, "command");
        assert_eq!(hit.object.to_ascii_lowercase(), "get");
        assert!(hit.count >= 1);
        assert!(!hit.client_info.is_empty(), "the entry names the connection: {hit:?}");

        // `ACL DRYRUN` answers the same question without running anything.
        if supports(&admin_id, floors::ACL_V2).await {
            assert_eq!(
                acl_dryrun(&at_admin, &username, &["ping".to_string()])
                    .await
                    .expect("dryrun ping"),
                AclDryRun::Allowed
            );
            match acl_dryrun(&at_admin, &username, &["get".to_string(), "zedis:it:nope".to_string()])
                .await
                .expect("dryrun get")
            {
                AclDryRun::Denied(reason) => {
                    assert!(reason.to_lowercase().contains("get"), "the reason names it: {reason}");
                }
                AclDryRun::Allowed => panic!("GET must be denied for {username}"),
            }
        } else {
            eprintln!("skipped ACL DRYRUN: it is Redis 7.0+");
        }

        let password = acl_genpass(&at_admin, None).await.expect("genpass");
        assert_eq!(password.len(), 64, "256 bits, hex encoded: {password}");
        assert!(password.chars().all(|c| c.is_ascii_hexdigit()), "{password}");
        assert_eq!(
            acl_genpass(&at_admin, Some(64)).await.expect("genpass 64").len(),
            16,
            "the bit count decides the length"
        );

        // No aclfile here, so the page offers neither Save nor Load — and
        // the command itself says why.
        assert!(acl_file(&at_admin).await.expect("config get aclfile").is_none());
        assert!(acl_save(&at_admin).await.is_err(), "ACL SAVE needs an aclfile");

        acl_log_reset(&at_admin).await.expect("acl log reset");
        assert!(
            !acl_log(&at_admin, 128)
                .await
                .expect("acl log")
                .iter()
                .any(|entry| entry.username == username),
            "RESET cleared the log"
        );
        acl_del_user(&at_admin, &username).await.expect("deluser");
    });
}

#[test]
#[ignore]
fn standalone_acl_selectors_round_trip() {
    smol::block_on(async {
        let id = register(server("it-acl-sel", standalone())).await;
        if !supports(&id, floors::ACL_V2).await {
            eprintln!("skipped: server predates ACL v2");
            return;
        }
        let username = unique("selector-user").replace(':', "-");
        let at = ServerDb::new(&id, 0);
        let rules = split_acl_rules("on ~app:* +@read (-@all +lpush ~queue:*)");
        assert_eq!(rules.len(), 4, "the selector group must stay one argument");
        acl_set_user(&at, &username, &rules).await.expect("setuser");

        let user = acl_get_user(&at, &username).await.expect("getuser");
        assert_eq!(user.selectors.len(), 1, "the selector is parsed, not dropped");
        assert!(
            user.selectors[0].commands.contains("+lpush"),
            "selector commands: {:?}",
            user.selectors[0]
        );
        assert_eq!(user.selectors[0].keys, vec!["~queue:*".to_string()]);
        let text = user.to_rules_text();
        assert!(text.contains("(") && text.contains("~queue:*"), "rules text: {text}");

        // The editor round trip: what we display must re-apply as-is.
        acl_set_user(&at, &username, &split_acl_rules(&text))
            .await
            .expect("re-apply rules text");
        let again = acl_get_user(&at, &username).await.expect("getuser again");
        assert_eq!(again.selectors, user.selectors, "re-applying is lossless");

        acl_del_user(&at, &username).await.expect("deluser");
    });
}

/// `SET … IFEQ` (Redis 8.4+ / Valkey 8.1+): the version gate reports
/// support, and the guard's wire semantics hold — a matching baseline
/// writes, a stale one answers nil and leaves the value alone. This is
/// what makes the string editor's save a compare-and-set.
#[test]
#[ignore]
fn standalone_set_ifeq_guards_concurrent_writes() {
    smol::block_on(async {
        let id = register(server("it-cas", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        if !client.supports_set_ifeq() {
            eprintln!("skipped: server predates SET IFEQ");
            return;
        }
        let key = unique("cas");
        let mut c = conn(&id, 0).await;
        cmd("SET").arg(&key).arg("v1").exec_async(&mut c).await.expect("seed");
        let hit: redis::Value = cmd("SET")
            .arg(&key)
            .arg("v2")
            .arg("KEEPTTL")
            .arg("IFEQ")
            .arg("v1")
            .query_async(&mut c)
            .await
            .expect("cas hit");
        assert!(!matches!(hit, redis::Value::Nil), "matching baseline writes");
        let refused: redis::Value = cmd("SET")
            .arg(&key)
            .arg("v3")
            .arg("KEEPTTL")
            .arg("IFEQ")
            .arg("v1")
            .query_async(&mut c)
            .await
            .expect("cas miss");
        assert!(matches!(refused, redis::Value::Nil), "stale baseline must be refused");
        let now: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        assert_eq!(now, "v2", "the refused write left the value alone");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// `EXPIRE … NX | XX | GT | LT` (7.0): the batch helper reports, per key,
/// whether the server honoured the condition — a key without a TTL counts
/// as infinite, so GT skips it and LT catches it.
#[test]
#[ignore]
fn standalone_batch_ttl_conditions_report_skipped_keys() {
    smol::block_on(async {
        let id = register(server("it-ttl-cond", standalone())).await;
        check_batch_ttl_conditions(&id).await;
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

async fn check_batch_ttl_conditions(id: &str) {
    let client = get_connection_manager().get_client(id, 0).await.expect("client");
    if !client.supports(floors::EXPIRE_CONDITIONS) {
        eprintln!("skipped: server predates EXPIRE conditions");
        return;
    }
    let mut c = conn(id, 0).await;
    let volatile = unique("ttl:volatile");
    let permanent = unique("ttl:permanent");
    cmd("SET")
        .arg(&volatile)
        .arg("v")
        .arg("EX")
        .arg(1000)
        .exec_async(&mut c)
        .await
        .expect("set volatile");
    cmd("SET")
        .arg(&permanent)
        .arg("v")
        .exec_async(&mut c)
        .await
        .expect("set permanent");
    let keys = vec![volatile.clone(), permanent.clone()];

    // GT: 500 is sooner than the volatile key's 1000, and a permanent key
    // is never "extended".
    let applied = client
        .set_ttl_keys_scattered(keys.clone(), Some(500), Some(ExpireCondition::Gt))
        .await
        .expect("gt");
    assert_eq!(applied, [false, false], "GT touches neither key");
    // NX: only the permanent key gains a TTL.
    let applied = client
        .set_ttl_keys_scattered(keys.clone(), Some(500), Some(ExpireCondition::Nx))
        .await
        .expect("nx");
    assert_eq!(applied, [false, true], "NX only sets a TTL where there is none");
    // LT: 200 is sooner than both 1000 and 500.
    let applied = client
        .set_ttl_keys_scattered(keys.clone(), Some(200), Some(ExpireCondition::Lt))
        .await
        .expect("lt");
    assert_eq!(applied, [true, true], "LT shortens both");
    let ttl: i64 = cmd("TTL").arg(&volatile).query_async(&mut c).await.expect("ttl");
    assert!(
        (150..=200).contains(&ttl),
        "LT shortened the volatile key to 200s, got {ttl}"
    );
    // Unconditional PERSIST reports 0 for a key that is already permanent.
    cmd("PERSIST")
        .arg(&permanent)
        .exec_async(&mut c)
        .await
        .expect("persist");
    let applied = client
        .set_ttl_keys_scattered(keys, None, None)
        .await
        .expect("persist batch");
    assert_eq!(applied, [true, false], "PERSIST only changes the volatile key");

    cmd("DEL")
        .arg(&volatile)
        .arg(&permanent)
        .exec_async(&mut c)
        .await
        .expect("del");
}

/// Hash field writes carry their TTL decision: the `HSET` + `HEXPIRE` /
/// `HPERSIST` fallback on any 7.4+ server, and — where the probe finds
/// `HSETEX` — one atomic write whose `KEEPTTL` survives a value edit that
/// plain `HSET` would strip.
#[test]
#[ignore]
fn standalone_hash_field_writes_carry_their_ttl() {
    smol::block_on(async {
        let id = register(server("it-hash-ttl", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        if !client.supports(floors::HASH_FIELD_TTL) {
            eprintln!("skipped: server has no hash field TTL");
            return;
        }
        let atomic = probe_server_features(&id, 0)
            .await
            .expect("probe")
            .status(ServerCommand::HSetEx)
            == CommandStatus::Available;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("hf");

        async fn ttl_of(c: &mut RedisAsyncConn, key: &str, field: &str) -> i64 {
            let ttls: Vec<i64> = cmd("HTTL")
                .arg(key)
                .arg("FIELDS")
                .arg(1)
                .arg(field)
                .query_async(c)
                .await
                .expect("httl");
            ttls[0]
        }

        // Fallback path, available on every 7.4+ server.
        let created = write_hash_field(&at, &key, b"f", b"v1", FieldTtl::Expire(1000), false)
            .await
            .expect("hset+hexpire");
        assert!(created, "first write creates the field");
        assert!((900..=1000).contains(&ttl_of(&mut c, &key, "f").await));
        let created = write_hash_field(&at, &key, b"f", b"v2", FieldTtl::Persist, false)
            .await
            .expect("hset+hpersist");
        assert!(!created, "second write overwrites");
        assert_eq!(ttl_of(&mut c, &key, "f").await, -1, "Persist removed the TTL");

        if atomic {
            let created = write_hash_field(&at, &key, b"f", b"v3", FieldTtl::Expire(500), true)
                .await
                .expect("hsetex ex");
            assert!(!created, "HSETEX on an existing field reports an overwrite");
            assert!((450..=500).contains(&ttl_of(&mut c, &key, "f").await));
            write_hash_field(&at, &key, b"f", b"v4", FieldTtl::Keep, true)
                .await
                .expect("hsetex keepttl");
            let value: String = cmd("HGET").arg(&key).arg("f").query_async(&mut c).await.expect("hget");
            assert_eq!(value, "v4");
            assert!(
                (450..=500).contains(&ttl_of(&mut c, &key, "f").await),
                "KEEPTTL: the value changed, the TTL did not"
            );
            rename_hash_field(&at, &key, b"f", b"g", b"v5", FieldTtl::Expire(300), true)
                .await
                .expect("rename");
            let old: i64 = cmd("HEXISTS")
                .arg(&key)
                .arg("f")
                .query_async(&mut c)
                .await
                .expect("hexists");
            assert_eq!(old, 0, "rename removed the old field");
            assert!(
                (250..=300).contains(&ttl_of(&mut c, &key, "g").await),
                "the new field got its TTL"
            );
        } else {
            eprintln!("HSETEX not probed on this server: atomic path not exercised");
        }

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// The script library's cache operations: the digest computed locally is the
/// one the server computes, `SCRIPT EXISTS` answers per digest in order, and
/// a script never loaded is `false` rather than an error.
#[test]
#[ignore]
fn standalone_script_cache_agrees_with_the_local_digest() {
    smol::block_on(async {
        let id = register(server("it-script-cache", standalone())).await;
        let at = ServerDb::new(&id, 0);
        // Unique source, so another test's SCRIPT FLUSH or LOAD cannot be
        // what this one observes.
        let code = format!("return '{}'", unique("script"));
        let sha = script_sha1(&code);
        assert_eq!(sha.len(), 40);
        assert_eq!(
            script_sha1("return 1"),
            "e0e1f9fabfc9d4800c877a703b823ac0578ff8db",
            "SHA-1 of the source, as redis-cli prints it"
        );

        let never = script_sha1(&format!("{code} -- never loaded"));
        assert_eq!(
            script_exists(&at, &[sha.clone(), never.clone()])
                .await
                .expect("script exists"),
            [false, false]
        );
        assert_eq!(script_load(&at, &code).await.expect("script load"), sha);
        assert_eq!(
            script_exists(&at, &[never, sha]).await.expect("script exists"),
            [false, true],
            "one answer per digest, in the order asked"
        );
        assert!(script_exists(&at, &[]).await.expect("no digests").is_empty());
    });
}

/// `EVALSHA_RO` (7.0): the read-only spelling makes the server refuse a
/// write inside the script; the same script runs under `EVALSHA`, and a
/// reading script runs under the read-only one.
#[test]
#[ignore]
fn standalone_evalsha_ro_rejects_writes() {
    smol::block_on(async {
        let id = register(server("it-eval-ro", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        if !client.supports(floors::EVAL_RO) {
            eprintln!("skipped: server predates EVALSHA_RO");
            return;
        }
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let key = unique("evalro");
        let writer = "return redis.call('SET', KEYS[1], 'written')";
        let reader = "return redis.call('GET', KEYS[1])";
        let sha_of = |code: &str| redis::Script::new(code).get_hash().to_string();
        let keys = vec![key.clone()];

        let refused = run_script(&at, writer, &sha_of(writer), &keys, &[], true).await;
        let message = refused.expect_err("EVALSHA_RO must refuse a write").to_string();
        assert!(
            message.to_lowercase().contains("read-only"),
            "the server names the reason: {message}"
        );
        let exists: i64 = cmd("EXISTS").arg(&key).query_async(&mut c).await.expect("exists");
        assert_eq!(exists, 0, "the refused write left nothing behind");

        run_script(&at, writer, &sha_of(writer), &keys, &[], false)
            .await
            .expect("EVALSHA writes");
        let read = run_script(&at, reader, &sha_of(reader), &keys, &[], true)
            .await
            .expect("EVALSHA_RO reads");
        assert!(read.formatted.contains("written"), "read back: {}", read.formatted);

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// `INFO keysizes` (8+): written keys land in per-type bucket histograms —
/// strings bucketed by value bytes, containers by element count.
#[test]
#[ignore]
fn standalone_info_keysizes_buckets_types() {
    smol::block_on(async {
        let id = register(server("it-keysizes", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        // Flavor-aware: Valkey 8.x passes a bare 8.0 floor but has no
        // `keysizes` section, so `INFO keysizes` comes back empty there.
        if !client.supports_info_keysizes() {
            eprintln!("skipped: server has no INFO keysizes (Redis 8+ only)");
            return;
        }
        let prefix = unique("ks");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(format!("{prefix}:s"))
            .arg("x".repeat(100))
            .exec_async(&mut c)
            .await
            .expect("set");
        for i in 0..5 {
            cmd("RPUSH")
                .arg(format!("{prefix}:l"))
                .arg(i)
                .exec_async(&mut c)
                .await
                .expect("rpush");
        }

        let dists = client.info_keysizes().await.expect("keysizes");
        let strings = dists.iter().find(|d| d.type_name == "strings").expect("strings dist");
        assert_eq!(strings.unit, KeysizesUnit::Bytes);
        assert!(strings.total() >= 1);
        let lists = dists.iter().find(|d| d.type_name == "lists").expect("lists dist");
        assert_eq!(lists.unit, KeysizesUnit::Items);
        assert!(lists.total() >= 1);

        cmd("DEL")
            .arg(format!("{prefix}:s"))
            .arg(format!("{prefix}:l"))
            .exec_async(&mut c)
            .await
            .expect("del");
    });
}

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

#[test]
#[ignore]
fn cluster_migrates_slots_atomically_on_valkey_9() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(protected_server("it-cluster-atomic", addr)).await;
        let _slots = CLUSTER_SLOTS.lock().await;
        if !supports(&id, floors::ATOMIC_SLOT_MIGRATION).await {
            eprintln!("skipped: atomic slot migration is Valkey 9.0+");
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
        async fn migration_names<C: redis::aio::ConnectionLike + Send>(conn: &mut C) -> HashSet<String> {
            cluster_get_slot_migrations(conn)
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
            before: &HashSet<String>,
            target_id: &str,
            what: &str,
        ) -> zedis_connection::AtomicSlotMigration {
            let mut last = Vec::new();
            for _ in 0..160 {
                last = cluster_get_slot_migrations(conn).await.expect("getslotmigrations");
                if let Some(found) = last.iter().find(|migration| {
                    migration.is_export()
                        && migration.target_node == target_id
                        && !before.contains(&migration.name)
                        && !migration.is_active()
                }) {
                    return found.clone();
                }
                smol::Timer::after(std::time::Duration::from_millis(250)).await;
            }
            panic!("{what}: {last:?}");
        }

        let map = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("client")
            .nodes_description()
            .slot_map;
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

        let before = migration_names(&mut source_conn).await;
        cluster_migrate_slots(&mut source_conn, &[moving], &target.node_id)
            .await
            .expect("migrateslots");
        let job = wait_for_export(
            &mut source_conn,
            &before,
            &target.node_id,
            "the migration reached a terminal state",
        )
        .await;
        assert_eq!(job.state, "success", "{job:?}");
        assert_eq!(job.source_node, source.node_id);
        assert!(!job.slot_ranges.is_empty());
        assert_eq!(
            owner_of(&id, moving.1).await,
            target.node_id,
            "ownership moved with the slots"
        );

        // Put them back so the cluster ends where it started.
        let before = migration_names(&mut target_conn).await;
        cluster_migrate_slots(&mut target_conn, &[moving], &source.node_id)
            .await
            .expect("migrateslots back");
        let back = wait_for_export(
            &mut target_conn,
            &before,
            &source.node_id,
            "the migration back reached a terminal state",
        )
        .await;
        assert_eq!(back.state, "success", "{back:?}");
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

        // A node with no migration running answers `CANCELSLOTMIGRATIONS`
        // where it has the command, and says so where it does not.
        match node_cancel_slot_migrations(&source_node).await {
            Ok(()) => {}
            Err(e) => assert!(
                e.to_string().contains("CLUSTER CANCELSLOTMIGRATIONS on "),
                "the error names the node it was sent to: {e}"
            ),
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

// ── redis-stack ──────────────────────────────────────────────────────────

#[test]
#[ignore]
fn stack_modules_are_detected_and_usable() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        assert!(client.supports_rejson(), "ReJSON must be listed by MODULE LIST");
        assert!(client.supports_search(), "RediSearch must be listed by MODULE LIST");

        let key = unique("json");
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        cmd("JSON.SET")
            .arg(&key)
            .arg("$")
            .arg(r#"{"a":1}"#)
            .exec_async(&mut c)
            .await
            .expect("json.set");
        assert_eq!(client.key_type(&key).await.expect("type"), "ReJSON-RL");
        let listing = zedis_connection::ft_list(&at).await.expect("ft._list");
        assert!(!listing.unsupported);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// The `JSON.*` path writes behind the JSON editor's menu and tree: each
/// one is built by `run_key_op` and reports back what the panel shows —
/// for both path syntaxes, since RedisJSON answers a `$` path per match
/// and a legacy `.` path with a bare scalar.
#[test]
#[ignore]
fn stack_json_path_ops_run_and_report_their_result() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack-json", standalone())).await;
        let features = probe_server_features(&id, 0).await.expect("probe");
        for c in [
            ServerCommand::JsonSet,
            ServerCommand::JsonDel,
            ServerCommand::JsonNumIncrBy,
            ServerCommand::JsonToggle,
            ServerCommand::JsonArrAppend,
            ServerCommand::JsonStrAppend,
            ServerCommand::JsonClear,
        ] {
            assert_eq!(features.status(c), CommandStatus::Available, "{c:?} on the stack image");
        }

        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("json-ops");
        cmd("JSON.SET")
            .arg(&key)
            .arg("$")
            .arg(r#"{"n":5,"ok":false,"s":"ab","arr":[1],"o":{"k":1}}"#)
            .exec_async(&mut c)
            .await
            .expect("json.set");
        let json = |path: &str, op: JsonPathOp| KeyOp::Json {
            path: path.to_string(),
            op,
        };
        assert_eq!(
            run_key_op(&at, &key, json("$.n", JsonPathOp::NumIncrBy(2.0)))
                .await
                .expect("numincrby"),
            KeyOpOutcome::Number("7".into()),
            "a `$` path answers `[7]`, read back as the number"
        );
        assert_eq!(
            run_key_op(&at, &key, json(".n", JsonPathOp::NumIncrBy(0.5)))
                .await
                .expect("numincrby, legacy path"),
            KeyOpOutcome::Number("7.5".into())
        );
        assert_eq!(
            run_key_op(&at, &key, json("$.ok", JsonPathOp::Toggle))
                .await
                .expect("toggle"),
            KeyOpOutcome::Number("true".into())
        );
        assert_eq!(
            run_key_op(&at, &key, json("$.arr", JsonPathOp::ArrAppend(serde_json::json!("x"))))
                .await
                .expect("arrappend"),
            KeyOpOutcome::Count(2)
        );
        assert_eq!(
            run_key_op(&at, &key, json("$.s", JsonPathOp::StrAppend("cd".into())))
                .await
                .expect("strappend"),
            KeyOpOutcome::Count(4)
        );
        assert_eq!(
            run_key_op(
                &at,
                &key,
                json("$.o.k2", JsonPathOp::Set(serde_json::json!({"deep": true})))
            )
            .await
            .expect("set a new member"),
            KeyOpOutcome::Done
        );
        assert_eq!(
            run_key_op(&at, &key, json("$.o", JsonPathOp::Clear))
                .await
                .expect("clear"),
            KeyOpOutcome::Count(1)
        );
        assert_eq!(
            run_key_op(&at, &key, json("$.arr[0]", JsonPathOp::Del))
                .await
                .expect("del"),
            KeyOpOutcome::Count(1)
        );
        let doc: String = cmd("JSON.GET").arg(&key).query_async(&mut c).await.expect("json.get");
        let doc: serde_json::Value = serde_json::from_str(&doc).expect("a JSON document");
        assert_eq!(
            doc,
            serde_json::json!({"n": 7.5, "ok": true, "s": "abcd", "arr": ["x"], "o": {}})
        );
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// What the search panel shows around a query: the index's size from
/// FT.INFO, a TAG field's values, and the spelling suggestions offered
/// when a query matches nothing.
#[test]
#[ignore]
fn stack_search_index_size_tag_values_and_spelling() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack-tagvals", standalone())).await;
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let index = unique("idx");
        let prefix = unique("doc");
        cmd("FT.CREATE")
            .arg(&index)
            .arg("ON")
            .arg("HASH")
            .arg("PREFIX")
            .arg(1)
            .arg(format!("{prefix}:"))
            .arg("SCHEMA")
            .arg("title")
            .arg("TEXT")
            .arg("tags")
            .arg("TAG")
            .exec_async(&mut c)
            .await
            .expect("ft.create");
        for (name, title, tags) in [("a", "hello world", "red,green"), ("b", "hello again", "blue")] {
            cmd("HSET")
                .arg(format!("{prefix}:{name}"))
                .arg("title")
                .arg(title)
                .arg("tags")
                .arg(tags)
                .exec_async(&mut c)
                .await
                .expect("hset");
        }

        let info = ft_info(&at, &index).await.expect("ft.info");
        assert_eq!(info.num_docs, 2);
        assert!(
            info.index_bytes().is_some_and(|bytes| bytes > 0),
            "FT.INFO reports the index's size: {info:?}"
        );
        assert!(
            info.inverted_index_bytes.is_some(),
            "the inverted index is one of the parts"
        );

        let mut values = ft_tagvals(&at, &index, "tags").await.expect("ft.tagvals");
        values.sort();
        assert_eq!(
            values,
            vec!["blue", "green", "red"],
            "TAG values, lowercased by the index"
        );

        let suggestions = ft_spellcheck(&at, &index, "helo", None).await.expect("ft.spellcheck");
        let for_term = suggestions
            .iter()
            .find(|entry| entry.term == "helo")
            .expect("a suggestion for helo");
        assert!(
            for_term.suggestions.iter().any(|(word, _)| word == "hello"),
            "the indexed term is proposed: {suggestions:?}"
        );
        assert!(
            ft_spellcheck(&at, &index, "hello", None)
                .await
                .expect("ft.spellcheck")
                .is_empty(),
            "a known term gets no suggestion"
        );

        cmd("FT.DROPINDEX")
            .arg(&index)
            .arg("DD")
            .exec_async(&mut c)
            .await
            .expect("ft.dropindex");
    });
}

/// `FT.SEARCH … PARAMS`: a KNN query bound to a FLOAT32 blob encoded by
/// `search_params` ranks the nearest document first, and `FT.EXPLAIN`
/// plans the same query only because the binding travels with it.
#[test]
#[ignore]
fn stack_search_params_bind_a_knn_vector() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack-knn", standalone())).await;
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let index = unique("idx");
        let prefix = unique("vec");
        cmd("FT.CREATE")
            .arg(&index)
            .arg("ON")
            .arg("HASH")
            .arg("PREFIX")
            .arg(1)
            .arg(format!("{prefix}:"))
            .arg("SCHEMA")
            .arg("v")
            .arg("VECTOR")
            .arg("FLAT")
            .arg(6)
            .arg("TYPE")
            .arg("FLOAT32")
            .arg("DIM")
            .arg(2)
            .arg("DISTANCE_METRIC")
            .arg("L2")
            .exec_async(&mut c)
            .await
            .expect("ft.create");
        for (name, vector) in [("a", "1, 0"), ("b", "0, 1")] {
            cmd("HSET")
                .arg(format!("{prefix}:{name}"))
                .arg("v")
                .arg(encode_param(ParamKind::Float32, vector).expect("encode"))
                .exec_async(&mut c)
                .await
                .expect("hset");
        }

        let query = "*=>[KNN 2 @v $BLOB]";
        let params = vec![(
            "BLOB".to_string(),
            encode_param(ParamKind::Float32, "0.9, 0.1").expect("encode"),
        )];
        let opts = SearchOptions {
            limit: (0, 10),
            dialect: Some(2),
            params: params.clone(),
            ..Default::default()
        };
        let result = ft_search(&at, &index, query, &opts).await.expect("ft.search");
        assert_eq!(result.total, 2);
        assert_eq!(
            result.hits.first().map(|h| h.doc_id.as_str()),
            Some(format!("{prefix}:a").as_str()),
            "(0.9, 0.1) is nearest to (1, 0)"
        );
        let plan = ft_explain(&at, &index, query, &params, Some(2))
            .await
            .expect("ft.explain");
        assert!(plan.contains("VECTOR"), "plan names the vector iterator: {plan}");
        assert!(
            ft_explain(&at, &index, query, &[], Some(2)).await.is_err(),
            "without the binding the server refuses to plan"
        );

        cmd("FT.DROPINDEX")
            .arg(&index)
            .arg("DD")
            .exec_async(&mut c)
            .await
            .expect("ft.dropindex");
    });
}
