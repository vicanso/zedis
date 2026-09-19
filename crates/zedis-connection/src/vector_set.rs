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

//! Vector sets (Redis 8): what their viewer shows — the set's shape, a sample
//! of its elements, and similarity search around one of them.

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Cmd, Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything a `VSIM` run needs beyond the element: the COUNT, the optional
/// `FILTER` expression with its `FILTER-EF` candidate budget, and whether the
/// server understands `WITHATTRIBS` (Redis 8.2+).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorSimOptions {
    pub count: i64,
    pub filter: Option<String>,
    pub filter_ef: Option<i64>,
    pub with_attribs: bool,
}

/// One `VSIM` hit: the element, its similarity and — with `WITHATTRIBS` — its
/// attribute JSON, so a filtered result shows why it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorNeighbour {
    pub element: String,
    pub score: f64,
    pub attrs: Option<String>,
}

/// One KNN round: the ranked neighbours plus the queried element's own
/// attributes (`VGETATTR`) and dequantized vector (`VEMB`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorSim {
    pub neighbours: Vec<VectorNeighbour>,
    pub attrs: Option<String>,
    pub vector: Option<Vec<f64>>,
}

/// A vector set as its viewer opens it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorSetInfo {
    /// `VINFO` as field → value rows.
    pub info: Vec<(String, String)>,
    pub card: i64,
    pub dim: i64,
    /// `VRANDMEMBER`: a few elements to start from.
    pub sample: Vec<String>,
    /// The similarity search around `sample[0]`, so the neighbour panel is
    /// not empty on arrival. `None` when the sample is empty or the search
    /// failed — decoration, never a reason to fail the load.
    pub first: Option<VectorSim>,
}

/// `VINFO` + `VCARD` + `VDIM` + a `VRANDMEMBER` sample of `sample_cap`, and
/// the neighbours of the first sampled element. Only `VINFO` is fatal.
pub async fn vset_info(at: &ServerDb, key: &str, sample_cap: i64, sim: &VectorSimOptions) -> Result<VectorSetInfo> {
    let mut conn = at.connection().await?;
    let info_raw: Value = cmd("VINFO").arg(key).query_async(&mut conn).await?;
    let card: i64 = cmd("VCARD").arg(key).query_async(&mut conn).await.unwrap_or(0);
    let dim: i64 = cmd("VDIM").arg(key).query_async(&mut conn).await.unwrap_or(0);
    let sample: Vec<String> = cmd("VRANDMEMBER")
        .arg(key)
        .arg(sample_cap)
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    let first = match sample.first() {
        Some(element) => run_vsim(&mut conn, key, element, sim).await.ok(),
        None => None,
    };
    Ok(VectorSetInfo {
        info: reply::display_pairs(&info_raw),
        card,
        dim,
        sample,
        first,
    })
}

/// The neighbours of `element`, with its attributes and vector.
pub async fn vset_sim(at: &ServerDb, key: &str, element: &str, sim: &VectorSimOptions) -> Result<VectorSim> {
    run_vsim(&mut at.connection().await?, key, element, sim).await
}

/// `VREM key element`. `true` when the element was there.
pub async fn vset_remove(at: &ServerDb, key: &str, element: &str) -> Result<bool> {
    let removed: i64 = cmd("VREM")
        .arg(key)
        .arg(element)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(removed != 0)
}

/// `VSETATTR key element json` — an empty string clears the attributes.
/// `true` when the element exists.
pub async fn vset_set_attr(at: &ServerDb, key: &str, element: &str, json: &str) -> Result<bool> {
    let updated: i64 = cmd("VSETATTR")
        .arg(key)
        .arg(element)
        .arg(json)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(updated != 0)
}

