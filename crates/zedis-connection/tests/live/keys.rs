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

//! The keyspace: scanning and searching it, deleting, expiring and the
//! key menu's operations.

use super::*;

#[test]
#[ignore]
fn standalone_scan_sees_every_type_it_wrote() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        scan_sees_every_type_it_wrote(id).await;
    });
}

/// Multi-database search finds a key by its exact name and by a part of it,
/// per target, and says which target could not be searched instead of
/// failing the others.
#[test]
#[ignore]
fn standalone_multi_search_finds_keys_per_target() {
    smol::block_on(async {
        let id = register(server("it-multi-search", standalone())).await;
        let mut c = conn(&id, 0).await;
        let prefix = unique("multi-search");
        let hash = format!("{prefix}:needle-hash");
        cmd("HSET")
            .arg(&hash)
            .arg("f")
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("SET")
            .arg(format!("{prefix}:other"))
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("set");

        let targets = vec![(id.clone(), 0), ("it-no-such-server".to_string(), 0)];
        let exact = multi_search_exact(targets.clone(), hash.clone()).await;
        assert_eq!(exact.len(), 2);
        let found = exact.iter().find(|r| r.server_id == id).expect("the server's result");
        assert_eq!(found.error, None);
        assert_eq!(
            found
                .hits
                .iter()
                .map(|h| (h.key.as_str(), h.key_type.as_str()))
                .collect::<Vec<_>>(),
            [(hash.as_str(), "hash")]
        );
        let missing = exact.iter().find(|r| r.server_id != id).expect("the other target");
        assert!(missing.error.is_some(), "an unknown target answers with its own error");

        let scan = multi_search_scan(vec![(id.clone(), 0)], format!("{prefix}:needle"), 1000).await;
        let keys: Vec<&str> = scan[0].hits.iter().map(|h| h.key.as_str()).collect();
        assert_eq!(keys, [hash.as_str()], "contains-match on the name, not the other key");
        assert!(!scan[0].truncated);

        cmd("DEL")
            .arg(&hash)
            .arg(format!("{prefix}:other"))
            .exec_async(&mut c)
            .await
            .expect("cleanup");
    });
}

