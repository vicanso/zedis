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

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

// ── TimeSeries ───────────────────────────────────────────────────────────

/// `TS.ADD key timestamp value` — append or backfill one sample.
///
/// `timestamp` is `None` for "now" (`*`). A sample older than the series'
/// retention, or a duplicate the policy rejects, comes back as an error the
/// caller surfaces; there is nothing sensible to do about it here.
pub async fn ts_add(at: &ServerDb, key: &str, timestamp: Option<i64>, value: f64) -> Result<i64> {
    let mut command = cmd("TS.ADD");
    command.arg(key);
    match timestamp {
        Some(ts) => command.arg(ts),
        None => command.arg("*"),
    };
    Ok(command.arg(value).query_async(&mut at.connection().await?).await?)
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
pub async fn ts_alter(at: &ServerDb, key: &str, alter: &TsAlter) -> Result<()> {
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
    Ok(command.query_async(&mut at.connection().await?).await?)
}

/// `TS.CREATERULE source destination AGGREGATION aggregator bucketDuration`.
///
/// The destination series must already exist — RedisTimeSeries does not
/// create it, and the error when it is missing says only "TSDB: the key does
/// not exist", so the dialog says it up front instead.
pub async fn ts_create_rule(
    at: &ServerDb,
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
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `TS.DELETERULE source destination`.
pub async fn ts_delete_rule(at: &ServerDb, source: &str, destination: &str) -> Result<()> {
    Ok(cmd("TS.DELETERULE")
        .arg(source)
        .arg(destination)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// The aggregators `TS.CREATERULE` accepts, in the order the dialog lists
/// them. Kept here next to the command so the two never drift.
pub const TS_AGGREGATORS: &[&str] = &[
    "avg", "sum", "min", "max", "range", "count", "first", "last", "std.p", "std.s", "var.p", "var.s", "twa",
];

/// A compaction rule out of a series: `TS.INFO` reports them as
/// `[destination, bucket, aggregator]` triples.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TsRule {
    pub destination: String,
    pub bucket_ms: i64,
    pub aggregator: String,
}

/// `TS.INFO`, best effort — a field the server did not send stays 0 / empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TsInfo {
    pub total_samples: i64,
    pub memory_usage: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub retention_ms: i64,
    pub chunk_count: i64,
    pub labels: Vec<(String, String)>,
    pub rules: Vec<TsRule>,
    /// The series this one is a compaction *of*, when it is one. A
    /// destination series is written by its rule, not by hand.
    pub source_key: Option<String>,
}

/// A series as its viewer shows it: the metadata and one window of samples.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsWindow {
    pub info: TsInfo,
    pub samples: Vec<(i64, f64)>,
}

/// The `TS.RANGE` a window asks for: `(from, to, bucket_ms)`, or `None` when
/// the series has nothing to plot. A `bucket_ms` of 1 means "send no
/// aggregation at all" — the span is already short enough that one bucket per
/// millisecond would not reduce anything.
fn window_bounds(first_ts: i64, last_ts: i64, window_ms: Option<i64>, target_points: i64) -> Option<(i64, i64, i64)> {
    if last_ts <= 0 {
        return None;
    }
    let to = last_ts;
    let from = match window_ms {
        Some(window) => (to - window).max(first_ts),
        None => first_ts,
    }
    .min(to);
    Some((from, to, ((to - from).max(1) / target_points.max(1)).max(1)))
}

/// `TS.INFO` plus a `TS.RANGE` over the last `window_ms` of the series (all of
/// it for `None`), bucketed with a server-side `avg` so that about
/// `target_points` come back however dense the series is. An empty series is
/// its info and no samples, not an error.
///
/// The aggregation is aligned to the window's start (`ALIGN from`;
/// RedisTimeSeries 1.6+, and `ALIGN` sits immediately before `AGGREGATION`).
/// Without it buckets align to timestamp 0, and a
/// series holding one backfilled sample next to live ones spans decades — so
/// the bucket is billions of milliseconds wide and the first sample comes
/// back stamped `0`, i.e. *outside* the window that was asked for, plotted at
/// 1970. The samples a window returns have to lie inside that window.
pub async fn ts_window(at: &ServerDb, key: &str, window_ms: Option<i64>, target_points: i64) -> Result<TsWindow> {
    let mut conn = at.connection().await?;
    let info_raw: Value = cmd("TS.INFO").arg(key).query_async(&mut conn).await?;
    let info = parse_ts_info(&info_raw);
    let bounds = (info.total_samples > 0)
        .then(|| window_bounds(info.first_ts, info.last_ts, window_ms, target_points))
        .flatten();
    let Some((from, to, bucket)) = bounds else {
        return Ok(TsWindow {
            info,
            samples: Vec::new(),
        });
    };
    let mut range = cmd("TS.RANGE");
    range.arg(key).arg(from).arg(to);
    if bucket > 1 {
        range.arg("ALIGN").arg(from).arg("AGGREGATION").arg("avg").arg(bucket);
    }
    let samples: Vec<(i64, f64)> = range.query_async(&mut conn).await?;
    Ok(TsWindow { info, samples })
}

/// The fields of a `TS.INFO` reply, RESP3 map or RESP2 flat array alike.
/// Lenient where [`reply::pairs`] is strict: a field that cannot be read is
/// skipped, because one odd entry must not blank the whole panel.
fn ts_info_fields(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| reply::text_lossy(k).map(|name| (name, v)))
            .collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|chunk| Some((reply::text_lossy(chunk.first()?)?, chunk.get(1)?)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Read a `TS.INFO` reply, best effort.
fn parse_ts_info(value: &Value) -> TsInfo {
    let fields = ts_info_fields(value);
    let find = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| *v);
    let number = |name: &str| find(name).and_then(reply::int).unwrap_or(0);
    let mut info = TsInfo {
        total_samples: number("totalSamples"),
        memory_usage: number("memoryUsage"),
        first_ts: number("firstTimestamp"),
        last_ts: number("lastTimestamp"),
        retention_ms: number("retentionTime"),
        chunk_count: number("chunkCount"),
        source_key: find("sourceKey").and_then(reply::text_lossy).filter(|s| !s.is_empty()),
        ..Default::default()
    };
    // Labels: `[[name, value], …]`, or a map under RESP3.
    match find("labels") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Value::Array(kv) = item
                    && let (Some(k), Some(v)) = (
                        kv.first().and_then(reply::text_lossy),
                        kv.get(1).and_then(reply::text_lossy),
                    )
                {
                    info.labels.push((k, v));
                }
            }
        }
        Some(map @ Value::Map(_)) => {
            for (k, v) in reply::pairs(map).unwrap_or_default() {
                if let Some(v) = reply::text_lossy(&v) {
                    info.labels.push((k, v));
                }
            }
        }
        _ => {}
    }
    // Rules: `[[destination, bucket, aggregator], …]`, or RESP3's map from
    // destination to `[bucket, aggregator, …]`.
    match find("rules") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Value::Array(f) = item
                    && let (Some(destination), Some(bucket_ms), Some(aggregator)) = (
                        f.first().and_then(reply::text_lossy),
                        f.get(1).and_then(reply::int),
                        f.get(2).and_then(reply::text_lossy),
                    )
                {
                    info.rules.push(TsRule {
                        destination,
                        bucket_ms,
                        aggregator,
                    });
                }
            }
        }
        Some(map @ Value::Map(_)) => {
            for (destination, v) in reply::pairs(map).unwrap_or_default() {
                if let Value::Array(f) = v
                    && let (Some(bucket_ms), Some(aggregator)) =
                        (f.first().and_then(reply::int), f.get(1).and_then(reply::text_lossy))
                {
                    info.rules.push(TsRule {
                        destination,
                        bucket_ms,
                        aggregator,
                    });
                }
            }
        }
        _ => {}
    }
    info
}

