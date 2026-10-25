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

//! The module lanes (redis-stack, valkey-bundle): JSON, Search, the
//! probabilistic structures and TimeSeries, each asked for before it is used.

use super::*;

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
        // add of the same item says "already". (Every bloom module has it.)
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
        // valkey-bloom stops at Bloom filters; the rest are RedisBloom's.
        if !has_command(&mut c, "CF.ADD").await {
            eprintln!("skipped: no CF / CMS / TOPK / TDIGEST on this server");
            cmd("DEL").arg(&bf).exec_async(&mut c).await.expect("cleanup");
            return;
        }
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
        if !has_command(&mut c, "TS.CREATE").await {
            eprintln!("skipped: no TimeSeries module on this server");
            return;
        }
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

        // A sample at an explicit timestamp, and one at "now" — so the series
        // spans from 1970 to today and the window has to bucket coarsely.
        assert_eq!(ts_add(&at, &key, Some(1000), 1.5).await.expect("ts.add"), 1000);
        let now_ts = ts_add(&at, &key, None, 2.5).await.expect("ts.add now");
        // What the series viewer reads: the metadata and the samples of the
        // window.
        let window = ts_window(&at, &key, None, 240).await.expect("ts window");
        assert_eq!(window.info.total_samples, 2);
        assert_eq!((window.info.first_ts, window.info.last_ts), (1000, now_ts));
        assert_eq!(window.samples.len(), 2);
        // The property the alignment exists for: a bucketed sample is stamped
        // inside the window that was asked for. Without `ALIGN` the buckets
        // align to timestamp 0, the first one starts decades before the first
        // sample, and this comes back as `(0, 1.5)` — a point plotted at 1970.
        assert!(
            window.samples.iter().all(|(ts, _)| (1000..=now_ts).contains(ts)),
            "every sample belongs to the window {}..={now_ts}: {:?}",
            1000,
            window.samples
        );
        assert_eq!(window.samples.first(), Some(&(1000, 1.5)));
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
        if !has_command(&mut c, "TS.CREATE").await {
            eprintln!("skipped: no TimeSeries module on this server");
            return;
        }
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

        // Both modules say when the backfill is over — RediSearch as
        // `indexing` / `percent_indexed`, valkey-search as
        // `backfill_in_progress` / `backfill_complete_percent`. Valkey's
        // runs for a moment even with nothing to backfill, so the state is
        // waited for rather than read once: that moment is what the panel
        // now shows instead of a ready index answering from half its keys.
        let mut info = ft_info(&at, &index).await.expect("ft.info");
        for _ in 0..50 {
            if !info.indexing {
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(100)).await;
            info = ft_info(&at, &index).await.expect("ft.info");
        }
        assert_eq!(info.num_docs, 2);
        assert!(!info.indexing, "the backfill is over: {info:?}");
        assert_eq!(info.percent_indexed, Some(1.0), "{info:?}");
        // The size fields are RediSearch's (`inverted_sz_mb` and friends);
        // valkey-search's FT.INFO carries none of them.
        if !valkey_lane() {
            assert!(
                info.index_bytes().is_some_and(|bytes| bytes > 0),
                "FT.INFO reports the index's size: {info:?}"
            );
            assert!(
                info.inverted_index_bytes.is_some(),
                "the inverted index is one of the parts"
            );
        }

        // TAGVALS and SPELLCHECK are RediSearch's; valkey-search has neither,
        // and the panel learns that at run time like any missing command.
        if has_command(&mut c, "FT.TAGVALS").await {
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
        } else {
            eprintln!("skipped: no FT.TAGVALS / FT.SPELLCHECK on this server");
        }

        drop_index_and_docs(&mut c, &index, &prefix).await;
    });
}

