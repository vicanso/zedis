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

//! Moving keys: dump and restore, the readable export and import, compare,
//! and the copy between servers of either flavor.

use super::*;

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
        // A key the capped side did not list is asked for by name, not
        // called missing: only the keys one side really lacks are listed.
        assert!(
            capped.only_source.iter().all(|(k, _)| *k == key("only0")),
            "{:?}",
            capped.only_source
        );
        assert!(
            capped.only_target.iter().all(|(k, _)| *k == key("only1")),
            "{:?}",
            capped.only_target
        );

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
        // A wrong password against a server whose `default` user needs none:
        // the password-less retry (meant for sentinels) used to connect as
        // `default` and report success.
        let mut wrong = server("it-entry-check-wrong-password", standalone());
        wrong.username = Some("zedis_it_no_such_user".to_string());
        wrong.password = Some("not-the-password".to_string());
        let err = test_connection(&wrong)
            .await
            .expect_err("a wrong password must not pass");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Auth, "{err}");

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

/// The two cases a logical copy used to get wrong. An empty stream is a key
/// of its own: it was reported re-created and never made. And an overwrite
/// deleted the target's key before finding that the source's type cannot be
/// re-created by commands, so the target lost it and got nothing back.
#[test]
#[ignore]
fn standalone_logical_copy_makes_empty_streams_and_never_deletes_what_it_cannot_replace() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let src = ServerDb::new(&id, 0);
        let dst = ServerDb::new(&id, 1);
        let mut c0 = conn(&id, 0).await;
        let mut c1 = conn(&id, 1).await;
        let prefix = unique("logical-edge");

        // An emptied stream, whose next id has moved on.
        let stream = format!("{prefix}:x");
        cmd("XADD")
            .arg(&stream)
            .arg("5-1")
            .arg("f")
            .arg("v")
            .exec_async(&mut c0)
            .await
            .expect("xadd");
        cmd("XDEL")
            .arg(&stream)
            .arg("5-1")
            .exec_async(&mut c0)
            .await
            .expect("xdel");
        let status = copy_key_logically(&src, &dst, stream.as_bytes(), -1, ConflictMode::Skip)
            .await
            .expect("copy");
        assert_eq!(status, RestoreStatus::Recreated);
        let kind: String = cmd("TYPE").arg(&stream).query_async(&mut c1).await.expect("type");
        assert_eq!(kind, "stream", "the empty stream exists on the target");
        let len: usize = cmd("XLEN").arg(&stream).query_async(&mut c1).await.expect("xlen");
        assert_eq!(len, 0);
        let next: String = cmd("XADD")
            .arg(&stream)
            .arg("*")
            .arg("f")
            .arg("w")
            .query_async(&mut c1)
            .await
            .expect("xadd");
        let ms: u64 = next.split('-').next().and_then(|ms| ms.parse().ok()).expect("id");
        assert!(ms >= 5, "the copy kept the source's next id: {next}");

        // A module key (a Bloom filter where the server has one) over an
        // existing target key: refused, and the target's key left alone.
        let bloom = format!("{prefix}:bf");
        if cmd("BF.ADD").arg(&bloom).arg("a").exec_async(&mut c0).await.is_ok() {
            cmd("SET")
                .arg(&bloom)
                .arg("keep me")
                .exec_async(&mut c1)
                .await
                .expect("set");
            let status = copy_key_logically(&src, &dst, bloom.as_bytes(), -1, ConflictMode::Overwrite)
                .await
                .expect("copy");
            assert!(matches!(status, RestoreStatus::Failed(_)), "{status:?}");
            let kept: String = cmd("GET").arg(&bloom).query_async(&mut c1).await.expect("get");
            assert_eq!(kept, "keep me");
        } else {
            eprintln!("skipped the module half: no BF.ADD on this server");
        }

        for key in [&stream, &bloom] {
            cmd("DEL").arg(key).exec_async(&mut c0).await.expect("del");
            cmd("DEL").arg(key).exec_async(&mut c1).await.expect("del");
        }
    });
}