/// One series returned by `TS.MRANGE`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsSeries {
    pub key: String,
    /// Labels, when the query asked for them (`WITHLABELS`).
    pub labels: Vec<(String, String)>,
    /// `(timestamp_ms, value)`, oldest first.
    pub samples: Vec<(i64, f64)>,
}

/// A multi-series query.
///
/// `filters` are RedisTimeSeries label matchers (`env=prod`, `host!=a`,
/// `region=(eu,us)`) and at least one of them must be a positive `k=v` —
/// the server rejects a query that only excludes, because it would have to
/// scan every series to answer. The panel says so rather than passing the
/// refusal through.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsMRange {
    /// Milliseconds; `None` for the open end (`-` / `+`).
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub filters: Vec<String>,
    /// `(aggregator, bucket_ms)` — downsampling, which is what makes a
    /// multi-series chart readable at all beyond a few thousand points.
    pub aggregation: Option<(String, i64)>,
    /// Samples per series. RedisTimeSeries has no default cap, and a panel
    /// that asks for a year of raw samples across fifty series is a hang.
    pub count: Option<u64>,
}

/// Whether `filters` contains at least one positive `k=v` matcher, which is
/// what RedisTimeSeries requires. Pure so the dialog can check as you type.
pub fn has_positive_matcher(filters: &[String]) -> bool {
    filters.iter().any(|f| {
        let Some((name, value)) = f.split_once('=') else {
            return false;
        };
        // `k!=v` splits at the same `=`, leaving a name ending in `!`.
        !name.ends_with('!') && !name.trim().is_empty() && !value.trim().is_empty()
    })
}