/// `VGETATTR key element` — `None` for no attributes (nil reply) or any
/// error (attrs are decoration; a failed read must not fail the search).
async fn fetch_attrs(conn: &mut RedisAsyncConn, key: &str, element: &str) -> Option<String> {
    cmd("VGETATTR")
        .arg(key)
        .arg(element)
        .query_async::<Option<String>>(conn)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// `VEMB key element` — the stored vector as the server dequantizes it.
/// Decoration like the attributes: any failure just hides the row.
async fn fetch_vector(conn: &mut RedisAsyncConn, key: &str, element: &str) -> Option<Vec<f64>> {
    let raw: Value = cmd("VEMB").arg(key).arg(element).query_async(conn).await.ok()?;
    let Value::Array(items) = raw else {
        return None;
    };
    let components: Vec<f64> = items.iter().filter_map(reply::float).collect();
    (!components.is_empty()).then_some(components)
}

/// `VSIM key ELE elem WITHSCORES [WITHATTRIBS] COUNT n [FILTER expr
/// [FILTER-EF n]]` — FILTER-EF only means something next to a FILTER.
fn vsim_cmd(key: &str, element: &str, opts: &VectorSimOptions) -> Cmd {
    let mut c = cmd("VSIM");
    c.arg(key).arg("ELE").arg(element).arg("WITHSCORES");
    if opts.with_attribs {
        c.arg("WITHATTRIBS");
    }
    c.arg("COUNT").arg(opts.count);
    if let Some(filter) = &opts.filter {
        c.arg("FILTER").arg(filter.as_str());
        if let Some(ef) = opts.filter_ef {
            c.arg("FILTER-EF").arg(ef);
        }
    }
    c
}

/// One KNN round on an open connection.
async fn run_vsim(conn: &mut RedisAsyncConn, key: &str, element: &str, opts: &VectorSimOptions) -> Result<VectorSim> {
    let raw: Value = vsim_cmd(key, element, opts).query_async(conn).await?;
    Ok(VectorSim {
        neighbours: parse_neighbours(&raw, opts.with_attribs),
        attrs: fetch_attrs(conn, key, element).await,
        vector: fetch_vector(conn, key, element).await,
    })
}

/// Parse a `WITHSCORES [WITHATTRIBS]` reply. RESP3 is a map from element to
/// either the score or `[score, attrs]`; RESP2 is a flat array of pairs, or
/// of triples with `WITHATTRIBS` (nil for an element without attributes).
fn parse_neighbours(value: &Value, with_attribs: bool) -> Vec<VectorNeighbour> {
    let attrs_of = |v: Option<&Value>| v.and_then(reply::text_lossy).filter(|s| !s.is_empty());
    match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| {
                let element = reply::text_lossy(k)?;
                let (score, attrs) = match v {
                    Value::Array(items) => (reply::float(items.first()?)?, attrs_of(items.get(1))),
                    scalar => (reply::float(scalar)?, None),
                };
                Some(VectorNeighbour { element, score, attrs })
            })
            .collect(),
        Value::Array(items) => {
            let stride = if with_attribs { 3 } else { 2 };
            items
                .chunks(stride)
                .filter_map(|chunk| {
                    Some(VectorNeighbour {
                        element: reply::text_lossy(chunk.first()?)?,
                        score: reply::float(chunk.get(1)?)?,
                        attrs: attrs_of(chunk.get(2)),
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Arg;

    fn words(c: &Cmd) -> Vec<String> {
        c.args_iter()
            .map(|a| match a {
                Arg::Simple(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => String::new(),
            })
            .collect()
    }

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn vsim_cmd_spells_filter_and_attribs() {
        let opts = VectorSimOptions {
            count: 5,
            filter: Some(".year > 2000".to_string()),
            filter_ef: Some(500),
            with_attribs: true,
        };
        assert_eq!(
            words(&vsim_cmd("k", "e", &opts)),
            [
                "VSIM",
                "k",
                "ELE",
                "e",
                "WITHSCORES",
                "WITHATTRIBS",
                "COUNT",
                "5",
                "FILTER",
                ".year > 2000",
                "FILTER-EF",
                "500"
            ]
        );
        let plain = VectorSimOptions {
            count: 10,
            ..Default::default()
        };
        assert_eq!(
            words(&vsim_cmd("k", "e", &plain)),
            ["VSIM", "k", "ELE", "e", "WITHSCORES", "COUNT", "10"]
        );
        // FILTER-EF without a FILTER is meaningless — never sent alone.
        let ef_only = VectorSimOptions {
            count: 10,
            filter_ef: Some(9),
            ..Default::default()
        };
        assert!(!words(&vsim_cmd("k", "e", &ef_only)).iter().any(|w| w == "FILTER-EF"));
    }

    #[test]
    fn neighbours_parse_both_transports() {
        // RESP2 triples; nil attrs for an element that has none.
        let resp2 = Value::Array(vec![bs("a"), bs("1"), bs(r#"{"y":1}"#), bs("b"), bs("0.5"), Value::Nil]);
        let n = parse_neighbours(&resp2, true);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].attrs.as_deref(), Some(r#"{"y":1}"#));
        assert_eq!(n[1].attrs, None);
        assert_eq!(n[1].score, 0.5);
        // RESP3 map: element → [score, attrs].
        let resp3 = Value::Map(vec![
            (bs("a"), Value::Array(vec![Value::Double(1.0), bs(r#"{"y":1}"#)])),
            (bs("b"), Value::Array(vec![Value::Double(0.5), Value::Nil])),
        ]);
        let n = parse_neighbours(&resp3, true);
        assert_eq!(n[0].attrs.as_deref(), Some(r#"{"y":1}"#));
        assert_eq!(n[1].attrs, None);
        // Without WITHATTRIBS: pairs, or plain scores in the map.
        let n = parse_neighbours(&Value::Array(vec![bs("a"), bs("0.9")]), false);
        assert_eq!((n[0].element.as_str(), n[0].score), ("a", 0.9));
        let n = parse_neighbours(&Value::Map(vec![(bs("a"), Value::Double(0.9))]), false);
        assert_eq!(n[0].score, 0.9);
        assert_eq!(n[0].attrs, None);
    }
}
