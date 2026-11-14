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

//! Server-side cursor pagination for `FT.AGGREGATE … WITHCURSOR` and
//! `FT.CURSOR READ / DEL`.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

use super::{AggregateOptions, AggregateResult, aggregate_cmd_with_cursor, parse_aggregate};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One page of an `FT.AGGREGATE … WITHCURSOR` or `FT.CURSOR READ` response.
/// `cursor_id == 0` means the server has exhausted and closed the cursor.
#[derive(Debug, Clone, Default)]
pub struct AggregateCursorResult {
    pub result: AggregateResult,
    pub cursor_id: u64,
}

/// Start a cursor-backed aggregation (`FT.AGGREGATE … WITHCURSOR COUNT read_count`).
pub async fn ft_aggregate_cursor(
    at: &ServerDb,
    index: &str,
    query: &str,
    opts: &AggregateOptions,
    read_count: u32,
) -> Result<AggregateCursorResult> {
    let load_fields = at.client().await?.is_valkey();
    let conn = &mut at.connection().await?;
    let value: Value = aggregate_cmd_with_cursor(index, query, opts, load_fields, Some(read_count))
        .query_async(conn)
        .await?;
    parse_aggregate_cursor(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.AGGREGATE {index} WITHCURSOR returned unexpected shape"),
    })
}

/// Read the next batch from an open aggregation cursor (`FT.CURSOR READ index cursor_id COUNT read_count`).
pub async fn ft_cursor_read(
    at: &ServerDb,
    index: &str,
    cursor_id: u64,
    read_count: u32,
) -> Result<AggregateCursorResult> {
    let conn = &mut at.connection().await?;
    let value: Value = cmd("FT.CURSOR")
        .arg("READ")
        .arg(index)
        .arg(cursor_id)
        .arg("COUNT")
        .arg(read_count.max(1))
        .query_async(conn)
        .await?;
    parse_aggregate_cursor(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.CURSOR READ {index} {cursor_id} returned unexpected shape"),
    })
}

/// Explicitly release an open aggregation cursor (`FT.CURSOR DEL index cursor_id`).
pub async fn ft_cursor_del(at: &ServerDb, index: &str, cursor_id: u64) -> Result<()> {
    if cursor_id == 0 {
        return Ok(());
    }
    let conn = &mut at.connection().await?;
    let _: Value = cmd("FT.CURSOR")
        .arg("DEL")
        .arg(index)
        .arg(cursor_id)
        .query_async(conn)
        .await?;
    Ok(())
}

fn parse_aggregate_map(value: &Value) -> Option<AggregateResult> {
    let pairs = reply::pairs(value)?;
    let mut total = 0u64;
    let mut rows = Vec::new();
    for (k, v) in pairs {
        match k.to_ascii_lowercase().as_str() {
            "total_results" => total = reply::int(&v).unwrap_or_default().max(0) as u64,
            "results" => {
                if let Value::Array(items) = v {
                    for item in items {
                        let Some(item_pairs) = reply::pairs(&item) else {
                            continue;
                        };
                        for (ik, iv) in item_pairs {
                            if matches!(ik.to_ascii_lowercase().as_str(), "extra_attributes" | "values")
                                && let Some(row_pairs) = reply::pairs(&iv)
                            {
                                rows.push(
                                    row_pairs
                                        .into_iter()
                                        .map(|(fk, fv)| (fk, reply::text(&fv).unwrap_or_default()))
                                        .collect(),
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(AggregateResult { total, rows })
}

pub(super) fn parse_aggregate_cursor(value: &Value) -> Option<AggregateCursorResult> {
    match value {
        Value::Array(pair) if pair.len() == 2 => {
            let result = parse_aggregate(&pair[0]).or_else(|| parse_aggregate_map(&pair[0]))?;
            let cursor_id = reply::int(&pair[1]).unwrap_or_default().max(0) as u64;
            Some(AggregateCursorResult { result, cursor_id })
        }
        Value::Map(_) => {
            let pairs = reply::pairs(value)?;
            let mut cursor_id = 0u64;
            for (k, v) in &pairs {
                if k.eq_ignore_ascii_case("cursor") {
                    cursor_id = reply::int(v).unwrap_or_default().max(0) as u64;
                }
            }
            let result = parse_aggregate_map(value)?;
            Some(AggregateCursorResult { result, cursor_id })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn aggregate_cursor_parses_resp2_and_exhausted_cursor() {
        let agg = Value::Array(vec![
            Value::Int(2),
            Value::Array(vec![bs("region"), bs("us"), bs("count"), bs("12")]),
            Value::Array(vec![bs("region"), bs("eu"), bs("count"), bs("7")]),
        ]);
        let page1 = Value::Array(vec![agg.clone(), Value::Int(987654)]);
        let parsed1 = parse_aggregate_cursor(&page1).expect("parse cursor page");
        assert_eq!(parsed1.cursor_id, 987654);
        assert_eq!(parsed1.result.rows.len(), 2);
        assert_eq!(
            parsed1.result.rows[0],
            vec![
                ("region".to_string(), "us".to_string()),
                ("count".to_string(), "12".to_string())
            ]
        );

        let page2 = Value::Array(vec![Value::Array(vec![Value::Int(0)]), Value::Int(0)]);
        let parsed2 = parse_aggregate_cursor(&page2).expect("parse exhausted page");
        assert_eq!(parsed2.cursor_id, 0);
        assert!(parsed2.result.rows.is_empty());
    }
}
