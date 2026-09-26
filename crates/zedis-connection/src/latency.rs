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

//! `LATENCY` command wrappers and parsers.
//!
//! Latency monitoring is a different beast from slowlog: instead of
//! tracking slow *commands*, it tracks slow *events* in the server's
//! internal pipeline — fork, AOF rewrite, expire cycles, etc. The
//! `latency-monitor-threshold` config (in ms) gates whether anything
//! gets recorded; the GUI surfaces that fact when LATEST comes back
//! empty so users don't think the panel is broken.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One row from `LATENCY LATEST`. Redis answers four positional fields per
/// event, Valkey 8.1+ six (a running sum and count behind them) — mapped to
/// named fields here.
#[derive(Debug, Clone, Default)]
pub struct LatencyEvent {
    pub event: String,
    /// Unix timestamp seconds of the most recent occurrence.
    pub timestamp: i64,
    /// Latency (ms) of the most recent occurrence.
    pub latest_ms: i64,
    /// Worst latency (ms) seen in the recorded window.
    pub max_ms: i64,
    /// Mean latency (ms) over `avg_samples` occurrences: the server's own
    /// sum and count where `LATENCY LATEST` carries them
    /// (`floors::LATENCY_STATS`, Valkey 8.1+), else the mean of what
    /// `LATENCY HISTORY` still holds — `avg_exact` says which. `None` with
    /// nothing to average.
    pub avg_ms: Option<f64>,
    pub avg_samples: u64,
    pub avg_exact: bool,
}

/// One sample from `LATENCY HISTORY <event>`: `(timestamp, latency_ms)`.
#[derive(Debug, Clone, Default)]
pub struct LatencySample {
    pub timestamp: i64,
    pub latency_ms: i64,
}

/// Wrapper for `LATENCY LATEST`. `unsupported=true` when the server
/// returns `ERR unknown command` (Redis < 2.8.13 or pre-LATENCY
/// builds), so the UI can show an explainer instead of an empty list.
#[derive(Debug, Clone, Default)]
pub struct LatencyListing {
    pub events: Vec<LatencyEvent>,
    pub unsupported: bool,
}

