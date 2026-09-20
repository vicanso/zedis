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

//! RedisBloom's probabilistic structures — Bloom and Cuckoo filters, Count-Min
//! Sketch, Top-K, t-digest: what their viewer shows and its one probe box.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Which structure a key is, from the module type string `TYPE` returns.
/// Carried inside the app's `KeyType::Probabilistic` so a single key-type arm
/// fans out to the right viewer and commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbKind {
    Bloom,
    Cuckoo,
    CountMinSketch,
    TopK,
    TDigest,
}

impl ProbKind {
    /// Command prefix / short label (`BF`, `CF`, `CMS`, `TOPK`, `TDIGEST`).
    pub fn prefix(&self) -> &'static str {
        match self {
            ProbKind::Bloom => "BF",
            ProbKind::Cuckoo => "CF",
            ProbKind::CountMinSketch => "CMS",
            ProbKind::TopK => "TOPK",
            ProbKind::TDigest => "TDIGEST",
        }
    }
}

/// A probabilistic key as its viewer shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbInfo {
    /// The `*.INFO` reply as field → value rows.
    pub info: Vec<(String, String)>,
    /// Top-K only: `(item, count)` from `TOPK.LIST … WITHCOUNT`.
    pub top_items: Vec<(String, i64)>,
    /// t-digest only: `min`, `max`, `p50`, `p90`, `p99` — those that are
    /// finite (an empty digest answers `nan`).
    pub quantiles: Vec<(&'static str, f64)>,
}

/// What a probe — a query, or an add — learned. The app words it.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// Bloom / Cuckoo positive: probabilistic, may be a false positive.
    MaybeExists,
    /// Bloom / Cuckoo negative: definitive.
    DefinitelyNot,
    /// CMS estimate (query, or the new estimate after INCRBY).
    Count(i64),
    InTopK,
    NotInTopK,
    /// `TDIGEST.CDF` — fraction of samples ≤ the probed value.
    Cdf(f64),
    Added,
    /// `BF.ADD` returned 0 — the filter thinks it was already there.
    AlreadyMaybe,
    /// `TOPK.ADD` pushed this item out of the list.
    TopkDropped(String),
}

/// One probe round-trip. A query answers the structure's defining question;
/// an add inserts (`CMS.INCRBY … 1` for the sketch — its "add" is a count
/// increment by definition).
pub async fn prob_probe(at: &ServerDb, key: &str, kind: ProbKind, item: &str, add: bool) -> Result<ProbeOutcome> {
    let mut conn = at.connection().await?;
    let outcome = match (kind, add) {
        (ProbKind::Bloom, false) | (ProbKind::Cuckoo, false) => {
            let exists: i64 = cmd(&format!("{}.EXISTS", kind.prefix()))
                .arg(key)
                .arg(item)
                .query_async(&mut conn)
                .await?;
            if exists == 1 {
                ProbeOutcome::MaybeExists
            } else {
                ProbeOutcome::DefinitelyNot
            }
        }
        (ProbKind::Bloom, true) => {
            let added: i64 = cmd("BF.ADD").arg(key).arg(item).query_async(&mut conn).await?;
            if added == 1 {
                ProbeOutcome::Added
            } else {
                ProbeOutcome::AlreadyMaybe
            }
        }
        (ProbKind::Cuckoo, true) => {
            let _: i64 = cmd("CF.ADD").arg(key).arg(item).query_async(&mut conn).await?;
            ProbeOutcome::Added
        }
        (ProbKind::CountMinSketch, false) => {
            let counts: Vec<i64> = cmd("CMS.QUERY").arg(key).arg(item).query_async(&mut conn).await?;
            ProbeOutcome::Count(counts.first().copied().unwrap_or(0))
        }
        (ProbKind::CountMinSketch, true) => {
            let counts: Vec<i64> = cmd("CMS.INCRBY")
                .arg(key)
                .arg(item)
                .arg(1)
                .query_async(&mut conn)
                .await?;
            ProbeOutcome::Count(counts.first().copied().unwrap_or(0))
        }
        (ProbKind::TopK, false) => {
            let hits: Vec<i64> = cmd("TOPK.QUERY").arg(key).arg(item).query_async(&mut conn).await?;
            if hits.first().copied().unwrap_or(0) == 1 {
                ProbeOutcome::InTopK
            } else {
                ProbeOutcome::NotInTopK
            }
        }
        (ProbKind::TopK, true) => {
            let dropped: Vec<Option<String>> = cmd("TOPK.ADD").arg(key).arg(item).query_async(&mut conn).await?;
            match dropped.into_iter().next().flatten() {
                Some(evicted) => ProbeOutcome::TopkDropped(evicted),
                None => ProbeOutcome::Added,
            }
        }
        (ProbKind::TDigest, false) => {
            let fractions: Vec<f64> = cmd("TDIGEST.CDF").arg(key).arg(item).query_async(&mut conn).await?;
            ProbeOutcome::Cdf(fractions.first().copied().unwrap_or(f64::NAN))
        }
        (ProbKind::TDigest, true) => {
            let _: () = cmd("TDIGEST.ADD").arg(key).arg(item).query_async(&mut conn).await?;
            ProbeOutcome::Added
        }
    };
    Ok(outcome)
}