/// A key of each core type re-created on another database by commands
/// alone (ADR 16): the value equal, the TTL kept, and the three conflict
/// answers where the key already exists.
#[test]
#[ignore]
fn standalone_logical_copy_recreates_every_core_type_with_its_ttl() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let src = ServerDb::new(&id, 0);
        let dst = ServerDb::new(&id, 1);
        let mut c0 = conn(&id, 0).await;
        let mut c1 = conn(&id, 1).await;
        let prefix = unique("logical");
        let string = format!("{prefix}:s");
        let hash = format!("{prefix}:h");
        let list = format!("{prefix}:l");
        let set = format!("{prefix}:set");
        let zset = format!("{prefix}:z");
        let stream = format!("{prefix}:x");
        let json = format!("{prefix}:j");
        let set_ok = async |c: &mut RedisAsyncConn, words: &[&str]| {
            let mut command = cmd(words[0]);
            for word in &words[1..] {
                command.arg(*word);
            }
            command.exec_async(c).await.expect(words[0]);
        };
        set_ok(&mut c0, &["SET", &string, "v\u{1f600}", "PX", "300000"]).await;
        set_ok(&mut c0, &["HSET", &hash, "a", "1", "b", "2", "c", "3"]).await;
        set_ok(&mut c0, &["RPUSH", &list, "x", "y", "z", "x"]).await;
        set_ok(&mut c0, &["SADD", &set, "m", "n", "o"]).await;
        set_ok(&mut c0, &["ZADD", &zset, "1.5", "a", "2", "b"]).await;
        set_ok(&mut c0, &["XADD", &stream, "1-1", "f", "v"]).await;
        set_ok(&mut c0, &["XADD", &stream, "1-2", "f", "w"]).await;
        set_ok(&mut c0, &["XADD", &stream, "2-0", "g", "h"]).await;
        let json_ok = cmd("JSON.SET")
            .arg(&json)
            .arg("$")
            .arg("{\"a\":[1,2]}")
            .exec_async(&mut c0)
            .await
            .is_ok();
        let mut keys = vec![
            string.clone(),
            hash.clone(),
            list.clone(),
            set.clone(),
            zset.clone(),
            stream.clone(),
        ];
        if json_ok {
            keys.push(json.clone());
        }
        for key in &keys {
            let pttl: i64 = cmd("PTTL").arg(key).query_async(&mut c0).await.expect("pttl");
            let status = copy_key_logically(&src, &dst, key.as_bytes(), pttl, ConflictMode::Skip)
                .await
                .expect("copy");
            assert_eq!(status, RestoreStatus::Recreated, "{key}");
        }

        let value: String = cmd("GET").arg(&string).query_async(&mut c1).await.expect("get");
        assert_eq!(value, "v\u{1f600}");
        let pttl: i64 = cmd("PTTL").arg(&string).query_async(&mut c1).await.expect("pttl");
        assert!((1..=300_000).contains(&pttl), "the TTL travelled: {pttl}");
        let fields: HashMap<String, String> = cmd("HGETALL").arg(&hash).query_async(&mut c1).await.expect("hgetall");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields.get("b").map(String::as_str), Some("2"));
        let items: Vec<String> = cmd("LRANGE")
            .arg(&list)
            .arg(0)
            .arg(-1)
            .query_async(&mut c1)
            .await
            .expect("lrange");
        assert_eq!(items, ["x", "y", "z", "x"], "order and repeats kept");
        let mut members: Vec<String> = cmd("SMEMBERS").arg(&set).query_async(&mut c1).await.expect("smembers");
        members.sort();
        assert_eq!(members, ["m", "n", "o"]);
        let scored: Vec<(String, f64)> = cmd("ZRANGE")
            .arg(&zset)
            .arg(0)
            .arg(-1)
            .arg("WITHSCORES")
            .query_async(&mut c1)
            .await
            .expect("zrange");
        assert_eq!(scored, [("a".to_string(), 1.5), ("b".to_string(), 2.0)]);
        let entries: Vec<(String, HashMap<String, String>)> = cmd("XRANGE")
            .arg(&stream)
            .arg("-")
            .arg("+")
            .query_async(&mut c1)
            .await
            .expect("xrange");
        let ids: Vec<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["1-1", "1-2", "2-0"], "entries keep their ids");
        assert_eq!(entries[2].1.get("g").map(String::as_str), Some("h"));
        if json_ok {
            let document: String = cmd("JSON.GET").arg(&json).query_async(&mut c1).await.expect("json.get");
            assert_eq!(document, "{\"a\":[1,2]}");
        }

        // The key exists now: skip leaves it, overwrite replaces rather than
        // merges, abort is an error.
        let skipped = copy_key_logically(&src, &dst, list.as_bytes(), -1, ConflictMode::Skip)
            .await
            .expect("skip");
        assert_eq!(skipped, RestoreStatus::Skipped);
        let replaced = copy_key_logically(&src, &dst, list.as_bytes(), -1, ConflictMode::Overwrite)
            .await
            .expect("overwrite");
        assert_eq!(replaced, RestoreStatus::Recreated);
        let len: usize = cmd("LLEN").arg(&list).query_async(&mut c1).await.expect("llen");
        assert_eq!(len, 4, "overwrite replaced the list instead of appending to it");
        assert!(
            copy_key_logically(&src, &dst, list.as_bytes(), -1, ConflictMode::Abort)
                .await
                .is_err(),
            "abort on an existing key"
        );

        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c0).await.expect("del");
            cmd("DEL").arg(key).exec_async(&mut c1).await.expect("del");
        }
    });
}