/// A value search finds the text in every type it reads and in none it
/// does not, goes on from the cursors a round answers until the keyspace is
/// covered, and with one type asked for reads only keys of that type.
#[test]
#[ignore]
fn standalone_value_search_reads_by_type_and_goes_on_from_its_cursors() {
    smol::block_on(async {
        let id = register(server("it-value-search", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let prefix = unique("value-search");
        let key = |name: &str| format!("{prefix}:{name}");
        let writes = [
            cmd("SET").arg(key("s1")).arg("hello NEEDLE world").clone(),
            cmd("SET").arg(key("s2")).arg("nothing here").clone(),
            cmd("HSET").arg(key("h1")).arg("f").arg("a needle inside").clone(),
            cmd("RPUSH").arg(key("l1")).arg("x").arg("the needle").clone(),
            cmd("SADD").arg(key("set1")).arg("needle").clone(),
            cmd("ZADD").arg(key("z1")).arg(1).arg("needle-member").clone(),
            // Streams are not read, whatever they hold.
            cmd("XADD").arg(key("x1")).arg("*").arg("f").arg("needle").clone(),
        ];
        for write in &writes {
            write.exec_async(&mut c).await.expect("write");
        }
        // SCAN COUNT is a hint. Valkey 9's hashtable can return a small
        // MATCH set in one call (7 keys at COUNT 2 → cursor 0), so extra
        // string keys under the same prefix, without the needle, grow the
        // table until a round leaves a cursor — the Continue path the panel
        // uses.
        const FILLERS: usize = 32;
        let mut fill = cmd("MSET");
        for i in 0..FILLERS {
            fill.arg(key(&format!("fill-{i}"))).arg("pad");
        }
        fill.exec_async(&mut c).await.expect("fillers");

        // Every round goes on from the last one's cursors, as a search that
        // stopped and is continued does.
        let search = |only| {
            let at = at.clone();
            let spec = ValueSearchSpec {
                pattern: format!("{prefix}:*"),
                needle_lower: "needle".to_string(),
                only,
                max_value_bytes: 1024 * 1024,
                max_container_elems: 10_000,
                page_count: 2,
            };
            async move {
                let (mut cursors, mut found, mut scanned, mut rounds) = (None, Vec::new(), 0, 0);
                loop {
                    let round = scan_values_round(&at, &spec, cursors).await.expect("round");
                    rounds += 1;
                    scanned += round.scanned;
                    found.extend(round.matches.into_iter().map(|m| (m.key, m.key_type)));
                    if round.done {
                        break;
                    }
                    cursors = Some(round.cursors);
                }
                found.sort();
                found.dedup();
                (found, scanned, rounds)
            }
        };

        let (found, _, rounds) = search(None).await;
        let expected: Vec<(String, String)> = [
            ("h1", "hash"),
            ("l1", "list"),
            ("s1", "string"),
            ("set1", "set"),
            ("z1", "zset"),
        ]
        .into_iter()
        .map(|(name, t)| (key(name), t.to_string()))
        .collect();
        assert_eq!(
            found, expected,
            "case-insensitive, every readable type, never the stream"
        );
        assert!(rounds > 1, "the search went on from its cursors: {rounds} round(s)");

        let (found, scanned, _) = search(Some(SearchableType::Hash)).await;
        assert_eq!(found, [(key("h1"), "hash".to_string())]);
        if supports(&id, floors::SCAN_TYPE).await {
            assert_eq!(scanned, 1, "the server narrowed the scan to the one hash");
        }

        let (found, _, _) = search(Some(SearchableType::Zset)).await;
        assert_eq!(found, [(key("z1"), "zset".to_string())]);

        let mut names: Vec<String> = ["s1", "s2", "h1", "l1", "set1", "z1", "x1"].map(key).to_vec();
        names.extend((0..FILLERS).map(|i| key(&format!("fill-{i}"))));
        cmd("DEL").arg(&names).exec_async(&mut c).await.expect("cleanup");
    });
}

/// The key tree's scan: every type it wrote comes back named, with its TTL.
/// Shared with the Dragonfly lane (`dragonfly_scan_sees_every_type_it_wrote`),
/// so an assertion added here is one Dragonfly has to meet too — a
/// Redis-only one belongs in a test of its own.
pub(super) async fn scan_sees_every_type_it_wrote(id: String) {
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
        let (cursors, rows) = scan_page(&at, None, &format!("{prefix}:*"), 100, true, None, None)
            .await
            .expect("scan");
        assert_eq!(cursors.iter().sum::<u64>(), 0, "one round covered two keys");
        let mut names: Vec<&str> = rows.iter().map(|(key, _, _)| key.as_str()).collect();
        names.sort();
        assert_eq!(names, [format!("{prefix}:h"), format!("{prefix}:s")]);
        let typed = scan_page(&at, None, &format!("{prefix}:*"), 100, false, Some("hash"), None)
            .await
            .expect("scan")
            .1;
        assert_eq!(typed.len(), 1, "the TYPE filter is the server's: {typed:?}");
        // Filtered with TTLs: the type comes from the filter, the TTL still
        // from the server (a hash with no expiry answers -1).
        let typed = scan_page(&at, None, &format!("{prefix}:*"), 100, true, Some("hash"), None)
            .await
            .expect("scan")
            .1;
        assert_eq!(
            typed,
            [(format!("{prefix}:h"), "hash".to_string(), -1)],
            "type named by the filter, TTL asked of the server"
        );
        // The keyword box read as a regex. `SCAN` has none, so the names are
        // narrowed on this side, and the type and TTL that come back belong
        // to the key that was kept — they are asked after the narrowing.
        let only_hash = Regex::new(&format!("^{}:h$", regex::escape(&prefix))).expect("regex");
        let (cursors, kept) = scan_page(&at, None, &format!("{prefix}:*"), 100, true, None, Some(&only_hash))
            .await
            .expect("scan");
        assert_eq!(cursors.iter().sum::<u64>(), 0, "the cursors are the server's");
        assert_eq!(
            kept,
            [(format!("{prefix}:h"), "hash".to_string(), -1)],
            "one name passes the regex, with its own type and TTL"
        );
        let no_such = Regex::new("^no-such-name$").expect("regex");
        assert!(
            scan_page(&at, None, &format!("{prefix}:*"), 100, true, None, Some(&no_such))
                .await
                .expect("scan")
                .1
                .is_empty(),
            "a regex nothing matches keeps nothing"
        );

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
        persist_key(&at, &format!("{prefix}:s")).await.expect("persist");
        assert_eq!(
            key_type_and_ttl(&at, &format!("{prefix}:s")).await.expect("ttl").1,
            -1,
            "PERSIST drops the expiry"
        );
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
        // The count that precedes a folder delete: the same walk, and a
        // sampled size wherever the server has MEMORY USAGE.
        let impact = count_keys_matching(&at, &format!("{prefix}:*"))
            .await
            .expect("count prefix");
        assert!(impact.complete, "a small prefix is walked in one pass: {impact:?}");
        assert!(impact.keys >= 2, "s and b2 are still there: {impact:?}");
        if supports(&id, floors::MEMORY_USAGE).await {
            assert!(impact.estimated_bytes().is_some_and(|bytes| bytes > 0), "{impact:?}");
        }
        assert!(
            delete_keys_matching(&at, &format!("{prefix}:*"))
                .await
                .expect("del prefix"),
            "a small prefix is deleted in one walk"
        );
        assert!(
            scan_page(&at, None, &format!("{prefix}:*"), 1000, false, None, None)
                .await
                .expect("scan")
                .1
                .is_empty(),
            "the prefix is gone"
        );
        let gone = count_keys_matching(&at, &format!("{prefix}:*"))
            .await
            .expect("count gone");
        assert_eq!((gone.keys, gone.complete, gone.sampled), (0, true, None), "{gone:?}");

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

pub(super) async fn check_batch_ttl_conditions(id: &str) {
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

#[test]
#[ignore]
fn standalone_key_timeline_snapshot_captures_hash_and_missing_key() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;

        let hash_key = unique("timeline-hash");
        let _: () = cmd("HSET")
            .arg(&hash_key)
            .arg("b_field")
            .arg("2")
            .arg("a_field")
            .arg("1")
            .query_async(&mut c)
            .await
            .expect("hset");

        let probe = capture_key_timeline_snapshot(&at, &hash_key)
            .await
            .expect("timeline snapshot");
        assert!(probe.exists);
        assert_eq!(probe.key_type, "hash");
        assert_eq!(probe.snapshot_text, "a_field: 1\nb_field: 2");

        cmd("DEL").arg(&hash_key).exec_async(&mut c).await.expect("del");
        let after_del = capture_key_timeline_snapshot(&at, &hash_key)
            .await
            .expect("timeline snapshot after del");
        assert!(!after_del.exists);
        assert_eq!(after_del.key_type, "none");
    });
}