/// `FT.SEARCH … PARAMS`: a KNN query bound to a FLOAT32 blob encoded by
/// `search_params` ranks the nearest document first, and `FT.EXPLAIN`
/// plans the same query only because the binding travels with it.
///
/// The `AS dist` + `SORTBY` is the whole point of the ordering assertion,
/// not decoration. RediSearch sorts by the *document's* score unless told
/// otherwise — "By default, the results are sorted by their document's
/// score. To sort by vector similarity score, use `SORTBY <distance_field>`"
/// — and with `*` as the filter every document scores the same, so the order
/// was whatever the index happened to yield. It yielded nearest-first for
/// months and then, on a `redis-stack-server:latest` that had moved, the
/// other way round. Asking for the order the assertion checks is the fix;
/// the distances below prove the blob really was bound, which is what an
/// order alone would not.
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

        let query = "*=>[KNN 2 @v $BLOB AS dist]";
        let params = vec![(
            "BLOB".to_string(),
            encode_param(ParamKind::Float32, "0.9, 0.1").expect("encode"),
        )];
        let opts = SearchOptions {
            limit: (0, 10),
            dialect: Some(2),
            params: params.clone(),
            // valkey-search answers a KNN query nearest-first by itself and
            // refuses SORTBY on the alias; RediSearch needs to be asked.
            sort_by: (!valkey_lane()).then(|| "dist".to_string()),
            ..Default::default()
        };
        let result = ft_search(&at, &index, query, &opts).await.expect("ft.search");
        assert_eq!(result.total, 2);
        assert_eq!(
            result.hits.first().map(|h| h.doc_id.as_str()),
            Some(format!("{prefix}:a").as_str()),
            "(0.9, 0.1) is nearest to (1, 0)"
        );
        // The distance itself, which is what says the blob arrived as a
        // vector rather than as some other two numbers. Compared, never
        // asserted to a value: `L2` has been the squared distance in some
        // RediSearch versions and the root in others, and either way `a` is
        // the smaller of the two.
        let distance = |name: &str| -> f64 {
            result
                .hits
                .iter()
                .find(|h| h.doc_id == format!("{prefix}:{name}"))
                .unwrap_or_else(|| panic!("{name} is in the hits: {:?}", result.hits))
                .fields
                .iter()
                .find(|(k, _)| k == "dist")
                .unwrap_or_else(|| panic!("{name} carries the aliased distance: {:?}", result.hits))
                .1
                .parse()
                .expect("the distance is a number")
        };
        let (near, far) = (distance("a"), distance("b"));
        assert!(near < far, "(1, 0) is nearer than (0, 1): {near} vs {far}");
        // EXPLAIN is RediSearch's; valkey-search has no plan to show.
        if has_command(&mut c, "FT.EXPLAIN").await {
            let plan = ft_explain(&at, &index, query, &params, Some(2))
                .await
                .expect("ft.explain");
            assert!(plan.contains("VECTOR"), "plan names the vector iterator: {plan}");
            assert!(
                ft_explain(&at, &index, query, &[], Some(2)).await.is_err(),
                "without the binding the server refuses to plan"
            );
        } else {
            eprintln!("skipped: no FT.EXPLAIN on this server");
        }

        drop_index_and_docs(&mut c, &index, &prefix).await;
    });
}

