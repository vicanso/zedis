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

//! The server's own diagnostics, on request: `MEMORY DOCTOR` and `LATENCY
//! DOCTOR` answer prose, `MEMORY STATS` the numbers behind the memory one.
//! All three are read-only.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One node's answer. `node` is the `host:port` a cluster connection
/// fanned the command out to, and empty for a single server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReply<T> {
    pub node: String,
    pub value: T,
}

/// `MEMORY DOCTOR` — the server's advice on its memory use, per node.
pub async fn memory_doctor(conn: &mut RedisAsyncConn) -> Result<Vec<NodeReply<String>>> {
    let reply: Value = cmd("MEMORY").arg("DOCTOR").query_async(conn).await?;
    Ok(per_node(conn, reply)
        .into_iter()
        .map(|(node, value)| NodeReply {
            node,
            value: reply_text(&value),
        })
        .collect())
}

/// `LATENCY DOCTOR` — the server's analysis of its recorded latency
/// events, per node.
pub async fn latency_doctor(conn: &mut RedisAsyncConn) -> Result<Vec<NodeReply<String>>> {
    let reply: Value = cmd("LATENCY").arg("DOCTOR").query_async(conn).await?;
    Ok(per_node(conn, reply)
        .into_iter()
        .map(|(node, value)| NodeReply {
            node,
            value: reply_text(&value),
        })
        .collect())
}

/// `MEMORY STATS` as `(metric, value)` rows in the server's order, per
/// node. The per-database entries nest a map of their own; those are
/// flattened with a dotted prefix (`db.0.overhead.hashtable.main`).
pub async fn memory_stats(conn: &mut RedisAsyncConn) -> Result<Vec<NodeReply<Vec<(String, String)>>>> {
    let reply: Value = cmd("MEMORY").arg("STATS").query_async(conn).await?;
    Ok(per_node(conn, reply)
        .into_iter()
        .map(|(node, value)| {
            let mut rows = Vec::new();
            flatten_stats("", &value, &mut rows);
            NodeReply { node, value: rows }
        })
        .collect())
}

/// A cluster connection sends a keyless command to every master and
/// answers with a map of `host:port` → that node's reply; a single
/// connection answers the reply itself. The connection decides which —
/// `MEMORY STATS` is a map on one RESP3 server too.
fn per_node(conn: &RedisAsyncConn, reply: Value) -> Vec<(String, Value)> {
    if matches!(conn, RedisAsyncConn::Single(_)) {
        return vec![(String::new(), reply)];
    }
    node_replies(reply)
}

fn node_replies(reply: Value) -> Vec<(String, Value)> {
    match reply {
        Value::Map(entries) => entries
            .into_iter()
            .map(|(node, value)| (scalar_text(&node).unwrap_or_default(), value))
            .collect(),
        other => vec![(String::new(), other)],
    }
}

fn reply_text(reply: &Value) -> String {
    match reply {
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => text.clone(),
        Value::Nil => String::new(),
        other => format!("{other:?}"),
    }
}

/// A scalar as text; `None` for a container.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Int(n) => Some(n.to_string()),
        // RESP3 answers ratios as doubles; whole ones print without `.0`,
        // as RESP2's bulk strings do.
        Value::Double(d) if d.fract() == 0.0 && d.abs() < 1e15 => Some(format!("{}", *d as i64)),
        Value::Double(d) => Some(d.to_string()),
        Value::Boolean(flag) => Some(flag.to_string()),
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => Some(text.clone()),
        Value::Nil => Some(String::new()),
        _ => None,
    }
}

/// Reads a RESP2 flat `key value key value…` array or a RESP3 map into
/// `rows`, recursing into nested maps with `prefix.key` names.
fn flatten_stats(prefix: &str, value: &Value, rows: &mut Vec<(String, String)>) {
    let pairs: Vec<(String, &Value)> = match value {
        Value::Map(entries) => entries
            .iter()
            .filter_map(|(key, value)| scalar_text(key).map(|key| (key, value)))
            .collect(),
        Value::Array(items) => items
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|[key, value]| scalar_text(key).map(|key| (key, value)))
            .collect(),
        _ => Vec::new(),
    };
    for (key, value) in pairs {
        let name = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        match scalar_text(value) {
            Some(text) => rows.push((name, text)),
            None => flatten_stats(&name, value, rows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> Value {
        Value::BulkString(text.as_bytes().to_vec())
    }

    #[test]
    fn resp2_stats_flatten_with_the_database_prefix() {
        let reply = Value::Array(vec![
            bulk("peak.allocated"),
            Value::Int(1_048_576),
            bulk("db.0"),
            Value::Array(vec![
                bulk("overhead.hashtable.main"),
                Value::Int(72),
                bulk("overhead.hashtable.expires"),
                Value::Int(0),
            ]),
            bulk("fragmentation"),
            bulk("1.23"),
        ]);
        let mut rows = Vec::new();
        flatten_stats("", &reply, &mut rows);
        assert_eq!(
            rows,
            vec![
                ("peak.allocated".to_string(), "1048576".to_string()),
                ("db.0.overhead.hashtable.main".to_string(), "72".to_string()),
                ("db.0.overhead.hashtable.expires".to_string(), "0".to_string()),
                ("fragmentation".to_string(), "1.23".to_string()),
            ]
        );
    }

    #[test]
    fn resp3_maps_and_doubles_read_the_same_way() {
        let reply = Value::Map(vec![
            (bulk("fragmentation"), Value::Double(1.5)),
            (bulk("allocator.allocated"), Value::Double(4096.0)),
            (
                bulk("db.1"),
                Value::Map(vec![(bulk("overhead.hashtable.main"), Value::Int(8))]),
            ),
        ]);
        let mut rows = Vec::new();
        flatten_stats("", &reply, &mut rows);
        assert_eq!(
            rows,
            vec![
                ("fragmentation".to_string(), "1.5".to_string()),
                ("allocator.allocated".to_string(), "4096".to_string()),
                ("db.1.overhead.hashtable.main".to_string(), "8".to_string()),
            ]
        );
    }

    #[test]
    fn a_cluster_map_splits_into_one_reply_per_node() {
        let reply = Value::Map(vec![
            (bulk("10.0.0.1:6379"), bulk("Hi Sam, all good.")),
            (bulk("10.0.0.2:6379"), bulk("Sam, I detected a few issues.")),
        ]);
        let nodes = node_replies(reply);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].0, "10.0.0.1:6379");
        assert_eq!(reply_text(&nodes[1].1), "Sam, I detected a few issues.");
        // Anything else is one server's own reply.
        let single = node_replies(bulk("Hi Sam, all good."));
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].0, "");
    }

    #[test]
    fn doctor_prose_is_read_from_either_string_shape() {
        assert_eq!(
            reply_text(&bulk("Sam, I have no memory problems.")),
            "Sam, I have no memory problems."
        );
        assert_eq!(
            reply_text(&Value::VerbatimString {
                format: redis::VerbatimFormat::Text,
                text: "ok".into()
            }),
            "ok"
        );
        assert_eq!(reply_text(&Value::Nil), "");
    }
}
