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

//! Each value type's operations: the collections, streams, geo, HyperLogLog,
//! bitmaps, vector sets and hash-field TTLs.

use super::*;

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
            vec![Some(0x0f)]
        );
        assert_eq!(
            bit_field(&at, &bits_a, &args("SET u4 0 15 GET u8 0"))
                .await
                .expect("bitfield set"),
            vec![Some(0), Some(0xff)],
            "one integer per sub-command: the old u4, then the new u8"
        );
        // OVERFLOW FAIL refuses the increment past u8 with a nil, and the
        // rest of the command still answers.
        assert_eq!(
            bit_field(&at, &bits_a, &args("OVERFLOW FAIL INCRBY u8 0 1 GET u8 0"))
                .await
                .expect("an overflow is an answer, not an error"),
            vec![None, Some(0xff)]
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
        // Compared as ids, not as text: in the same millisecond the next id is
        // `…-10`, which sorts before `…-9` as a string.
        let as_id = |id: &str| -> (u64, u64) {
            let (ms, seq) = id.split_once('-').expect("an id");
            (ms.parse().expect("ms"), seq.parse().expect("seq"))
        };
        assert!(
            as_id(&next) > as_id(&raised),
            "the next id follows the one just set: {next}"
        );

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

/// The collection editors' paging and writes (ADR 10): every one of these
/// sat inline in `src/states` and so had no test at all until it moved.
/// Bytes are kept as the server answered them — a member that is not UTF-8
/// is a row to draw, not a row to drop.
#[test]
#[ignore]
fn standalone_collections_page_and_write_through_their_operations() {
    smol::block_on(async {
        let id = register(server("it-collections", standalone())).await;
        collections_page_and_write_through_their_operations(id).await;
    });
}

/// The list, set, hash and sorted-set operations the editors are built on.
/// Shared with the Dragonfly lane, on the same terms as
/// [`scan_sees_every_type_it_wrote`].
pub(super) async fn collections_page_and_write_through_their_operations(id: String) {
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
    // One command for the whole block. `RPUSH` keeps the given order.
    // `LPUSH` inserts right to left, so the caller passes that block
    // reversed and the head still reads in the entered order.
    let block = format!("{prefix}:list-block");
    assert_eq!(
        list_push_many(&at, &block, &[b"a", b"b", b"c"], false)
            .await
            .expect("rpush block"),
        3
    );
    assert_eq!(
        list_range(&at, &block, 0, 2).await.expect("lrange block"),
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
    );
    assert_eq!(
        list_push_many(&at, &block, &[b"c", b"b", b"a"], true)
            .await
            .expect("lpush block"),
        6
    );
    assert_eq!(
        list_range(&at, &block, 0, 2).await.expect("lrange head"),
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
    );
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
        // Writing it again without one takes the TTL away: `HSET`
        // discards it, which is why `Persist` sends nothing extra.
        write_hash_field(&at, &hash, b"f1", b"v1", FieldTtl::Persist, false)
            .await
            .expect("hset without ttl");
        assert_eq!(
            hash_field_ttls(&at, &hash, &[b"f1"]).await.expect("httl")[0],
            -1,
            "the plain write dropped the TTL"
        );
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
    // The preview of a value too large to load: its first bytes and its
    // whole length; a shorter string whole, a missing one empty.
    assert_eq!(
        string_prefix(&at, &string, 1).await.expect("prefix"),
        (binary[..1].to_vec(), binary.len() as u64)
    );
    assert_eq!(
        string_prefix(&at, &string, 64).await.expect("prefix"),
        (binary.to_vec(), binary.len() as u64)
    );
    assert_eq!(
        string_prefix(&at, &format!("{prefix}:no-such-string"), 8)
            .await
            .expect("missing"),
        (Vec::new(), 0)
    );
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
        let jumped = stream_entry(&at, &key, &first[0].0).await.expect("xrange one id");
        assert_eq!(jumped.as_ref().map(|(id, _)| id.as_str()), Some(first[0].0.as_str()));
        assert_eq!(stream_entry(&at, &key, "0-1").await.expect("missing id"), None);
        assert_eq!(first[0].1, vec![("n".to_string(), b"1".to_vec())]);
        assert!(!cursor.is_empty(), "more to come");
        let (next, rest) = stream_page(&at, &key, Some(&cursor), 2, false).await.expect("xrange");
        assert_eq!(rest.len(), 1, "the cursor is exclusive");
        assert!(next.is_empty(), "the end of the stream");
        // A binary field value does not fail the page, and arrives as the
        // bytes that were stored — what a decoder needs.
        let binary_key = format!("{key}:binary");
        cmd("XADD")
            .arg(&binary_key)
            .arg("*")
            .arg("payload")
            .arg(b"\x80\xffok".as_slice())
            .exec_async(&mut c)
            .await
            .expect("xadd binary");
        let (_, binary) = stream_page(&at, &binary_key, None, 10, false)
            .await
            .expect("a page with a non-UTF-8 value");
        assert_eq!(binary.len(), 1);
        assert_eq!(binary[0].1[0].0, "payload");
        assert_eq!(binary[0].1[0].1, b"\x80\xffok".to_vec(), "{:?}", binary[0].1);
        cmd("DEL").arg(&binary_key).exec_async(&mut c).await.expect("cleanup");

        let (_, newest) = stream_page(&at, &key, None, 1, true).await.expect("xrevrange");
        assert_eq!(
            newest[0].1,
            vec![("n".to_string(), b"3".to_vec())],
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
        if supports(&id, floors::STREAM_GROUP_LAG).await {
            assert_eq!(described.lag, Some(1), "one entry nobody has been delivered");
        } else {
            assert_eq!(
                described.lag, None,
                "a server without the field has no lag to report, not a 0"
            );
        }
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
        assert_eq!(
            stream_ack_many(&at, &key, group, &[]).await.expect("empty xack"),
            0,
            "empty batch ack is a no-op"
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

        // The entries are written while the tail is blocked, and that is not
        // incidental: `XREAD … $` resolves `$` when the command is *sent*, so
        // anything written beforehand is already in the past and a tail that
        // reads after the writes sees nothing. The concurrency is what
        // exercises the cursor at all.
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
                ("n".to_string(), n.as_bytes().to_vec()),
                ("name".to_string(), b"zedis".to_vec()),
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

/// One field's TTL set and taken away on its own — the hash row menu's Set
/// field TTL… / Remove field TTL (#225): the value is left alone, a field
/// without a TTL is still there, and one deleted since it was listed is
/// answered as gone. Standalone only: the shared collection body runs on
/// Dragonfly and Garnet, where `HPERSIST` has not been checked.
#[test]
#[ignore]
fn standalone_a_hash_field_ttl_is_set_and_taken_away_alone() {
    smol::block_on(async {
        let id = register(server("it-hash-field-ttl", standalone())).await;
        if !supports(&id, floors::HASH_FIELD_TTL).await {
            eprintln!("skipped: server has no hash field TTL");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let hash = unique("hfttl");
        write_hash_field(&at, &hash, b"f1", b"v1", FieldTtl::Persist, false)
            .await
            .expect("hset");

        assert!(set_hash_field_ttl(&at, &hash, b"f1", Some(300)).await.expect("hexpire"));
        let ttl = hash_field_ttls(&at, &hash, &[b"f1"]).await.expect("httl")[0];
        assert!((1..=300).contains(&ttl), "{ttl}");
        let (_, pairs) = hash_scan(&at, &hash, Some("f1"), 0, 100).await.expect("hscan");
        assert_eq!(pairs, [(b"f1".to_vec(), b"v1".to_vec())], "the value is left alone");

        assert!(set_hash_field_ttl(&at, &hash, b"f1", None).await.expect("hpersist"));
        assert_eq!(hash_field_ttls(&at, &hash, &[b"f1"]).await.expect("httl")[0], -1);
        assert!(
            set_hash_field_ttl(&at, &hash, b"f1", None)
                .await
                .expect("hpersist again"),
            "a field without a TTL is still there"
        );
        assert!(
            !set_hash_field_ttl(&at, &hash, b"gone", Some(60))
                .await
                .expect("hexpire gone")
        );
        assert!(
            !set_hash_field_ttl(&at, &hash, b"gone", None)
                .await
                .expect("hpersist gone")
        );

        assert_eq!(hash_delete_fields(&at, &hash, &[b"f1"]).await.expect("hdel"), 1);
    });
}

/// The pending list's two newer settlements, each on the Redis that has it:
/// `XACKDEL` (8.2) acknowledges an entry and deletes it from the stream in
/// one step, and `XNACK` (8.8) hands one back to its group unowned — still
/// pending, its delivery still counted, its consumer cleared. Valkey has
/// neither.
#[test]
#[ignore]
fn standalone_stream_ack_delete_and_nack_settle_one_pending_entry() {
    smol::block_on(async {
        let id = register(server("it-stream-settle", standalone())).await;
        if !supports(&id, floors::STREAM_REF_POLICIES).await {
            eprintln!("skipped: XACKDEL is Redis 8.2+");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("stream-settle");
        let mut ids = Vec::new();
        for n in 1..=3 {
            ids.push(
                stream_add(&at, &key, "*", &[("n".to_string(), n.to_string())])
                    .await
                    .expect("xadd"),
            );
        }
        let group = "g1";
        group_create(&at, &key, group, "0").await.expect("xgroup create");
        let _: redis::Value = cmd("XREADGROUP")
            .arg("GROUP")
            .arg(group)
            .arg("c1")
            .arg("COUNT")
            .arg(3)
            .arg("STREAMS")
            .arg(&key)
            .arg(">")
            .query_async(&mut c)
            .await
            .expect("xreadgroup");
        let pending_ids = async || -> Vec<String> {
            pending_page(&at, &key, group, "-")
                .await
                .expect("xpending")
                .into_iter()
                .map(|entry| entry.id)
                .collect()
        };

        stream_ack_delete(&at, &key, group, &ids[0]).await.expect("xackdel");
        assert_eq!(stream_len(&at, &key).await.expect("xlen"), 2, "deleted from the stream");
        assert_eq!(pending_ids().await, ids[1..], "and acknowledged");

        if supports(&id, floors::STREAM_NACK).await {
            stream_nack(&at, &key, group, &ids[1]).await.expect("xnack");
            assert_eq!(pending_ids().await, ids[1..], "a released entry is still pending");
            let pending = pending_page(&at, &key, group, "-").await.expect("xpending");
            let released = pending.iter().find(|entry| entry.id == ids[1]).expect("released");
            assert_eq!(
                (released.consumer.as_str(), released.delivery_count),
                ("", 1),
                "owned by nobody, its delivery still counted"
            );
            let info = stream_info(&at, &key).await.expect("xinfo");
            assert_eq!(
                info.groups[0].consumers[0].pending, 1,
                "the consumer holds only the other one"
            );
        } else {
            eprintln!("skipped the XNACK half: it is Redis 8.8+");
        }

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}