/// The JSON editor reads and writes a document whole: `JSON.GET` hands back
/// what `JSON.SET` stored, a second set replaces it, and `JSON.MERGE` writes
/// only what changed — RFC 7396, where a `null` removes a member.
#[test]
#[ignore]
fn stack_json_document_is_read_written_whole_and_merged() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack-json-whole", standalone())).await;
        let mut c = conn(&id, 0).await;
        if !has_command(&mut c, "JSON.SET").await {
            eprintln!("skipped: no JSON module on this server");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let key = unique("json-whole");
        let read = async || -> serde_json::Value {
            let text = json_get(&at, &key).await.expect("json.get");
            serde_json::from_str(&text).expect("a JSON document")
        };

        json_set(&at, &key, r#"{"name":"zedis","tags":["a"],"n":1}"#)
            .await
            .expect("json.set");
        assert_eq!(
            read().await,
            serde_json::json!({"name": "zedis", "tags": ["a"], "n": 1})
        );

        if has_command(&mut c, "JSON.MERGE").await {
            json_merge(&at, &key, r#"{"n":2,"tags":null,"extra":true}"#)
                .await
                .expect("json.merge");
            assert_eq!(
                read().await,
                serde_json::json!({"name": "zedis", "n": 2, "extra": true}),
                "a null removes, the rest replaces or adds"
            );
        } else {
            eprintln!("skipped JSON.MERGE: not in this module");
        }

        json_set(&at, &key, "[1,2]").await.expect("json.set again");
        assert_eq!(read().await, serde_json::json!([1, 2]), "the whole document replaced");

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// An index from the search panel's own forms, start to end: `FT.CREATE`
/// from a schema, `FT.ALTER` adding a field, `FT.AGGREGATE` grouping by a
/// tag, `FT.PROFILE` timing a query and `FT.DROPINDEX` — with its documents
/// on RediSearch, and on valkey-search, which has no `DD`, refused in words
/// that say so and then without them.
#[test]
#[ignore]
fn stack_search_index_is_created_altered_aggregated_and_dropped() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack-ft-lifecycle", standalone())).await;
        let mut c = conn(&id, 0).await;
        if !has_command(&mut c, "FT.CREATE").await {
            eprintln!("skipped: no search module on this server");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let index = unique("idx-life");
        let prefix = unique("doc-life");
        let field = |name: &str, field_type: &str, sortable: bool| CreateFieldSpec {
            name: name.to_string(),
            field_type: field_type.to_string(),
            sortable,
            ..Default::default()
        };
        ft_create(
            &at,
            &CreateIndexOptions {
                index: index.clone(),
                on_json: false,
                prefixes: vec![format!("{prefix}:")],
                // SORTABLE, so the grouping below reads the tag without a
                // LOAD on any RediSearch: the panel sends none. valkey-search
                // reads only what `LOAD` put in the working set — one bucket,
                // `count 2`, no `kind` — which is why `ft_aggregate` LOADs
                // the grouped fields on Valkey; this test is what holds it to
                // that on the valkey-bundle lane.
                fields: vec![field("title", "TEXT", false), field("kind", "TAG", true)],
            },
        )
        .await
        .expect("ft.create");
        for (name, title, kind, n) in [
            ("a", "hello world", "red", 1),
            ("b", "hello again", "blue", 2),
            ("c", "goodbye", "red", 3),
        ] {
            cmd("HSET")
                .arg(format!("{prefix}:{name}"))
                .arg("title")
                .arg(title)
                .arg("kind")
                .arg(kind)
                .arg("n")
                .arg(n)
                .exec_async(&mut c)
                .await
                .expect("hset");
        }
        let mut info = ft_info(&at, &index).await.expect("ft.info");
        for _ in 0..50 {
            if !info.indexing && info.num_docs == 3 {
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(100)).await;
            info = ft_info(&at, &index).await.expect("ft.info");
        }
        assert_eq!(info.num_docs, 3, "the schema indexed every document: {info:?}");

        // A field added later is searchable on what is written after it.
        if has_command(&mut c, "FT.ALTER").await {
            ft_alter_add(&at, &index, &field("n", "NUMERIC", true))
                .await
                .expect("ft.alter");
            cmd("HSET")
                .arg(format!("{prefix}:d"))
                .arg("title")
                .arg("later")
                .arg("kind")
                .arg("green")
                .arg("n")
                .arg(50)
                .exec_async(&mut c)
                .await
                .expect("hset");
            let opts = SearchOptions {
                limit: (0, 10),
                ..Default::default()
            };
            let mut hits = Vec::new();
            for _ in 0..50 {
                hits = ft_search(&at, &index, "@n:[50 50]", &opts)
                    .await
                    .expect("ft.search")
                    .hits
                    .into_iter()
                    .map(|hit| hit.doc_id)
                    .collect();
                if !hits.is_empty() {
                    break;
                }
                smol::Timer::after(std::time::Duration::from_millis(100)).await;
            }
            assert_eq!(hits, [format!("{prefix}:d")], "the added NUMERIC field answers a range");
        } else {
            eprintln!("skipped FT.ALTER: not in this module");
        }

        if has_command(&mut c, "FT.AGGREGATE").await {
            let opts = AggregateOptions {
                group_by: vec!["kind".to_string()],
                reducer: Some(ReducerSpec {
                    func: Some(ReducerFn::Count),
                    args: Vec::new(),
                    alias: Some("count".to_string()),
                }),
                ..Default::default()
            };
            let result = ft_aggregate(&at, &index, "@title:hello", &opts)
                .await
                .expect("ft.aggregate");
            let mut groups: Vec<(String, String)> = result
                .rows
                .iter()
                .map(|row| {
                    let cell = |name: &str| {
                        row.iter()
                            .find(|(field, _)| field == name)
                            .map(|(_, value)| value.clone())
                            .unwrap_or_default()
                    };
                    (cell("kind"), cell("count"))
                })
                .collect();
            groups.sort();
            assert_eq!(
                groups,
                [
                    ("blue".to_string(), "1".to_string()),
                    ("red".to_string(), "1".to_string())
                ],
                "one row per tag among the matches, counted: {:?}",
                result.rows
            );
        } else {
            eprintln!("skipped FT.AGGREGATE: not in this module");
        }

        if has_command(&mut c, "FT.PROFILE").await {
            for aggregate in [false, true] {
                let profile = ft_profile(&at, &index, aggregate, "hello", &[], None)
                    .await
                    .expect("ft.profile");
                assert!(!profile.trim().is_empty(), "the profile section, as text");
            }
        } else {
            eprintln!("skipped FT.PROFILE: not in this module");
        }

        let docs = || {
            cmd("EXISTS")
                .arg(format!("{prefix}:a"))
                .arg(format!("{prefix}:b"))
                .clone()
        };
        if valkey_lane() {
            let refused = ft_dropindex(&at, &index, true)
                .await
                .expect_err("valkey-search has no DD");
            assert!(refused.to_string().contains("no DD"), "said in our words: {refused}");
            assert!(ft_info(&at, &index).await.is_ok(), "and nothing was dropped");
            ft_dropindex(&at, &index, false).await.expect("ft.dropindex");
            let left: i64 = docs().query_async(&mut c).await.expect("exists");
            assert_eq!(left, 2, "the documents stay");
            let keys: Vec<String> = cmd("KEYS")
                .arg(format!("{prefix}:*"))
                .query_async(&mut c)
                .await
                .expect("keys");
            cmd("DEL").arg(keys).exec_async(&mut c).await.expect("del docs");
        } else {
            ft_dropindex(&at, &index, true).await.expect("ft.dropindex dd");
            let left: i64 = docs().query_async(&mut c).await.expect("exists");
            assert_eq!(left, 0, "DD took the documents with the index");
        }
        assert!(ft_info(&at, &index).await.is_err(), "the index is gone");
    });
}
