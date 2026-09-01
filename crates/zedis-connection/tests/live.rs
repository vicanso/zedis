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

use redis::cmd;
use std::collections::HashSet;
use std::env;
use std::sync::Once;
use zedis_connection::error::ConnectionErrorKind;
use zedis_connection::{
    CommandStatus, ConflictMode, ImportFormat, ReadLimits, ReadableValue, ReadableWriteStatus, RedisAsyncConn,
    RedisServer, RestoreStatus, ServerCommand, ServerFlavor, SlotStatMetric, acl_del_user, acl_get_user, acl_set_user,
    csv_header, dump_keys_chunk, entry_to_csv, entry_to_json, get_connection_manager, get_servers,
    open_single_connection, parse_readable_entries, probe_server_features, read_readable_chunk, restore_keys_chunk,
    save_servers, sniff_import_format, split_acl_rules, write_readable_chunk,
};
use zedis_core::keysizes::KeysizesUnit;

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

async fn version_at_least(id: &str, version: &str) -> bool {
    get_connection_manager()
        .get_client(id, 0)
        .await
        .expect("client")
        .is_at_least_version(version)
}

// ── standalone ───────────────────────────────────────────────────────────

#[test]
#[ignore]
fn standalone_connect_reports_metadata() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping");
        assert!(!client.version().is_empty(), "version must be read from INFO server");
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
fn standalone_dump_restore_round_trips_a_key() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
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
        let entries = dump_keys_chunk(&mut c, std::slice::from_ref(&key)).await.expect("dump");
        assert_eq!(entries.len(), 1);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");

        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Skip)
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
        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Skipped), "{statuses:?}");
        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Overwrite)
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
        let entries = read_readable_chunk(&mut c, &keys, limits).await.expect("read chunk");
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

        let exported = read_readable_chunk(&mut c, &keys, ReadLimits::default())
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
        let statuses = write_readable_chunk(&mut c, &entries, ConflictMode::Skip)
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
        let statuses = write_readable_chunk(&mut c, &entries, ConflictMode::Skip)
            .await
            .expect("write again");
        assert!(
            statuses.iter().all(|s| *s == ReadableWriteStatus::SkippedExists),
            "{statuses:?}"
        );
        let llen: i64 = cmd("LLEN").arg(&l_key).query_async(&mut c).await.expect("llen");
        assert_eq!(llen, 3, "Skip must not append to the existing list");

        // Overwrite replaces (DEL first), it must not append either.
        let statuses = write_readable_chunk(&mut c, &entries, ConflictMode::Overwrite)
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
        let statuses = write_readable_chunk(&mut c, &entries, ConflictMode::Skip)
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
        let has_functions = version_at_least(&id, "7.0.0").await;
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
        // the rule valid on Redis 6 (subcommand denials are 7.0+).
        cmd("ACL")
            .arg("SETUSER")
            .arg(&ro_user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("&*")
            .arg("+@read")
            .arg("+@connection")
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

        let mut ro = server(&format!("it-ro-{suffix}"), standalone());
        ro.username = Some(ro_user.clone());
        ro.password = Some("pw".into());
        let ro_id = register(ro).await;
        let client = get_connection_manager().get_client(&ro_id, 0).await.expect("ro client");
        assert_eq!(
            format!("{:?}", client.access_mode()),
            "StrictReadOnly",
            "a read-only ACL user must be detected (DRYRUN on 7+, SET probe before)"
        );

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
        if version_at_least(&admin_id, "7.0.0").await {
            for c in [ServerCommand::ConfigSet, ServerCommand::Bgsave, ServerCommand::FlushDb] {
                assert_eq!(features.status(c), CommandStatus::Denied, "{c:?} (ACL DRYRUN)");
            }
        }

        for user in [&ro_user, &limited_user] {
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

// ── tls ──────────────────────────────────────────────────────────────────

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

// ── sentinel ─────────────────────────────────────────────────────────────

/// `SENTINEL GET-MASTER-ADDR-BY-NAME` straight from the sentinel.
async fn sentinel_master_port(sentinel: &mut redis::aio::MultiplexedConnection, master_name: &str) -> u16 {
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
        let mut sentinel = open_single_connection(&server("it-sentinel-raw", addr.clone()), 0, false)
            .await
            .expect("sentinel connection");
        let before = sentinel_master_port(&mut sentinel, &master_name).await;

        let mut s = server("it-sentinel", addr);
        s.master_name = Some(master_name.clone());
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
        let demoted_server = server("it-demoted", ("127.0.0.1".into(), before));
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

// ── cluster ──────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn cluster_discovers_nodes_and_scans_every_master() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(server("it-cluster", addr)).await;
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
        if !version_at_least(&id, "8.6.0").await {
            eprintln!("skipped: server < 8.6");
            return;
        }
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
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
#[test]
#[ignore]
fn standalone_acl_selectors_round_trip() {
    smol::block_on(async {
        let id = register(server("it-acl-sel", standalone())).await;
        if !version_at_least(&id, "7.0.0").await {
            eprintln!("skipped: server < 7.0");
            return;
        }
        let username = unique("selector-user").replace(':', "-");
        let mut c = conn(&id, 0).await;
        let rules = split_acl_rules("on ~app:* +@read (-@all +lpush ~queue:*)");
        assert_eq!(rules.len(), 4, "the selector group must stay one argument");
        acl_set_user(&mut c, &username, &rules).await.expect("setuser");

        let user = acl_get_user(&mut c, &username).await.expect("getuser");
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
        acl_set_user(&mut c, &username, &split_acl_rules(&text))
            .await
            .expect("re-apply rules text");
        let again = acl_get_user(&mut c, &username).await.expect("getuser again");
        assert_eq!(again.selectors, user.selectors, "re-applying is lossless");

        acl_del_user(&mut c, &username).await.expect("deluser");
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

/// `INFO keysizes` (8+): written keys land in per-type bucket histograms —
/// strings bucketed by value bytes, containers by element count.
#[test]
#[ignore]
fn standalone_info_keysizes_buckets_types() {
    smol::block_on(async {
        let id = register(server("it-keysizes", standalone())).await;
        if !version_at_least(&id, "8.0.0").await {
            eprintln!("skipped: server < 8.0");
            return;
        }
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
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
#[test]
#[ignore]
fn cluster_slot_stats_ranks_slots_by_key_count() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(server("it-slot-stats", addr)).await;
        if !version_at_least(&id, "8.2.0").await {
            eprintln!("skipped: cluster < 8.2");
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
        cmd("JSON.SET")
            .arg(&key)
            .arg("$")
            .arg(r#"{"a":1}"#)
            .exec_async(&mut c)
            .await
            .expect("json.set");
        assert_eq!(client.key_type(&key).await.expect("type"), "ReJSON-RL");
        let listing = zedis_connection::ft_list(&mut c).await.expect("ft._list");
        assert!(!listing.unsupported);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}