/// The fallback fires on the one refusal it is for: a payload the target
/// calls another server's. Made here by writing an RDB version no server
/// has into a real dump; an intact one still goes through `RESTORE`.
#[test]
#[ignore]
fn standalone_restore_or_recreate_falls_back_when_the_payload_is_another_servers() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let src = ServerDb::new(&id, 0);
        let dst = ServerDb::new(&id, 1);
        let mut c0 = conn(&id, 0).await;
        let mut c1 = conn(&id, 1).await;
        let key = unique("foreign");
        cmd("HSET")
            .arg(&key)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut c0)
            .await
            .expect("hset");
        let entries = dump_keys_chunk(&src, std::slice::from_ref(&key)).await.expect("dump");
        let mut foreign = entries.first().cloned().expect("one entry");
        // The two RDB version bytes sit before the eight of the checksum.
        let n = foreign.payload.len();
        foreign.payload[n - 10] = 0xff;
        foreign.payload[n - 9] = 0xff;
        let refused = restore_keys_chunk(&dst, std::slice::from_ref(&foreign), ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(
            matches!(&refused[0], RestoreStatus::Failed(message) if is_foreign_payload(message)),
            "{refused:?}"
        );

        let statuses = restore_or_recreate_chunk(&src, &dst, std::slice::from_ref(&foreign), ConflictMode::Skip)
            .await
            .expect("restore or recreate");
        assert_eq!(statuses, [RestoreStatus::Recreated]);
        let fields: HashMap<String, String> = cmd("HGETALL").arg(&key).query_async(&mut c1).await.expect("hgetall");
        assert_eq!(fields.get("a").map(String::as_str), Some("1"));
        assert_eq!(fields.len(), 2);

        cmd("DEL").arg(&key).exec_async(&mut c1).await.expect("del");
        let statuses = restore_or_recreate_chunk(&src, &dst, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert_eq!(
            statuses,
            [RestoreStatus::Written],
            "an intact payload is restored as before"
        );
        cmd("DEL").arg(&key).exec_async(&mut c0).await.expect("del");
        cmd("DEL").arg(&key).exec_async(&mut c1).await.expect("del");
    });
}

