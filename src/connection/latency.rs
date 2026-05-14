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

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use gpui::SharedString;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One row from `LATENCY LATEST`. Redis returns four positional fields
/// per event — we map them to named fields here.
#[derive(Debug, Clone, Default)]
pub struct LatencyEvent {
    pub event: SharedString,
    /// Unix timestamp seconds of the most recent occurrence.
    pub timestamp: i64,
    /// Latency (ms) of the most recent occurrence.
    pub latest_ms: i64,
    /// Worst latency (ms) seen in the recorded window.
    pub max_ms: i64,
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

pub async fn latency_latest(conn: &mut RedisAsyncConn) -> Result<LatencyListing> {
    let res: redis::RedisResult<Value> = cmd("LATENCY").arg("LATEST").query_async(conn).await;
    match res {
        Ok(v) => Ok(LatencyListing {
            events: parse_latest(&v).unwrap_or_default(),
            unsupported: false,
        }),
        Err(e) if is_unsupported(&e) => Ok(LatencyListing {
            unsupported: true,
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

pub async fn latency_history(conn: &mut RedisAsyncConn, event: &str) -> Result<Vec<LatencySample>> {
    let v: Value = cmd("LATENCY").arg("HISTORY").arg(event).query_async(conn).await?;
    Ok(parse_history(&v).unwrap_or_default())
}

/// Returns the raw ASCII art `LATENCY GRAPH` reply verbatim — the
/// view renders it inside a monospace block so the histogram lines
/// up the way Redis intends.
pub async fn latency_graph(conn: &mut RedisAsyncConn, event: &str) -> Result<String> {
    let s: String = cmd("LATENCY").arg("GRAPH").arg(event).query_async(conn).await?;
    Ok(s)
}

/// `LATENCY RESET [event ...]`. Empty `events` clears everything;
/// returns the count of events Redis actually reset.
pub async fn latency_reset(conn: &mut RedisAsyncConn, events: &[String]) -> Result<u64> {
    let mut c = cmd("LATENCY");
    c.arg("RESET");
    for e in events {
        c.arg(e.as_str());
    }
    let n: i64 = c.query_async(conn).await?;
    Ok(n.max(0) as u64)
}

/// Read the current `latency-monitor-threshold` (in ms). 0 means
/// latency tracking is disabled — UI surfaces this directly so the
/// user knows why LATEST is empty.
pub async fn latency_monitor_threshold(conn: &mut RedisAsyncConn) -> Result<u64> {
    let res: redis::RedisResult<Vec<String>> = cmd("CONFIG")
        .arg("GET")
        .arg("latency-monitor-threshold")
        .query_async(conn)
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

fn is_unsupported(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    msg.contains("unknown command") || msg.contains("ERR unknown") || msg.contains("ERR Unknown")
}

fn parse_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::BulkString(bytes) => std::str::from_utf8(bytes).ok().and_then(|s| s.parse().ok()),
        Value::SimpleString(s) => s.parse().ok(),
        _ => None,
    }
}

fn parse_simple_string(v: &Value) -> Option<String> {
    match v {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

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
        let event = match parse_simple_string(&parts[0]) {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        out.push(LatencyEvent {
            event: event.into(),
            timestamp: parse_int(&parts[1]).unwrap_or_default(),
            latest_ms: parse_int(&parts[2]).unwrap_or_default(),
            max_ms: parse_int(&parts[3]).unwrap_or_default(),
        });
    }
    Some(out)
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
            timestamp: parse_int(&parts[0]).unwrap_or_default(),
            latency_ms: parse_int(&parts[1]).unwrap_or_default(),
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
        assert_eq!(events[0].event.as_ref(), "event-loop");
        assert_eq!(events[0].latest_ms, 15);
        assert_eq!(events[0].max_ms, 42);
        assert_eq!(events[1].event.as_ref(), "fork");
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
        assert_eq!(events[0].event.as_ref(), "event-loop");
    }
}
