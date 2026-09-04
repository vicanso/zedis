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

//! How the terminal shows a Redis reply: the plain text redis-cli style
//! (the default), an aligned table for arrays and maps, or pretty JSON.
//! Pure functions over `redis::Value`; the terminal only picks the format.
//!
//! RESP2 flattens a hash, a `WITHSCORES` range or `CONFIG GET` into one
//! flat array, so the table and JSON renderings take the command that
//! produced the reply and pair the elements up when it is one of those.

use crate::string::redis_value_to_string;
use redis::Value;
use serde_json::{Map, Number, json};

/// Rendering of a reply in the terminal's output pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplyFormat {
    /// redis-cli style: scalars verbatim, `[a, b]` for arrays.
    #[default]
    Text,
    /// An aligned grid — `#` / value for lists, field / value for pairs.
    Table,
    /// Pretty-printed JSON; pairs become an object.
    Json,
}

impl ReplyFormat {
    pub const ALL: [ReplyFormat; 3] = [ReplyFormat::Text, ReplyFormat::Table, ReplyFormat::Json];

    /// Stable name for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            ReplyFormat::Text => "text",
            ReplyFormat::Table => "table",
            ReplyFormat::Json => "json",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == name)
    }

    /// The toolbar label.
    pub fn label(self) -> &'static str {
        match self {
            ReplyFormat::Text => "Text",
            ReplyFormat::Table => "Table",
            ReplyFormat::Json => "JSON",
        }
    }
}

/// Longest cell drawn before it is cut with `…`; a value column is for
/// scanning, the full value is one Text-mode switch away.
const MAX_CELL_CHARS: usize = 80;
/// Rows drawn before the table stops with a "… n more rows" footer.
const MAX_TABLE_ROWS: usize = 500;

/// Render one reply. `cmd` / `args` tell a pairs reply (`HGETALL`,
/// `… WITHSCORES`, `CONFIG GET`) apart from a plain list.
pub fn format_reply(cmd: &str, args: &[String], value: &Value, format: ReplyFormat) -> String {
    match format {
        ReplyFormat::Text => redis_value_to_string(value),
        ReplyFormat::Table => format_table(value, pairs_reply(cmd, args)),
        ReplyFormat::Json => pretty_json(&redis_value_to_json(value, pairs_reply(cmd, args))),
    }
}

/// The result of a `MULTI … EXEC` block, or any command list run against
/// its replies: one row per queued command. Text and Table modes draw the
/// grid; JSON mode emits `[{"command", "reply"}]`.
pub fn format_exec(commands: &[String], replies: &[Value], format: ReplyFormat) -> String {
    if format == ReplyFormat::Json {
        let rows: Vec<serde_json::Value> = commands
            .iter()
            .enumerate()
            .map(|(ix, command)| {
                let reply = replies
                    .get(ix)
                    .map(|v| redis_value_to_json(v, false))
                    .unwrap_or(serde_json::Value::Null);
                json!({ "command": command, "reply": reply })
            })
            .collect();
        return pretty_json(&serde_json::Value::Array(rows));
    }
    let rows: Vec<Vec<String>> = commands
        .iter()
        .enumerate()
        .map(|(ix, command)| {
            let reply = replies
                .get(ix)
                .map(cell_text)
                .unwrap_or_else(|| "(no reply)".to_string());
            vec![(ix + 1).to_string(), clip_cell(command), reply]
        })
        .collect();
    render_table(&["#", "command", "reply"], rows)
}

/// Whether the command's flat array reply is really a list of pairs.
pub fn pairs_reply(cmd: &str, args: &[String]) -> bool {
    let cmd = cmd.to_ascii_uppercase();
    match cmd.as_str() {
        "HGETALL" | "ZPOPMIN" | "ZPOPMAX" => true,
        "CONFIG" => args.first().is_some_and(|sub| sub.eq_ignore_ascii_case("GET")),
        _ => args
            .iter()
            .any(|arg| arg.eq_ignore_ascii_case("WITHSCORES") || arg.eq_ignore_ascii_case("WITHVALUES")),
    }
}