/// A copy onto a server of the other flavor, run by hand:
/// `ZEDIS_IT_FOREIGN=host:port` names it (a Redis when the standalone is a
/// Valkey, or the other way round). `RESTORE` between the two is refused in
/// every direction but Valkey 8 → Redis, so the key lands re-created —
/// or written, where the payloads happen to agree — and equal either way.
#[test]
#[ignore]
fn standalone_copy_lands_on_a_server_of_the_other_flavor() {
    smol::block_on(async {
        let Some(foreign) = scenario("ZEDIS_IT_FOREIGN") else {
            eprintln!("skipped: ZEDIS_IT_FOREIGN not set");
            return;
        };
        let id = register(server("it-standalone", standalone())).await;
        let far = register(server("it-foreign", foreign)).await;
        let mut near = conn(&id, 0).await;
        let mut there = conn(&far, 0).await;
        let key = unique("across");
        cmd("HSET")
            .arg(&key)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut near)
            .await
            .expect("hset");
        cmd("EXPIRE")
            .arg(&key)
            .arg(300)
            .exec_async(&mut near)
            .await
            .expect("expire");
        let status = copy_key(id.clone(), 0, far.clone(), 0, key.clone(), ConflictMode::Skip)
            .await
            .expect("copy");
        assert!(
            matches!(status, Some(RestoreStatus::Written | RestoreStatus::Recreated)),
            "{status:?}"
        );
        let fields: HashMap<String, String> = cmd("HGETALL").arg(&key).query_async(&mut there).await.expect("hgetall");
        assert_eq!(fields.get("b").map(String::as_str), Some("2"));
        let ttl: i64 = cmd("TTL").arg(&key).query_async(&mut there).await.expect("ttl");
        assert!((1..=300).contains(&ttl), "the TTL travelled: {ttl}");
        eprintln!("{key} landed on the other flavor as {status:?}");
        cmd("DEL").arg(&key).exec_async(&mut near).await.expect("del");
        cmd("DEL").arg(&key).exec_async(&mut there).await.expect("del");
    });
}

/// Before a key is copied, the preflight says whether the target can take
/// its `DUMP` — asked of the target with a `RESTORE` that writes nothing —
/// and, where it cannot, what re-creating the key by type would leave
/// behind: a stream's consumer groups, a hash's field TTLs. Its answer is
/// the copy's: a key it calls re-created comes back `Recreated`.
/// `ZEDIS_IT_FOREIGN` names a server of the other flavor for the second half.
#[test]
#[ignore]
fn standalone_copy_preflight_says_what_a_copy_would_leave_behind() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut near = conn(&id, 0).await;
        let stream = unique("preflight-stream");
        cmd("XADD")
            .arg(&stream)
            .arg("*")
            .arg("f")
            .arg("v")
            .exec_async(&mut near)
            .await
            .expect("xadd");
        for group in ["g1", "g2"] {
            cmd("XGROUP")
                .arg("CREATE")
                .arg(&stream)
                .arg(group)
                .arg("0")
                .exec_async(&mut near)
                .await
                .expect("group");
        }
        let hash = unique("preflight-hash");
        cmd("HSET")
            .arg(&hash)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut near)
            .await
            .expect("hset");
        let field_ttls = supports(&id, floors::HASH_FIELD_TTL).await;
        if field_ttls {
            cmd("HEXPIRE")
                .arg(&hash)
                .arg(300)
                .arg("FIELDS")
                .arg(1)
                .arg("a")
                .exec_async(&mut near)
                .await
                .expect("hexpire");
        }
        let plain = unique("preflight-plain");
        cmd("SET")
            .arg(&plain)
            .arg("v")
            .exec_async(&mut near)
            .await
            .expect("set");
        let source = ServerDb::new(&id, 0);

        // One server, another database: it reads its own payloads.
        let same = copy_preflight(&source, &ServerDb::new(&id, 1), &stream)
            .await
            .expect("preflight")
            .expect("the key is there");
        assert!(!same.recreated, "{same:?}");
        assert!(same.losses.is_empty());
        assert_eq!(same.source, same.target);
        assert!(!same.source.is_empty());
        assert_eq!(
            copy_preflight(&source, &ServerDb::new(&id, 1), &unique("preflight-gone"))
                .await
                .expect("preflight"),
            None,
            "a key that is not there"
        );

        let Some(foreign) = scenario("ZEDIS_IT_FOREIGN") else {
            eprintln!("skipped the other flavor: ZEDIS_IT_FOREIGN not set");
            cmd("DEL")
                .arg(&stream)
                .arg(&hash)
                .arg(&plain)
                .exec_async(&mut near)
                .await
                .expect("cleanup");
            return;
        };
        let far = register(server("it-foreign", foreign)).await;
        let target = ServerDb::new(&far, 0);
        let mut there = conn(&far, 0).await;
        let check = |key: String| {
            let (source, target) = (source.clone(), target.clone());
            async move {
                copy_preflight(&source, &target, &key)
                    .await
                    .expect("preflight")
                    .expect("there")
            }
        };
        let plain_check = check(plain.clone()).await;
        assert_ne!(plain_check.source, plain_check.target, "{plain_check:?}");
        let stream_check = check(stream.clone()).await;
        let hash_check = check(hash.clone()).await;
        if plain_check.recreated {
            assert!(plain_check.losses.is_empty(), "a string loses nothing");
            assert_eq!(stream_check.losses, [CopyLoss::ConsumerGroups(2)]);
            if field_ttls {
                assert_eq!(
                    hash_check.losses,
                    [CopyLoss::FieldTtls {
                        count: 1,
                        all_checked: true
                    }]
                );
            }
        } else {
            assert!(stream_check.losses.is_empty() && hash_check.losses.is_empty());
        }
        // The probe wrote nothing on the target.
        let (_, probes): (u64, Vec<String>) = cmd("SCAN")
            .arg(0)
            .arg("MATCH")
            .arg("zedis:copy-probe:*")
            .arg("COUNT")
            .arg(10_000)
            .query_async(&mut there)
            .await
            .expect("scan");
        assert!(probes.is_empty(), "{probes:?}");
        // And the copy of what it dumped does what it said it would.
        let status = plain_check
            .copy(&source, &target, ConflictMode::Overwrite)
            .await
            .expect("copy");
        let expected = if plain_check.recreated {
            RestoreStatus::Recreated
        } else {
            RestoreStatus::Written
        };
        assert_eq!(status, expected);
        let copied: String = cmd("GET").arg(&plain).query_async(&mut there).await.expect("get");
        assert_eq!(copied, "v");
        eprintln!(
            "{} -> {}: re-created = {}",
            plain_check.source, plain_check.target, plain_check.recreated
        );
        cmd("DEL")
            .arg(&stream)
            .arg(&hash)
            .arg(&plain)
            .exec_async(&mut near)
            .await
            .expect("cleanup");
        cmd("DEL").arg(&plain).exec_async(&mut there).await.expect("cleanup");
    });
}