pub async fn latency_latest(at: &ServerDb) -> Result<LatencyListing> {
    let res: redis::RedisResult<Value> = cmd("LATENCY")
        .arg("LATEST")
        .query_async(&mut at.connection().await?)
        .await;
    match res {
        Ok(v) => {
            let mut events = parse_latest(&v).unwrap_or_default();
            // A server that keeps no running sum (Redis) is asked for the
            // samples it still holds — up to 160 per event — and the mean of
            // those stands in. One `HISTORY` per event, and only there.
            for event in events.iter_mut().filter(|event| event.avg_ms.is_none()) {
                let samples = latency_history(at, &event.event).await.unwrap_or_default();
                if let Some(mean) = history_mean(&samples) {
                    event.avg_ms = Some(mean);
                    event.avg_samples = samples.len() as u64;
                    event.avg_exact = false;
                }
            }
            Ok(LatencyListing {
                events,
                unsupported: false,
            })
        }
        Err(e) if reply::is_unsupported(&e) => Ok(LatencyListing {
            unsupported: true,
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

pub async fn latency_history(at: &ServerDb, event: &str) -> Result<Vec<LatencySample>> {
    let v: Value = cmd("LATENCY")
        .arg("HISTORY")
        .arg(event)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(parse_history(&v).unwrap_or_default())
}

/// `LATENCY RESET [event ...]`. Empty `events` clears everything;
/// returns the count of events Redis actually reset.
pub async fn latency_reset(at: &ServerDb, events: &[String]) -> Result<u64> {
    let mut c = cmd("LATENCY");
    c.arg("RESET");
    for e in events {
        c.arg(e.as_str());
    }
    let n: i64 = c.query_async(&mut at.connection().await?).await?;
    Ok(n.max(0) as u64)
}

/// Read the current `latency-monitor-threshold` (in ms). 0 means
/// latency tracking is disabled — UI surfaces this directly so the
/// user knows why LATEST is empty.
pub async fn latency_monitor_threshold(at: &ServerDb) -> Result<u64> {
    let res: redis::RedisResult<Vec<String>> = cmd("CONFIG")
        .arg("GET")
        .arg("latency-monitor-threshold")
        .query_async(&mut at.connection().await?)
        .await;
    match res {
        Ok(pair) => {
            // CONFIG GET returns ["key", "value"]; sometimes just empty
            // if the directive isn't recognised.
            let value = pair.get(1).cloned().unwrap_or_default();
            Ok(value.parse::<u64>().unwrap_or(0))
        }
        Err(_) => Ok(0),
    }
}

// -------- parsers --------

fn parse_latest(v: &Value) -> Option<Vec<LatencyEvent>> {
    let items = match v {
        Value::Array(items) => items,
        _ => return None,
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        // Each entry is itself a 4-element array.
        let Value::Array(parts) = item else { continue };
        if parts.len() < 4 {
            continue;
        }
        let event = match reply::text(&parts[0]) {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        // Valkey 8.1+ appends the sum and the count of every occurrence
        // since the last reset; the mean is theirs to give exactly.
        let (avg_ms, avg_samples, avg_exact) =
            match (parts.get(4).and_then(reply::int), parts.get(5).and_then(reply::int)) {
                (Some(sum), Some(count)) if count > 0 => (Some(sum as f64 / count as f64), count as u64, true),
                _ => (None, 0, false),
            };
        out.push(LatencyEvent {
            event,
            timestamp: reply::int(&parts[1]).unwrap_or_default(),
            latest_ms: reply::int(&parts[2]).unwrap_or_default(),
            max_ms: reply::int(&parts[3]).unwrap_or_default(),
            avg_ms,
            avg_samples,
            avg_exact,
        });
    }
    Some(out)
}

/// The plain mean of the samples' latencies, `None` for none.
fn history_mean(samples: &[LatencySample]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let sum: i64 = samples.iter().map(|sample| sample.latency_ms).sum();
    Some(sum as f64 / samples.len() as f64)
}

fn parse_history(v: &Value) -> Option<Vec<LatencySample>> {
    let items = match v {
        Value::Array(items) => items,
        _ => return None,
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Value::Array(parts) = item else { continue };
        if parts.len() < 2 {
            continue;
        }
        out.push(LatencySample {
            timestamp: reply::int(&parts[0]).unwrap_or_default(),
            latency_ms: reply::int(&parts[1]).unwrap_or_default(),
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Value;

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    /// Redis's four fields leave the mean to HISTORY; Valkey 8.1's six
    /// carry it as a sum and a count.
    #[test]
    fn latest_carries_the_mean_where_the_server_keeps_a_sum() {
        let redis = Value::Array(vec![Value::Array(vec![
            bs("command"),
            Value::Int(1_790_407_875),
            Value::Int(15),
            Value::Int(15),
        ])]);
        let events = parse_latest(&redis).expect("rows");
        assert_eq!(events[0].avg_ms, None);
        assert!(!events[0].avg_exact);

        let valkey = Value::Array(vec![Value::Array(vec![
            bs("command"),
            Value::Int(1_790_407_875),
            Value::Int(16),
            Value::Int(16),
            Value::Int(48),
            Value::Int(3),
        ])]);
        let events = parse_latest(&valkey).expect("rows");
        assert_eq!(events[0].avg_ms, Some(16.0));
        assert_eq!(events[0].avg_samples, 3);
        assert!(events[0].avg_exact);
    }

    #[test]
    fn history_mean_is_the_plain_mean_and_none_for_nothing() {
        let samples = vec![
            LatencySample {
                timestamp: 1,
                latency_ms: 10,
            },
            LatencySample {
                timestamp: 2,
                latency_ms: 20,
            },
            LatencySample {
                timestamp: 3,
                latency_ms: 45,
            },
        ];
        assert_eq!(history_mean(&samples), Some(25.0));
        assert_eq!(history_mean(&[]), None);
    }

    #[test]
    fn parses_latest_with_two_events() {
        let raw = Value::Array(vec![
            Value::Array(vec![
                bs("event-loop"),
                Value::Int(1715731200),
                Value::Int(15),
                Value::Int(42),
            ]),
            Value::Array(vec![
                bs("fork"),
                Value::Int(1715731230),
                Value::Int(120),
                Value::Int(120),
            ]),
        ]);
        let events = parse_latest(&raw).expect("parse");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_str(), "event-loop");
        assert_eq!(events[0].latest_ms, 15);
        assert_eq!(events[0].max_ms, 42);
        assert_eq!(events[1].event.as_str(), "fork");
        assert_eq!(events[1].latest_ms, 120);
    }

    #[test]
    fn parses_latest_empty_array() {
        assert!(parse_latest(&Value::Array(vec![])).expect("parse").is_empty());
    }

    #[test]
    fn parses_history_samples() {
        let raw = Value::Array(vec![
            Value::Array(vec![Value::Int(1715731200), Value::Int(15)]),
            Value::Array(vec![Value::Int(1715731230), Value::Int(120)]),
            Value::Array(vec![Value::Int(1715731260), Value::Int(8)]),
        ]);
        let samples = parse_history(&raw).expect("parse");
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[1].latency_ms, 120);
    }

    #[test]
    fn skips_malformed_rows() {
        let raw = Value::Array(vec![
            Value::Array(vec![bs("event-loop"), Value::Int(1), Value::Int(2), Value::Int(3)]),
            // Missing fields — should be skipped, not crash.
            Value::Array(vec![bs("partial")]),
            // Non-array — also skipped.
            bs("garbage"),
        ]);
        let events = parse_latest(&raw).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_str(), "event-loop");
    }
}