/// `redis::Value` as JSON. Bulk strings are read as UTF-8 (lossily), a
/// pairs array becomes an object keyed by its even elements.
pub fn redis_value_to_json(value: &Value, pairs: bool) -> serde_json::Value {
    match value {
        Value::Nil => serde_json::Value::Null,
        Value::Int(i) => json!(i),
        Value::Okay => json!("OK"),
        Value::SimpleString(s) => json!(s),
        Value::BulkString(bytes) => json!(String::from_utf8_lossy(bytes)),
        Value::Double(f) => Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Boolean(b) => json!(b),
        Value::Array(items) | Value::Set(items) => {
            if pairs && items.len() % 2 == 0 {
                let mut object = Map::new();
                for pair in items.chunks(2) {
                    object.insert(scalar_text(&pair[0]), redis_value_to_json(&pair[1], false));
                }
                serde_json::Value::Object(object)
            } else {
                serde_json::Value::Array(items.iter().map(|v| redis_value_to_json(v, false)).collect())
            }
        }
        Value::Map(entries) => {
            let mut object = Map::new();
            for (k, v) in entries {
                object.insert(scalar_text(k), redis_value_to_json(v, false));
            }
            serde_json::Value::Object(object)
        }
        Value::VerbatimString { text, .. } => json!(text),
        Value::Attribute { data, .. } => redis_value_to_json(data, pairs),
        Value::BigNumber(n) => json!(n.to_string()),
        Value::ServerError(e) => json!({ "error": e.to_string() }),
        Value::Push { kind, data } => json!({
            "kind": format!("{kind:?}"),
            "data": data.iter().map(|v| redis_value_to_json(v, false)).collect::<Vec<_>>(),
        }),
        _ => json!("Unsupported"),
    }
}

fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn format_table(value: &Value, pairs: bool) -> String {
    match value {
        Value::Array(items) | Value::Set(items) => {
            if items.is_empty() {
                return "(empty array)".to_string();
            }
            if pairs && items.len() % 2 == 0 {
                let rows = items
                    .chunks(2)
                    .map(|pair| vec![cell_text(&pair[0]), cell_text(&pair[1])])
                    .collect();
                render_table(&["field", "value"], rows)
            } else {
                let rows = items
                    .iter()
                    .enumerate()
                    .map(|(ix, item)| vec![(ix + 1).to_string(), cell_text(item)])
                    .collect();
                render_table(&["#", "value"], rows)
            }
        }
        Value::Map(entries) => {
            if entries.is_empty() {
                return "(empty map)".to_string();
            }
            let rows = entries.iter().map(|(k, v)| vec![cell_text(k), cell_text(v)]).collect();
            render_table(&["field", "value"], rows)
        }
        Value::Attribute { data, .. } => format_table(data, pairs),
        _ => redis_value_to_string(value),
    }
}

/// A scalar as the JSON object key / table field it names.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        other => redis_value_to_string(other),
    }
}

/// One cell: a scalar verbatim, a nested value in its compact text form,
/// on one line and clipped.
fn cell_text(value: &Value) -> String {
    clip_cell(&redis_value_to_string(value))
}

fn clip_cell(text: &str) -> String {
    let one_line: String = text
        .chars()
        .map(|c| match c {
            '\n' => '⏎',
            '\r' | '\t' => ' ',
            c => c,
        })
        .collect();
    if one_line.chars().count() <= MAX_CELL_CHARS {
        return one_line;
    }
    let mut clipped: String = one_line.chars().take(MAX_CELL_CHARS - 1).collect();
    clipped.push('…');
    clipped
}