/// The `*.INFO` stats plus the per-kind extras (the Top-K list, the t-digest
/// quantiles). Only `*.INFO` is fatal; the extras are best-effort.
pub async fn prob_info(at: &ServerDb, key: &str, kind: ProbKind) -> Result<ProbInfo> {
    let mut conn = at.connection().await?;
    let info_raw: Value = cmd(&format!("{}.INFO", kind.prefix()))
        .arg(key)
        .query_async(&mut conn)
        .await?;
    let mut data = ProbInfo {
        info: reply::display_pairs(&info_raw),
        ..Default::default()
    };
    match kind {
        ProbKind::TopK => {
            if let Ok(raw) = cmd("TOPK.LIST")
                .arg(key)
                .arg("WITHCOUNT")
                .query_async::<Value>(&mut conn)
                .await
            {
                data.top_items = parse_topk_list(&raw);
            }
        }
        ProbKind::TDigest => {
            let finite = |v: f64| v.is_finite().then_some(v);
            if let Some(v) = cmd("TDIGEST.MIN")
                .arg(key)
                .query_async::<f64>(&mut conn)
                .await
                .ok()
                .and_then(finite)
            {
                data.quantiles.push(("min", v));
            }
            if let Some(v) = cmd("TDIGEST.MAX")
                .arg(key)
                .query_async::<f64>(&mut conn)
                .await
                .ok()
                .and_then(finite)
            {
                data.quantiles.push(("max", v));
            }
            if let Ok(values) = cmd("TDIGEST.QUANTILE")
                .arg(key)
                .arg(0.5)
                .arg(0.9)
                .arg(0.99)
                .query_async::<Vec<f64>>(&mut conn)
                .await
            {
                for (label, value) in ["p50", "p90", "p99"].into_iter().zip(values) {
                    if let Some(value) = finite(value) {
                        data.quantiles.push((label, value));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(data)
}

/// `TOPK.LIST key WITHCOUNT`: a flat `[item, count, …]` array. A pair that
/// cannot be read is skipped.
fn parse_topk_list(value: &Value) -> Vec<(String, i64)> {
    let Value::Array(items) = value else {
        return Vec::new();
    };
    items
        .chunks(2)
        .filter_map(|chunk| Some((reply::text_lossy(chunk.first()?)?, reply::int(chunk.get(1)?)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_topk_list_is_read_in_pairs_and_a_broken_pair_is_skipped() {
        let bulk = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let list = Value::Array(vec![
            bulk("a"),
            Value::Int(9),
            bulk("b"),
            bulk("4"),
            Value::Nil,
            Value::Int(1),
            bulk("c"),
        ]);
        assert_eq!(parse_topk_list(&list), vec![("a".to_string(), 9), ("b".to_string(), 4)]);
        assert!(parse_topk_list(&Value::Nil).is_empty());
    }

    #[test]
    fn every_kind_has_the_prefix_its_commands_start_with() {
        let prefixes: Vec<_> = [
            ProbKind::Bloom,
            ProbKind::Cuckoo,
            ProbKind::CountMinSketch,
            ProbKind::TopK,
            ProbKind::TDigest,
        ]
        .iter()
        .map(ProbKind::prefix)
        .collect();
        assert_eq!(prefixes, ["BF", "CF", "CMS", "TOPK", "TDIGEST"]);
    }
}
