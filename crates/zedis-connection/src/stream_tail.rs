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

//! The live tail of a stream: `XREAD BLOCK` in a loop, on a connection of
//! its own so the block never parks the pool.
//!
//! The view owns the *loop* — a cancellable task it drops to stop — and this
//! owns everything the loop used to know about Redis: the dialing, the
//! command, the cursor (`$`, then the last id seen) and the reply's shape.
//! Dropping the [`StreamTail`] closes the connection.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::reply::{bytes, text, text_lossy};
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One stream entry: its id and its field → value pairs, in order — the
/// value as its bytes, like a page's ([`crate::StreamEntry`]).
pub type StreamTailEntry = (String, Vec<(String, Vec<u8>)>);

/// Consumer-group mode for [`StreamTail::open_group`]: tails new entries (`>`)
/// via `XREADGROUP GROUP group consumer …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamGroupTailSpec {
    pub group: String,
    pub consumer: String,
    /// Pass `NOACK` so tailed entries are not added to the PEL unless the
    /// user explicitly wants consumer-group delivery tracking.
    pub noack: bool,
}

/// A tail in progress. Plain mode starts at `$` (`XREAD`), consumer-group mode
/// reads `>` (`XREADGROUP`), so only entries arriving after open are returned.
pub struct StreamTail {
    conn: RedisAsyncConn,
    key: String,
    last_id: String,
    group: Option<StreamGroupTailSpec>,
}

impl StreamTail {
    /// Dial the tail's connection. Nothing is sent until the first
    /// [`next_batch`](Self::next_batch). Uses a dedicated topology-aware
    /// connection (`ServerDb::dedicated_connection`) so `XREAD BLOCK` routes to
    /// the master owning `key`'s hash slot on a cluster (or the Sentinel master)
    /// without blocking the shared connection pool.
    pub async fn open(at: &ServerDb, key: &str) -> Result<Self> {
        let conn = at.dedicated_connection().await?;
        Ok(Self {
            conn,
            key: key.to_string(),
            last_id: "$".to_string(),
            group: None,
        })
    }

    /// Dial a dedicated connection for `XREADGROUP GROUP group consumer … >`.
    pub async fn open_group(at: &ServerDb, key: &str, spec: StreamGroupTailSpec) -> Result<Self> {
        let conn = at.dedicated_connection().await?;
        Ok(Self {
            conn,
            key: key.to_string(),
            last_id: ">".to_string(),
            group: Some(spec),
        })
    }

    /// One `XREAD` / `XREADGROUP` round. Empty when the block timed out with
    /// nothing new; an error ends the tail.
    pub async fn next_batch(&mut self, block_ms: u64, count: usize) -> Result<Vec<StreamTailEntry>> {
        let reply: Value = if let Some(spec) = &self.group {
            let mut c = cmd("XREADGROUP");
            c.arg("GROUP")
                .arg(&spec.group)
                .arg(&spec.consumer)
                .arg("COUNT")
                .arg(count)
                .arg("BLOCK")
                .arg(block_ms);
            if spec.noack {
                c.arg("NOACK");
            }
            c.arg("STREAMS")
                .arg(&self.key)
                .arg(">")
                .query_async(&mut self.conn)
                .await?
        } else {
            cmd("XREAD")
                .arg("COUNT")
                .arg(count)
                .arg("BLOCK")
                .arg(block_ms)
                .arg("STREAMS")
                .arg(&self.key)
                .arg(&self.last_id)
                .query_async(&mut self.conn)
                .await?
        };
        let entries = parse_xread(&reply);
        if self.group.is_none()
            && let Some((id, _)) = entries.last()
        {
            self.last_id = id.clone();
        }
        Ok(entries)
    }
}

/// A field or value as text: what the user stored, so never dropped for not
/// being UTF-8.
fn cell(value: &Value) -> String {
    text_lossy(value).or_else(|| text(value)).unwrap_or_default()
}

/// The entries of an `XREAD` reply. Parsed by hand because redis's `streams`
/// feature is not enabled on the desktop (it keeps the dependency surface
/// lean). RESP2: `[[name, [[id, [f, v, …]], …]], …]`; RESP3 sends the outer
/// level as a map. Nil — the block timed out — is no entries.
fn parse_xread(reply: &Value) -> Vec<StreamTailEntry> {
    let per_stream: Vec<&Value> = match reply {
        Value::Array(streams) => streams
            .iter()
            .filter_map(|stream| match stream {
                Value::Array(name_and_entries) => name_and_entries.get(1),
                _ => None,
            })
            .collect(),
        Value::Map(streams) => streams.iter().map(|(_, entries)| entries).collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for entries in per_stream {
        let Value::Array(entries) = entries else { continue };
        for entry in entries {
            let Value::Array(id_and_fields) = entry else { continue };
            let Some(id) = id_and_fields.first() else { continue };
            let mut fields = Vec::new();
            if let Some(Value::Array(flat)) = id_and_fields.get(1) {
                let mut it = flat.iter();
                while let (Some(field), Some(value)) = (it.next(), it.next()) {
                    fields.push((cell(field), bytes(value).unwrap_or_else(|| cell(value).into_bytes())));
                }
            }
            out.push((cell(id), fields));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    fn entry(id: &str, fields: &[&str]) -> Value {
        Value::Array(vec![bulk(id), Value::Array(fields.iter().map(|f| bulk(f)).collect())])
    }

    #[test]
    fn xread_entries_are_read_from_both_protocols_and_nil_is_none() {
        let entries = Value::Array(vec![entry("1-1", &["a", "1"]), entry("1-2", &["b", "2", "c", "3"])]);
        let expected = vec![
            ("1-1".to_string(), vec![("a".to_string(), b"1".to_vec())]),
            (
                "1-2".to_string(),
                vec![("b".to_string(), b"2".to_vec()), ("c".to_string(), b"3".to_vec())],
            ),
        ];
        let resp2 = Value::Array(vec![Value::Array(vec![bulk("s"), entries.clone()])]);
        assert_eq!(parse_xread(&resp2), expected);
        let resp3 = Value::Map(vec![(bulk("s"), entries)]);
        assert_eq!(parse_xread(&resp3), expected);

        assert!(parse_xread(&Value::Nil).is_empty(), "the block timed out");
        // A value that is not UTF-8 is still an entry, and still its bytes.
        let binary = Value::Array(vec![Value::Array(vec![
            bulk("s"),
            Value::Array(vec![Value::Array(vec![
                bulk("2-0"),
                Value::Array(vec![bulk("f"), Value::BulkString(vec![0xff])]),
            ])]),
        ])]);
        assert_eq!(
            parse_xread(&binary),
            vec![("2-0".to_string(), vec![("f".to_string(), vec![0xff])])]
        );
    }
}