/// Monospace grid: header, rule, rows; a `#` column is right-aligned.
fn render_table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let total = rows.len();
    let rows: Vec<Vec<String>> = rows.into_iter().take(MAX_TABLE_ROWS).collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (ix, cell) in row.iter().enumerate() {
            widths[ix] = widths[ix].max(cell.chars().count());
        }
    }
    let last = headers.len().saturating_sub(1);
    // The last column is not padded: no trailing blanks to copy or save.
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(ix, cell)| {
                let pad = widths[ix].saturating_sub(cell.chars().count());
                if headers[ix] == "#" {
                    format!("{}{cell}", " ".repeat(pad))
                } else if ix == last {
                    cell.clone()
                } else {
                    format!("{cell}{}", " ".repeat(pad))
                }
            })
            .collect::<Vec<_>>()
            .join(" │ ")
    };
    let mut out = String::new();
    out.push_str(&line(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
    out.push('\n');
    out.push_str(&widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─"));
    for row in &rows {
        out.push('\n');
        out.push_str(&line(row));
    }
    if total > rows.len() {
        out.push_str(&format!("\n… {} more rows", total - rows.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_round_trip() {
        for format in ReplyFormat::ALL {
            assert_eq!(ReplyFormat::from_name(format.as_str()), Some(format));
        }
        assert_eq!(ReplyFormat::from_name("csv"), None);
    }

    #[test]
    fn pairs_are_recognised_by_command_or_modifier() {
        assert!(pairs_reply("hgetall", &args(&["h"])));
        assert!(pairs_reply("CONFIG", &args(&["GET", "maxmemory"])));
        assert!(!pairs_reply("CONFIG", &args(&["SET", "maxmemory", "0"])));
        assert!(pairs_reply("ZRANGE", &args(&["z", "0", "-1", "WITHSCORES"])));
        assert!(pairs_reply("HRANDFIELD", &args(&["h", "2", "withvalues"])));
        assert!(!pairs_reply("LRANGE", &args(&["l", "0", "-1"])));
    }

    #[test]
    fn table_pairs_a_hash_and_numbers_a_list() {
        let hash = Value::Array(vec![bulk("name"), bulk("zedis"), bulk("stars"), bulk("1")]);
        let table = format_reply("HGETALL", &args(&["h"]), &hash, ReplyFormat::Table);
        assert_eq!(table, "field │ value\n──────┼──────\nname  │ zedis\nstars │ 1");

        let list = Value::Array(vec![bulk("a"), bulk("bb")]);
        let table = format_reply("LRANGE", &args(&["l", "0", "-1"]), &list, ReplyFormat::Table);
        assert_eq!(table, "# │ value\n──┼──────\n1 │ a\n2 │ bb");

        // Scalars stay plain, an empty reply says so.
        assert_eq!(format_reply("GET", &args(&["k"]), &bulk("v"), ReplyFormat::Table), "v");
        assert_eq!(
            format_reply("LRANGE", &args(&[]), &Value::Array(vec![]), ReplyFormat::Table),
            "(empty array)"
        );
    }

    #[test]
    fn json_pairs_become_an_object_and_nested_values_stay_arrays() {
        let hash = Value::Array(vec![bulk("name"), bulk("zedis"), bulk("n"), Value::Int(3)]);
        let json = format_reply("HGETALL", &args(&["h"]), &hash, ReplyFormat::Json);
        assert_eq!(json, "{\n  \"name\": \"zedis\",\n  \"n\": 3\n}");

        let nested = Value::Array(vec![bulk("1-0"), Value::Array(vec![bulk("f"), bulk("v")])]);
        let json = format_reply("XRANGE", &args(&[]), &nested, ReplyFormat::Json);
        assert_eq!(json, "[\n  \"1-0\",\n  [\n    \"f\",\n    \"v\"\n  ]\n]");

        assert_eq!(format_reply("GET", &args(&[]), &Value::Nil, ReplyFormat::Json), "null");
        assert_eq!(
            format_reply("SET", &args(&[]), &Value::Okay, ReplyFormat::Json),
            "\"OK\""
        );
    }

    #[test]
    fn cells_are_clipped_to_one_line() {
        let long = "x".repeat(200);
        assert_eq!(clip_cell(&long).chars().count(), MAX_CELL_CHARS);
        assert!(clip_cell(&long).ends_with('…'));
        assert_eq!(clip_cell("a\nb"), "a⏎b");
    }

    #[test]
    fn exec_rows_pair_commands_with_replies() {
        let commands = args(&["SET a 1", "INCR a", "GET missing"]);
        let replies = vec![Value::Okay, Value::Int(2)];
        let table = format_exec(&commands, &replies, ReplyFormat::Text);
        assert_eq!(
            table,
            "# │ command     │ reply\n──┼─────────────┼───────────\n1 │ SET a 1     │ OK\n2 │ INCR a      │ 2\n3 │ GET missing │ (no reply)"
        );
        let json = format_exec(&commands[..1], &replies[..1], ReplyFormat::Json);
        assert_eq!(
            json,
            "[\n  {\n    \"command\": \"SET a 1\",\n    \"reply\": \"OK\"\n  }\n]"
        );
    }

    #[test]
    fn long_tables_stop_with_a_footer() {
        let items: Vec<Value> = (0..(MAX_TABLE_ROWS + 5)).map(|i| Value::Int(i as i64)).collect();
        let table = format_reply("LRANGE", &args(&[]), &Value::Array(items), ReplyFormat::Table);
        assert!(
            table.ends_with("… 5 more rows"),
            "{}",
            table.lines().last().unwrap_or_default()
        );
    }
}
