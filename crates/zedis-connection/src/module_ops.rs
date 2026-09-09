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

//! Writes for the module and special key types whose viewers were read-only.
//!
//! Availability is *probed*, never inferred from a brand or a version — the
//! caller checks `ServerCommand::TsAdd` and friends before offering any of
//! this, so a server without RedisTimeSeries simply never shows the buttons.
//!
//! Grouped in one module because they share a shape: a small typed request
//! from a dialog, one command, and a reply the panel reports.

use crate::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

// ── TimeSeries ───────────────────────────────────────────────────────────

/// `TS.ADD key timestamp value` — append or backfill one sample.
///
/// `timestamp` is `None` for "now" (`*`). A sample older than the series'
/// retention, or a duplicate the policy rejects, comes back as an error the
/// caller surfaces; there is nothing sensible to do about it here.
pub async fn ts_add(conn: &mut RedisAsyncConn, key: &str, timestamp: Option<i64>, value: f64) -> Result<i64> {
    let mut command = cmd("TS.ADD");
    command.arg(key);
    match timestamp {
        Some(ts) => command.arg(ts),
        None => command.arg("*"),
    };
    Ok(command.arg(value).query_async(conn).await?)
}

/// One `TS.ALTER` change set. Every field is optional and an omitted one is
/// left alone — `TS.ALTER` only touches what it is given, and sending
/// `LABELS` with nothing after it would *clear* them, which is why an empty
/// label list and "don't change the labels" are different states here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsAlter {
    /// Retention in milliseconds; `0` means keep forever.
    pub retention_ms: Option<i64>,
    /// Replaces the whole label set when present. `Some(vec![])` clears it.
    pub labels: Option<Vec<(String, String)>>,
}

impl TsAlter {
    /// Whether this would send anything at all.
    pub fn is_empty(&self) -> bool {
        self.retention_ms.is_none() && self.labels.is_none()
    }
}

/// `TS.ALTER key [RETENTION ms] [LABELS ...]`.
pub async fn ts_alter(conn: &mut RedisAsyncConn, key: &str, alter: &TsAlter) -> Result<()> {
    if alter.is_empty() {
        return Ok(());
    }
    let mut command = cmd("TS.ALTER");
    command.arg(key);
    if let Some(retention) = alter.retention_ms {
        command.arg("RETENTION").arg(retention);
    }
    if let Some(labels) = &alter.labels {
        command.arg("LABELS");
        for (name, value) in labels {
            command.arg(name).arg(value);
        }
    }
    Ok(command.query_async(conn).await?)
}

/// `TS.CREATERULE source destination AGGREGATION aggregator bucketDuration`.
///
/// The destination series must already exist — RedisTimeSeries does not
/// create it, and the error when it is missing says only "TSDB: the key does
/// not exist", so the dialog says it up front instead.
pub async fn ts_create_rule(
    conn: &mut RedisAsyncConn,
    source: &str,
    destination: &str,
    aggregation: &str,
    bucket_ms: i64,
) -> Result<()> {
    Ok(cmd("TS.CREATERULE")
        .arg(source)
        .arg(destination)
        .arg("AGGREGATION")
        .arg(aggregation)
        .arg(bucket_ms)
        .query_async(conn)
        .await?)
}

/// `TS.DELETERULE source destination`.
pub async fn ts_delete_rule(conn: &mut RedisAsyncConn, source: &str, destination: &str) -> Result<()> {
    Ok(cmd("TS.DELETERULE")
        .arg(source)
        .arg(destination)
        .query_async(conn)
        .await?)
}

/// The aggregators `TS.CREATERULE` accepts, in the order the dialog lists
/// them. Kept here next to the command so the two never drift.
pub const TS_AGGREGATORS: &[&str] = &[
    "avg", "sum", "min", "max", "range", "count", "first", "last", "std.p", "std.s", "var.p", "var.s", "twa",
];

// ── Geo ──────────────────────────────────────────────────────────────────