#[test]
#[ignore]
fn standalone_verify_copied_keys_reports_matched_missing_and_differing() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let src = ServerDb::new(&id, 0);
        let dst = ServerDb::new(&id, 1);
        let mut c0 = conn(&id, 0).await;
        let mut c1 = conn(&id, 1).await;

        let k_same = unique("verify-same");
        let k_diff = unique("verify-diff");
        let k_missing = unique("verify-missing");

        cmd("SET")
            .arg(&k_same)
            .arg("v1")
            .exec_async(&mut c0)
            .await
            .expect("set");
        cmd("SET")
            .arg(&k_same)
            .arg("v1")
            .exec_async(&mut c1)
            .await
            .expect("set");

        cmd("SET")
            .arg(&k_diff)
            .arg("v1")
            .exec_async(&mut c0)
            .await
            .expect("set");
        cmd("SET")
            .arg(&k_diff)
            .arg("v2")
            .exec_async(&mut c1)
            .await
            .expect("set");

        cmd("SET")
            .arg(&k_missing)
            .arg("v1")
            .exec_async(&mut c0)
            .await
            .expect("set");

        let keys = vec![k_same.clone(), k_diff.clone(), k_missing.clone()];
        let cancel = AtomicBool::new(false);
        let report = verify_copied_keys(&src, &dst, &keys, &cancel)
            .await
            .expect("verify_copied_keys");
        assert_eq!(report.checked, 3);
        assert_eq!(report.matched, 1);
        assert_eq!(report.missing_on_target, vec![k_missing.clone()]);
        assert_eq!(report.differing.len(), 1);
        assert_eq!(report.differing[0].key, k_diff);

        cmd("DEL")
            .arg(&k_same)
            .arg(&k_diff)
            .arg(&k_missing)
            .exec_async(&mut c0)
            .await
            .expect("del c0");
        cmd("DEL")
            .arg(&k_same)
            .arg(&k_diff)
            .exec_async(&mut c1)
            .await
            .expect("del c1");
    });
}