/// `TS.MRANGE from to [AGGREGATION agg bucket] [COUNT n] WITHLABELS FILTER …`
pub async fn ts_mrange(at: &ServerDb, query: &TsMRange) -> Result<Vec<TsSeries>> {
    if !has_positive_matcher(&query.filters) {
        return Err(Error::Invalid {
            message: "at least one label filter must be a positive match (label=value)".to_string(),
        });
    }
    let mut command = cmd("TS.MRANGE");
    match query.from_ms {
        Some(from) => command.arg(from),
        None => command.arg("-"),
    };
    match query.to_ms {
        Some(to) => command.arg(to),
        None => command.arg("+"),
    };
    if let Some(count) = query.count {
        command.arg("COUNT").arg(count);
    }
    if let Some((aggregator, bucket_ms)) = &query.aggregation {
        command.arg("AGGREGATION").arg(aggregator).arg(*bucket_ms);
    }
    command.arg("WITHLABELS").arg("FILTER");
    for filter in &query.filters {
        command.arg(filter.as_str());
    }
    let raw: Value = command.query_async(&mut at.connection().await?).await?;
    Ok(parse_mrange(&raw))
}

/// `[[key, [[label, value] …], [[ts, value] …]] …]`.
///
/// Anything that does not have that shape is skipped rather than defaulted:
/// a series drawn at the wrong key or a sample at timestamp 0 would be worse
/// than one missing from the chart.
fn parse_mrange(value: &Value) -> Vec<TsSeries> {
    let Value::Array(entries) = value else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let Value::Array(fields) = entry else {
                return None;
            };
            let key = value_text(fields.first()?)?;
            let labels = match fields.get(1) {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| {
                        let Value::Array(pair) = item else {
                            return None;
                        };
                        Some((value_text(pair.first()?)?, value_text(pair.get(1)?)?))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let samples = match fields.get(2) {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| {
                        let Value::Array(pair) = item else {
                            return None;
                        };
                        let ts = match pair.first()? {
                            Value::Int(ts) => *ts,
                            other => value_text(other)?.parse().ok()?,
                        };
                        // Values come back as bulk strings even though they
                        // are doubles, which is why this is not `as_f64`.
                        let sample = value_text(pair.get(1)?)?.parse().ok()?;
                        Some((ts, sample))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Some(TsSeries { key, labels, samples })
        })
        .collect()
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(text) => Some(text.clone()),
        Value::Int(number) => Some(number.to_string()),
        Value::Double(number) => Some(number.to_string()),
        _ => None,
    }
}

// ── Geo ──────────────────────────────────────────────────────────────────

// ── HyperLogLog / Bitmap ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{TsAlter, TsSeries, has_positive_matcher, parse_mrange, window_bounds};

    #[test]
    fn ts_info_is_read_from_both_protocols_and_survives_an_odd_field() {
        use super::{TsRule, parse_ts_info};
        use redis::Value;
        let b = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let resp2 = Value::Array(vec![
            b("totalSamples"),
            Value::Int(42),
            Value::Nil, // a field name that cannot be read: skipped, not fatal
            Value::Int(1),
            b("firstTimestamp"),
            Value::Int(1000),
            b("lastTimestamp"),
            Value::Int(9000),
            b("retentionTime"),
            b("60000"),
            b("labels"),
            Value::Array(vec![Value::Array(vec![b("region"), b("eu")])]),
            b("rules"),
            Value::Array(vec![Value::Array(vec![b("series:1m"), Value::Int(60_000), b("AVG")])]),
            b("sourceKey"),
            Value::Nil,
        ]);
        let info = parse_ts_info(&resp2);
        assert_eq!(
            (info.total_samples, info.first_ts, info.last_ts, info.retention_ms),
            (42, 1000, 9000, 60_000)
        );
        assert_eq!(info.labels, vec![("region".to_string(), "eu".to_string())]);
        assert_eq!(
            info.rules,
            vec![TsRule {
                destination: "series:1m".to_string(),
                bucket_ms: 60_000,
                aggregator: "AVG".to_string()
            }]
        );
        assert_eq!(info.source_key, None);

        // RESP3: maps all the way down.
        let resp3 = Value::Map(vec![
            (b("totalSamples"), Value::Int(1)),
            (b("labels"), Value::Map(vec![(b("region"), b("eu"))])),
            (
                b("rules"),
                Value::Map(vec![(b("series:1m"), Value::Array(vec![Value::Int(60_000), b("AVG")]))]),
            ),
            (b("sourceKey"), b("series")),
        ]);
        let info = parse_ts_info(&resp3);
        assert_eq!(info.labels, vec![("region".to_string(), "eu".to_string())]);
        assert_eq!(info.rules[0].bucket_ms, 60_000);
        assert_eq!(info.source_key.as_deref(), Some("series"));
        // Not a TS.INFO at all: everything at its default.
        assert_eq!(parse_ts_info(&Value::Nil).total_samples, 0);
    }
    use redis::Value;

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
    fn a_query_that_only_excludes_is_refused_before_it_is_sent() {
        assert!(has_positive_matcher(&["env=prod".to_string()]));
        assert!(has_positive_matcher(&[
            "host!=a".to_string(),
            "region=(eu,us)".to_string()
        ]));
        // Only negatives, only an empty value, or no matcher at all: the
        // server would refuse each of these.
        assert!(!has_positive_matcher(&["host!=a".to_string()]));
        assert!(!has_positive_matcher(&["env=".to_string()]));
        assert!(!has_positive_matcher(&["env".to_string()]));
        assert!(!has_positive_matcher(&[]));
    }

    fn bulk(text: &str) -> Value {
        Value::BulkString(text.as_bytes().to_vec())
    }

    #[test]
    fn mrange_parses_key_labels_and_samples() {
        let reply = Value::Array(vec![Value::Array(vec![
            bulk("cpu:1"),
            Value::Array(vec![Value::Array(vec![bulk("host"), bulk("a")])]),
            Value::Array(vec![
                Value::Array(vec![Value::Int(1000), bulk("1.5")]),
                Value::Array(vec![Value::Int(2000), bulk("2")]),
            ]),
        ])]);
        assert_eq!(
            parse_mrange(&reply),
            vec![TsSeries {
                key: "cpu:1".to_string(),
                labels: vec![("host".to_string(), "a".to_string())],
                samples: vec![(1000, 1.5), (2000, 2.0)],
            }]
        );
    }

    #[test]
    fn malformed_entries_are_skipped_rather_than_defaulted() {
        let reply = Value::Array(vec![
            // No key at all.
            Value::Array(vec![]),
            // A sample whose value is not a number, next to a good one.
            Value::Array(vec![
                bulk("cpu:2"),
                Value::Nil,
                Value::Array(vec![
                    Value::Array(vec![Value::Int(1), bulk("oops")]),
                    Value::Array(vec![Value::Int(2), bulk("3.5")]),
                ]),
            ]),
        ]);
        let series = parse_mrange(&reply);
        assert_eq!(series.len(), 1, "the keyless entry is dropped");
        assert_eq!(series[0].key, "cpu:2");
        assert!(series[0].labels.is_empty(), "a nil label block is no labels");
        assert_eq!(
            series[0].samples,
            vec![(2, 3.5)],
            "the unparsable sample is skipped, not zeroed"
        );
    }

    #[test]
    fn a_window_buckets_to_about_the_points_asked_for_and_starts_where_it_says() {
        // A dense minute, 240 points wanted: 250ms buckets.
        assert_eq!(window_bounds(0, 60_000, None, 240), Some((0, 60_000, 250)));
        // A window narrower than the series starts `window_ms` before the end.
        assert_eq!(window_bounds(0, 60_000, Some(10_000), 100), Some((50_000, 60_000, 100)));
        // …but never before the first sample.
        assert_eq!(
            window_bounds(55_000, 60_000, Some(10_000), 100),
            Some((55_000, 60_000, 50))
        );

        // The shape the redis-stack suite caught: one sample backfilled at
        // ts 1000 beside a live one. The span is decades, so the bucket is
        // billions of milliseconds — which is exactly why the range has to
        // carry `ALIGN from`. Epoch-aligned, the 1000 sample would come back
        // stamped 0, outside the window.
        let live = 1_758_000_000_000;
        let (from, to, bucket) = window_bounds(1000, live, None, 240).expect("two samples");
        assert_eq!((from, to), (1000, live));
        assert!(bucket > 7_000_000_000, "a decades-wide span buckets coarsely: {bucket}");
        assert!(
            from % bucket != 0,
            "the window start is not on an epoch-aligned boundary, which is what makes ALIGN matter"
        );

        // A span too short to reduce: no aggregation clause at all.
        assert_eq!(window_bounds(1000, 1001, None, 240), Some((1000, 1001, 1)));
        assert_eq!(window_bounds(1000, 1000, None, 240), Some((1000, 1000, 1)));
        // `target_points` of 0 must not divide by zero.
        assert_eq!(window_bounds(0, 60_000, None, 0), Some((0, 60_000, 60_000)));
        // Nothing ever written: no range to ask for.
        assert_eq!(window_bounds(0, 0, None, 240), None);
    }
}