/// `GEOADD key longitude latitude member` — the only way to put a point in.
///
/// A geo key is a sorted set whose score is a geohash, so the sorted-set
/// editor cannot add one: nobody computes that score by hand. Longitude
/// comes first, which is the opposite of how coordinates are usually spoken,
/// so the dialog labels both.
pub async fn geo_add(conn: &mut RedisAsyncConn, key: &str, lon: f64, lat: f64, member: &str) -> Result<i64> {
    Ok(cmd("GEOADD")
        .arg(key)
        .arg(lon)
        .arg(lat)
        .arg(member)
        .query_async(conn)
        .await?)
}

/// `GEODIST key member1 member2 M` — metres, or `None` when either member is
/// absent.
pub async fn geo_dist(conn: &mut RedisAsyncConn, key: &str, from: &str, to: &str) -> Result<Option<f64>> {
    let raw: Option<String> = cmd("GEODIST")
        .arg(key)
        .arg(from)
        .arg(to)
        .arg("M")
        .query_async(conn)
        .await?;
    Ok(raw.and_then(|value| value.parse().ok()))
}

// ── HyperLogLog / Bitmap ─────────────────────────────────────────────────

/// `PFMERGE destination source [source …]`.
///
/// The destination is *included* in the merge by Redis, so this folds the
/// sources into what is already there rather than replacing it — which is
/// what "merge into this key" should mean, and worth saying in the dialog
/// because the opposite reading is just as natural.
pub async fn pf_merge(conn: &mut RedisAsyncConn, destination: &str, sources: &[String]) -> Result<()> {
    if sources.is_empty() {
        return Ok(());
    }
    let mut command = cmd("PFMERGE");
    command.arg(destination);
    for source in sources {
        command.arg(source);
    }
    Ok(command.query_async(conn).await?)
}

/// The bitwise operations `BITOP` accepts. `NOT` is the odd one out: it
/// takes exactly one source, and the server rejects more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOpKind {
    And,
    Or,
    Xor,
    Not,
}

impl BitOpKind {
    pub const ALL: [BitOpKind; 4] = [BitOpKind::And, BitOpKind::Or, BitOpKind::Xor, BitOpKind::Not];

    pub const fn word(self) -> &'static str {
        match self {
            BitOpKind::And => "AND",
            BitOpKind::Or => "OR",
            BitOpKind::Xor => "XOR",
            BitOpKind::Not => "NOT",
        }
    }

    /// `NOT` inverts a single bitmap; the rest combine any number.
    pub const fn single_source(self) -> bool {
        matches!(self, BitOpKind::Not)
    }
}

/// `BITOP op destination source [source …]` — returns the destination's
/// length in bytes.
pub async fn bit_op(conn: &mut RedisAsyncConn, op: BitOpKind, destination: &str, sources: &[String]) -> Result<u64> {
    if sources.is_empty() || (op.single_source() && sources.len() != 1) {
        return Err(Error::Invalid {
            message: format!("{} takes exactly one source key", op.word()),
        });
    }
    let mut command = cmd("BITOP");
    command.arg(op.word()).arg(destination);
    for source in sources {
        command.arg(source);
    }
    Ok(command.query_async(conn).await?)
}

#[cfg(test)]
mod tests {
    use super::{BitOpKind, TsAlter};

    #[test]
    fn an_alter_with_nothing_set_sends_nothing() {
        assert!(TsAlter::default().is_empty());
        assert!(
            !TsAlter {
                retention_ms: Some(0),
                ..Default::default()
            }
            .is_empty()
        );
        // Clearing the labels is a change, not an absence of one — the
        // distinction `Option<Vec<_>>` exists for.
        assert!(
            !TsAlter {
                labels: Some(Vec::new()),
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn only_not_is_limited_to_one_source() {
        assert!(BitOpKind::Not.single_source());
        for op in [BitOpKind::And, BitOpKind::Or, BitOpKind::Xor] {
            assert!(!op.single_source(), "{}", op.word());
        }
        assert_eq!(BitOpKind::Xor.word(), "XOR");
    }
}
