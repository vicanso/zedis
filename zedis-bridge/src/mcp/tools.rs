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

use super::*;
use serde_json::Map;
use std::collections::BTreeMap;
use zedis_connection::{
    HotkeyEntry, HotkeysReport, ServerDb, SlowLogEntry, TroubleshootingSnapshot, floors, get_connection_manager,
    is_subcommand, troubleshooting_snapshot,
};
use zedis_core::tally::{memory_by_prefix, totals_by_command};

pub(super) fn list_servers(state: &AppState, account: &str) -> Result<Json, String> {
    let servers: Vec<Json> = get_servers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|server| visible_to(&state.accounts, server, account))
        .map(|server| {
            json!({
                "id": server.id,
                "name": server.name,
                "address": format!("{}:{}", server.host, server.port),
                "tag": server.tag_label(),
            })
        })
        .collect();
    Ok(json!({ "servers": servers }))
}

#[derive(Deserialize)]
pub(super) struct ScanArgs {
    #[serde(default)]
    db: usize,
    #[serde(default = "star")]
    pattern: String,
    limit: Option<u64>,
    cursor: Option<String>,
    #[serde(rename = "type")]
    key_type: Option<String>,
}

fn star() -> String {
    "*".to_string()
}

/// `SCAN` on every master, with a cursor per master carried between calls
/// as `host:port=cursor;…`. A master that has gone since the cursor was
/// handed out is simply not asked again: the two sides discover the
/// topology separately (ADR 9), and resuming its cursor on another node
/// would be a different keyspace.
pub(super) async fn scan_keys(state: &AppState, server: &RedisServer, args: ScanArgs) -> Result<Json, String> {
    let limit = args.limit.unwrap_or(DEFAULT_SCAN_LIMIT).clamp(1, MAX_SCAN_LIMIT);
    let labels = master_labels(server, args.db).await?;
    let cursors: Vec<(String, u64)> = match &args.cursor {
        None => labels.iter().map(|label| (label.clone(), 0)).collect(),
        Some(cursor) => parse_cursor(cursor, &labels)?,
    };
    let (found, cursors) = scan_masters(
        state,
        server,
        args.db,
        &args.pattern,
        args.key_type.as_deref(),
        limit,
        cursors,
    )
    .await?;
    let keys: Vec<String> = found.into_iter().map(|(_, key)| key).collect();
    let next_cursor = (!cursors.is_empty()).then(|| encode_cursor(&cursors));
    Ok(json!({
        "keys": keys,
        "count": keys.len(),
        "next_cursor": next_cursor,
        "complete": cursors.is_empty(),
    }))
}

/// Every master of `server`, as `host:port` — the labels the scan cursors
/// and the per-node commands carry.
async fn master_labels(server: &RedisServer, db: usize) -> Result<Vec<String>, String> {
    let client = get_connection_manager()
        .get_client(&server.id, db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(client
        .master_servers()
        .iter()
        .map(|s| format!("{}:{}", s.host, s.port))
        .collect())
}

/// `SCAN` on every master from `cursors` until about `limit` keys or
/// [`MAX_SCAN_ROUNDS`] rounds: each key with the master it came from, and
/// the cursors of the masters not yet done.
async fn scan_masters(
    state: &AppState,
    server: &RedisServer,
    db: usize,
    pattern: &str,
    key_type: Option<&str>,
    limit: u64,
    mut cursors: Vec<(String, u64)>,
) -> Result<(Vec<(String, String)>, Vec<(String, u64)>), String> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut rounds = 0;
    while !cursors.is_empty() && (found.len() as u64) < limit && rounds < MAX_SCAN_ROUNDS {
        rounds += 1;
        // Shared out over the masters still scanning, so a round brings
        // about `limit` keys in all rather than `limit` from each.
        let remaining = limit - found.len() as u64;
        let count = (remaining / cursors.len() as u64).clamp(10, SCAN_COUNT).to_string();
        let (nodes, commands): (Vec<String>, Vec<Vec<Vec<u8>>>) = cursors
            .iter()
            .map(|(label, cursor)| {
                let cursor = cursor.to_string();
                let mut parts = vec!["SCAN", cursor.as_str(), "MATCH", pattern, "COUNT", &count];
                if let Some(key_type) = key_type {
                    parts.extend(["TYPE", key_type]);
                }
                (label.clone(), words(&parts))
            })
            .unzip();
        let (values, answered) = run(state, server, db, commands, nodes, true).await?;
        let mut next = Vec::new();
        for (label, value) in answered.into_iter().zip(values) {
            let (cursor, page) = scan_reply(&value)?;
            found.extend(page.into_iter().map(|key| (label.clone(), key)));
            if cursor != 0 {
                next.push((label, cursor));
            }
        }
        cursors = next;
    }
    Ok((found, cursors))
}

pub(super) fn encode_cursor(cursors: &[(String, u64)]) -> String {
    cursors
        .iter()
        .map(|(label, cursor)| format!("{label}={cursor}"))
        .collect::<Vec<_>>()
        .join(";")
}

pub(super) fn parse_cursor(text: &str, labels: &[String]) -> Result<Vec<(String, u64)>, String> {
    let mut cursors = Vec::new();
    for part in text.split(';').filter(|p| !p.trim().is_empty()) {
        let (label, cursor) = part
            .rsplit_once('=')
            .ok_or_else(|| format!("cursor {text:?} is not one scan_keys handed out"))?;
        let cursor: u64 = cursor
            .trim()
            .parse()
            .map_err(|_| format!("cursor {text:?} is not one scan_keys handed out"))?;
        if labels.iter().any(|known| known == label.trim()) {
            cursors.push((label.trim().to_string(), cursor));
        }
    }
    Ok(cursors)
}

/// `[cursor, [key, …]]`.
pub(super) fn scan_reply(value: &Value) -> Result<(u64, Vec<String>), String> {
    let Value::Array(parts) = value else {
        return Err(format!("SCAN answered {}", describe(value)));
    };
    let cursor = parts
        .first()
        .and_then(text)
        .and_then(|c| c.parse::<u64>().ok())
        .ok_or_else(|| "SCAN answered without a cursor".to_string())?;
    let keys = match parts.get(1) {
        Some(Value::Array(keys)) => keys.iter().filter_map(text).collect(),
        _ => Vec::new(),
    };
    Ok((cursor, keys))
}

#[derive(Deserialize)]
pub(super) struct KeyArgs {
    #[serde(default)]
    db: usize,
    key: String,
}

/// One key described: what it is, how long it lives, what it costs, and a
/// little of what it holds. Each optional detail is asked separately and
/// simply absent where the server refuses it (`MEMORY`, `OBJECT` on a
/// managed cloud), so a restricted server still answers the rest.
pub(super) async fn inspect_key(state: &AppState, server: &RedisServer, args: KeyArgs) -> Result<Json, String> {
    let key = args.key.as_str();
    let db = args.db;
    let kind = text(&one(state, server, db, words(&["TYPE", key])).await?).unwrap_or_default();
    if kind == "none" {
        return Ok(json!({ "key": key, "exists": false }));
    }
    let ttl = int(&one(state, server, db, words(&["TTL", key])).await?);
    let memory = one(state, server, db, words(&["MEMORY", "USAGE", key, "SAMPLES", "0"]))
        .await
        .ok()
        .and_then(|v| int(&v));
    let encoding = one(state, server, db, words(&["OBJECT", "ENCODING", key]))
        .await
        .ok()
        .and_then(|v| text(&v));
    let items = PREVIEW_ITEMS.to_string();
    let last = (PREVIEW_ITEMS - 1).to_string();
    let last_char = (MAX_STRING_CHARS - 1).to_string();
    let (length_cmd, preview_cmd): (Option<Vec<&str>>, Option<Vec<&str>>) = match kind.as_str() {
        "string" => (Some(vec!["STRLEN", key]), Some(vec!["GETRANGE", key, "0", &last_char])),
        "hash" => (Some(vec!["HLEN", key]), Some(vec!["HSCAN", key, "0", "COUNT", &items])),
        "list" => (Some(vec!["LLEN", key]), Some(vec!["LRANGE", key, "0", &last])),
        "set" => (Some(vec!["SCARD", key]), Some(vec!["SSCAN", key, "0", "COUNT", &items])),
        "zset" => (
            Some(vec!["ZCARD", key]),
            Some(vec!["ZRANGE", key, "0", &last, "WITHSCORES"]),
        ),
        "stream" => (
            Some(vec!["XLEN", key]),
            Some(vec!["XRANGE", key, "-", "+", "COUNT", "20"]),
        ),
        "ReJSON-RL" => (None, Some(vec!["JSON.GET", key])),
        _ => (None, None),
    };
    let length = match length_cmd {
        Some(cmd) => int(&one(state, server, db, words(&cmd)).await?),
        None => None,
    };
    let preview = match preview_cmd {
        Some(cmd) => Some(render(&one(state, server, db, words(&cmd)).await?)),
        None => None,
    };
    // A scan's preview is `[cursor, [items]]`: the items alone, as pairs
    // for a hash.
    let preview = preview.map(|p| match kind.as_str() {
        "hash" | "set" => p.get(1).cloned().unwrap_or(p),
        _ => p,
    });
    let preview = preview.map(|p| if kind == "hash" { pairs(p) } else { p });
    let partial = match (kind.as_str(), length) {
        ("string", Some(len)) => len as usize > MAX_STRING_CHARS,
        ("stream", Some(len)) => len > 20,
        (_, Some(len)) => len > PREVIEW_ITEMS as i64,
        _ => false,
    };
    Ok(json!({
        "key": key,
        "exists": true,
        "type": kind,
        "ttl_seconds": ttl.filter(|ttl| *ttl >= 0),
        "expires": ttl.is_some_and(|ttl| ttl >= 0),
        "memory_bytes": memory,
        "encoding": encoding,
        "length": length,
        "preview": preview,
        "preview_is_partial": partial,
    }))
}

/// `[f, v, f, v]` as `{f: v}`.
pub(super) fn pairs(flat: Json) -> Json {
    let Json::Array(items) = flat else {
        return flat;
    };
    let mut map = Map::new();
    for pair in items.chunks(2) {
        if let [field, value] = pair {
            let field = field.as_str().map_or_else(|| field.to_string(), str::to_string);
            map.insert(field, value.clone());
        }
    }
    Json::Object(map)
}

#[derive(Deserialize)]
pub(super) struct InfoArgs {
    #[serde(default = "default_section")]
    section: String,
}

fn default_section() -> String {
    "default".to_string()
}

pub(super) async fn server_info(state: &AppState, server: &RedisServer, args: InfoArgs) -> Result<Json, String> {
    let section = args.section.trim();
    if section.is_empty() || !section.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!("section {section:?} is not an INFO section name"));
    }
    let (values, nodes) = run(state, server, 0, vec![words(&["INFO", section])], Vec::new(), true).await?;
    let nodes: Vec<Json> = nodes
        .iter()
        .zip(values.iter())
        .map(|(node, value)| json!({ "node": node, "info": parse_info(&text(value).unwrap_or_default()) }))
        .collect();
    Ok(json!({ "nodes": nodes }))
}

/// `INFO`'s text as sections of fields, numbers as numbers.
pub(super) fn parse_info(text: &str) -> Json {
    let mut sections: BTreeMap<String, Map<String, Json>> = BTreeMap::new();
    let mut current = String::from("other");
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('#') {
            current = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .parse::<i64>()
            .map(Json::from)
            .or_else(|_| value.parse::<f64>().map(Json::from))
            .unwrap_or_else(|_| Json::String(cut_text(value)));
        sections
            .entry(current.clone())
            .or_default()
            .insert(field.to_string(), value);
    }
    json!(sections)
}

#[derive(Deserialize)]
pub(super) struct SlowlogArgs {
    count: Option<u64>,
}

pub(super) async fn slowlog(state: &AppState, server: &RedisServer, args: SlowlogArgs) -> Result<Json, String> {
    let count = args.count.unwrap_or(DEFAULT_SLOWLOG).clamp(1, MAX_SLOWLOG).to_string();
    let (values, nodes) = run(
        state,
        server,
        0,
        vec![words(&["SLOWLOG", "GET", &count])],
        Vec::new(),
        true,
    )
    .await?;
    let nodes: Vec<Json> = nodes
        .iter()
        .zip(values.iter())
        .map(|(node, value)| {
            let entries: Vec<Json> = match value {
                Value::Array(entries) => entries.iter().map(slowlog_entry).collect(),
                _ => Vec::new(),
            };
            json!({ "node": node, "entries": entries })
        })
        .collect();
    Ok(json!({ "nodes": nodes }))
}

/// `[id, unix time, microseconds, [command…], client, name]`.
pub(super) fn slowlog_entry(entry: &Value) -> Json {
    let Value::Array(parts) = entry else {
        return render(entry);
    };
    let command = match parts.get(3) {
        Some(Value::Array(args)) => Json::Array(args.iter().map(render).collect()),
        _ => Json::Null,
    };
    json!({
        "id": parts.first().and_then(int),
        "unix_time": parts.get(1).and_then(int),
        "duration_us": parts.get(2).and_then(int),
        "command": command,
        "client": parts.get(4).and_then(text),
        "client_name": parts.get(5).and_then(text),
    })
}

/// A share in percent, to one decimal.
fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 * 1000.0 / total as f64).round() / 10.0
    }
}

/// The slow log of every master, grouped by command (`zedis_core::tally`,
/// the desktop's Top Commands), with the slowest entries quoted.
pub(super) async fn slowlog_summary(state: &AppState, server: &RedisServer, args: SlowlogArgs) -> Result<Json, String> {
    let count = args.count.unwrap_or(MAX_SLOWLOG).clamp(1, MAX_SLOWLOG).to_string();
    let (values, nodes) = run(
        state,
        server,
        0,
        vec![words(&["SLOWLOG", "GET", &count])],
        Vec::new(),
        true,
    )
    .await?;
    // (node, command name, microseconds, the entry as `slowlog` shows it)
    let mut entries: Vec<(String, String, u64, Json)> = Vec::new();
    for (node, value) in nodes.iter().zip(&values) {
        let Value::Array(list) = value else { continue };
        for entry in list {
            let shown = slowlog_entry(entry);
            let command = shown["command"]
                .get(0)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_ascii_uppercase();
            let micros = shown["duration_us"].as_u64().unwrap_or(0);
            entries.push((node.clone(), command, micros, shown));
        }
    }
    let totals = totals_by_command(
        entries
            .iter()
            .map(|(_, command, micros, _)| (command.as_str(), *micros)),
    );
    let by_command: Vec<Json> = totals
        .iter()
        .take(SUMMARY_COMMANDS)
        .map(|total| {
            json!({
                "command": total.command,
                "count": total.count,
                "total_us": total.total,
                "max_us": total.max,
                "avg_us": total.total / total.count.max(1) as u64,
                "share_pct": (total.share_pct * 10.0).round() / 10.0,
            })
        })
        .collect();
    entries.sort_by_key(|(_, _, micros, _)| std::cmp::Reverse(*micros));
    let slowest: Vec<Json> = entries
        .iter()
        .take(SUMMARY_SLOWEST)
        .map(|(node, _, _, shown)| {
            let mut shown = shown.clone();
            shown["node"] = json!(node);
            shown
        })
        .collect();
    Ok(json!({
        "entries": entries.len(),
        "masters": nodes.len(),
        "by_command": by_command,
        "commands_in_all": totals.len(),
        "slowest": slowest,
    }))
}

#[derive(Deserialize)]
pub(super) struct HotkeysArgs {
    limit: Option<usize>,
}

/// The report of the server's `HOTKEYS` collection, merged over every master
/// (`HotkeysReport`, as the Hot Keys panel reads it). It only reads: starting
/// a collection changes the server, so that is left to a person.
pub(super) async fn hotkeys(state: &AppState, server: &RedisServer, args: HotkeysArgs) -> Result<Json, String> {
    let client = get_connection_manager()
        .get_client(&server.id, 0)
        .await
        .map_err(|e| e.to_string())?;
    if !client.supports(floors::HOTKEYS) {
        return Err("HOTKEYS is a Redis 8.6 command, and this server does not have it".to_string());
    }
    let (values, _) = run(state, server, 0, vec![words(&["HOTKEYS", "GET"])], Vec::new(), true).await?;
    let mut report = HotkeysReport::default();
    for value in &values {
        report.merge_node_reply(value);
    }
    report.sort();
    if report.is_empty() {
        return Ok(json!({
            "collected": false,
            "message": "No HOTKEYS collection has run on this server. Start one from Zedis's Hot Keys panel (or HOTKEYS START), let it run, then ask again.",
        }));
    }
    let limit = args.limit.unwrap_or(DEFAULT_HOTKEYS).clamp(1, MAX_HOTKEYS);
    let listed = |list: &[HotkeyEntry], total: u64| -> Vec<Json> {
        list.iter()
            .take(limit)
            .map(|entry| json!({ "key": cut_text(&entry.key), "value": entry.value, "share_pct": percent(entry.value, total) }))
            .collect()
    };
    Ok(json!({
        "collected": true,
        "tracking_active": report.tracking_active,
        "sample_ratio": report.sample_ratio,
        "duration_ms": report.collection_duration_ms,
        "total_cpu_us": report.total_cpu_us,
        "total_net_bytes": report.total_net_bytes,
        "by_cpu_us": listed(&report.by_cpu, report.total_cpu_us),
        "by_net_bytes": listed(&report.by_net, report.total_net_bytes),
    }))
}

#[derive(Deserialize)]
pub(super) struct MemoryArgs {
    #[serde(default)]
    db: usize,
    #[serde(default = "star")]
    pattern: String,
    sample: Option<u64>,
    separator: Option<String>,
}

/// A sample of keys measured and grouped by prefix (`zedis_core::tally`).
/// Bounded twice: by `sample` keys, and by the scan's own rounds, so one
/// call costs a production server a few thousand cheap commands at most.
pub(super) async fn memory_summary(state: &AppState, server: &RedisServer, args: MemoryArgs) -> Result<Json, String> {
    let sample = args.sample.unwrap_or(DEFAULT_MEMORY_SAMPLE).clamp(1, MAX_MEMORY_SAMPLE);
    let separator = args.separator.unwrap_or_else(|| ":".to_string());
    let start = master_labels(server, args.db)
        .await?
        .into_iter()
        .map(|label| (label, 0))
        .collect();
    let (found, unscanned) = scan_masters(state, server, args.db, &args.pattern, None, sample, start).await?;
    // Complete when the scan ran out before the sample did: SCAN's COUNT is
    // a hint, so a page can bring more than asked, and those are cut.
    let complete = unscanned.is_empty() && (found.len() as u64) <= sample;
    let found: Vec<(String, String)> = found.into_iter().take(sample as usize).collect();

    // (key, type, bytes)
    let mut measured: Vec<(String, String, u64)> = Vec::new();
    for batch in found.chunks(MEMORY_BATCH) {
        let mut nodes = Vec::with_capacity(batch.len() * 2);
        let mut commands = Vec::with_capacity(batch.len() * 2);
        for (label, key) in batch {
            nodes.push(label.clone());
            commands.push(words(&["TYPE", key]));
            nodes.push(label.clone());
            commands.push(words(&["MEMORY", "USAGE", key]));
        }
        let (values, answered) = run(state, server, args.db, commands, nodes, true).await?;
        measured.extend(pair_measurements(batch, answered, values));
    }
    if measured.is_empty() && !found.is_empty() {
        return Err("MEMORY USAGE answered nothing for the keys found; the server may not allow it".to_string());
    }

    let total: u64 = measured.iter().map(|(_, _, bytes)| bytes).sum();
    let groups = memory_by_prefix(
        measured
            .iter()
            .map(|(key, key_type, bytes)| (key.as_str(), key_type.as_str(), *bytes)),
        &separator,
    );
    let prefixes: Vec<Json> = groups
        .iter()
        .take(MEMORY_TOP_PREFIXES)
        .map(|group| {
            json!({
                "prefix": cut_text(&group.prefix),
                "keys": group.keys,
                "bytes": group.bytes,
                "avg_bytes": group.bytes / group.keys.max(1),
                "share_pct": percent(group.bytes, total),
                "types": group.types,
            })
        })
        .collect();
    let mut largest: Vec<&(String, String, u64)> = measured.iter().collect();
    largest.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let largest: Vec<Json> = largest
        .into_iter()
        .take(MEMORY_TOP_KEYS)
        .map(|(key, key_type, bytes)| json!({ "key": cut_text(key), "type": key_type, "bytes": bytes }))
        .collect();
    Ok(json!({
        "measured_keys": measured.len(),
        "measured_bytes": total,
        "complete": complete,
        "note": if complete {
            "Every key matching the pattern was measured."
        } else {
            "A sample: the first keys SCAN returned, not the whole keyspace. Shares are of the sample."
        },
        "separator": separator,
        "prefix_count": groups.len(),
        "prefixes": prefixes,
        "largest_keys": largest,
    }))
}

/// Each key of `batch` with its type and size, from the replies to its
/// `TYPE` and `MEMORY USAGE`. Each master answers its own commands in the
/// order it got them — a key's `TYPE`, then its `MEMORY USAGE` — and the
/// masters one after another, so the pairs are put back together per
/// master. A key gone since the scan (`none`, nil), or one `MEMORY` was
/// refused for, is left out.
pub(super) fn pair_measurements(
    batch: &[(String, String)],
    answered: Vec<String>,
    values: Vec<Value>,
) -> Vec<(String, String, u64)> {
    let mut queues: HashMap<&str, VecDeque<&str>> = HashMap::new();
    for (label, key) in batch {
        queues.entry(label).or_default().push_back(key);
    }
    let mut types: HashMap<String, String> = HashMap::new();
    let mut measured = Vec::new();
    for (label, value) in answered.into_iter().zip(values) {
        match types.remove(&label) {
            None => {
                types.insert(label, text(&value).unwrap_or_default());
            }
            Some(key_type) => {
                let Some(key) = queues.get_mut(label.as_str()).and_then(VecDeque::pop_front) else {
                    continue;
                };
                if let Some(bytes) = int(&value).filter(|bytes| *bytes >= 0)
                    && key_type != "none"
                {
                    measured.push((key.to_string(), key_type, bytes as u64));
                }
            }
        }
    }
    measured
}

#[derive(Deserialize)]
pub(super) struct TroubleshootArgs {
    #[serde(default)]
    db: usize,
}

pub(super) async fn troubleshooting_report(server: &RedisServer, args: TroubleshootArgs) -> Result<Json, String> {
    let snapshot = troubleshooting_snapshot(&ServerDb::new(&server.id, args.db)).await;
    Ok(format_troubleshooting_snapshot(server, args.db, &snapshot))
}

pub(super) fn format_troubleshooting_snapshot(
    server: &RedisServer,
    db: usize,
    snapshot: &TroubleshootingSnapshot,
) -> Json {
    let summary = match &snapshot.summary {
        Ok(summary) => json!({
            "version": summary.version,
            "flavor": summary.description.flavor.label(),
            "flavor_version": summary.description.flavor_version,
            "server_type": summary.description.server_type,
            "masters": summary.nodes.0,
            "total_nodes": summary.nodes.1,
            "dbsize": summary.dbsize,
        }),
        Err(reason) => json!({ "error": reason }),
    };
    let nodes = match &snapshot.info {
        Ok(infos) => json!(
            infos
                .iter()
                .map(|(node, text)| json!({ "node": node, "info": parse_info(text) }))
                .collect::<Vec<_>>()
        ),
        Err(reason) => json!({ "error": reason }),
    };
    let memory_doctor = match &snapshot.memory_doctor {
        Ok(replies) => json!(
            replies
                .iter()
                .map(|r| json!({ "node": r.node, "report": cut_text(&r.value) }))
                .collect::<Vec<_>>()
        ),
        Err(reason) => json!({ "error": reason }),
    };
    let latency_doctor = match &snapshot.latency_doctor {
        Ok(replies) => json!(
            replies
                .iter()
                .map(|r| json!({ "node": r.node, "report": cut_text(&r.value) }))
                .collect::<Vec<_>>()
        ),
        Err(reason) => json!({ "error": reason }),
    };
    let latency = match &snapshot.latency {
        Ok(listing) => json!({
            "unsupported": listing.unsupported,
            "events": listing.events.iter().map(|e| json!({
                "event": e.event,
                "timestamp": e.timestamp,
                "latest_ms": e.latest_ms,
                "max_ms": e.max_ms,
                "avg_ms": e.avg_ms,
                "avg_samples": e.avg_samples,
            })).collect::<Vec<_>>(),
        }),
        Err(reason) => json!({ "error": reason }),
    };
    let slow_log = match &snapshot.slow_log {
        Ok(entries) => {
            let upper: Vec<(String, u64)> = entries
                .iter()
                .map(|entry| {
                    let cmd = entry.args.first().map(|s| s.to_ascii_uppercase()).unwrap_or_default();
                    (cmd, entry.duration.as_micros() as u64)
                })
                .collect();
            let totals = totals_by_command(upper.iter().map(|(cmd, micros)| (cmd.as_str(), *micros)));
            let by_command: Vec<Json> = totals
                .iter()
                .take(SUMMARY_COMMANDS)
                .map(|total| {
                    json!({
                        "command": total.command,
                        "count": total.count,
                        "total_us": total.total,
                        "max_us": total.max,
                        "avg_us": total.total / total.count.max(1) as u64,
                        "share_pct": (total.share_pct * 10.0).round() / 10.0,
                    })
                })
                .collect();
            let mut newest: Vec<&SlowLogEntry> = entries.iter().collect();
            newest.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
            newest.truncate(REPORT_SLOWLOG_ROWS);
            let recent: Vec<Json> = newest
                .into_iter()
                .map(|entry| {
                    json!({
                        "id": entry.id,
                        "unix_time": entry.timestamp,
                        "duration_us": entry.duration.as_micros() as u64,
                        "command": redact_slow_command(entry),
                        "client": entry.client_addr,
                        "client_name": entry.client_name,
                    })
                })
                .collect();
            json!({
                "total_entries": entries.len(),
                "by_command": by_command,
                "newest": recent,
            })
        }
        Err(reason) => json!({ "error": reason }),
    };
    let config = match &snapshot.config {
        Ok(pairs) => {
            let mut map = Map::new();
            for (name, value) in pairs {
                map.insert(
                    name.clone(),
                    value.as_ref().map_or(Json::Null, |v| Json::String(cut_text(v))),
                );
            }
            Json::Object(map)
        }
        Err(reason) => json!({ "error": reason }),
    };
    json!({
        "server": {
            "id": server.id,
            "name": server.name,
            "address": format!("{}:{}", server.host, server.port),
            "db": db,
        },
        "summary": summary,
        "nodes": nodes,
        "memory_doctor": memory_doctor,
        "latency_doctor": latency_doctor,
        "latency": latency,
        "slow_log": slow_log,
        "config": config,
    })
}

/// A slow command as ADR 19's troubleshooting report shows it: its name (and
/// subcommand where applicable) plus its first argument capped at
/// [`REPORT_ARG_CHARS`], with credentials blanked via [`redact_secrets`] and
/// values omitted.
fn redact_slow_command(entry: &SlowLogEntry) -> String {
    let Some((name, args)) = entry.args.split_first() else {
        return String::new();
    };
    let args = redact_secrets(name, args);
    let mut shown = name.to_ascii_uppercase();
    let mut rest = args.as_slice();
    if let Some((sub, after)) = rest.split_first()
        && is_subcommand(name, sub)
    {
        shown = format!("{shown} {}", sub.to_ascii_uppercase());
        rest = after;
    }
    if let Some(first) = rest.first() {
        shown.push(' ');
        shown.extend(first.chars().take(REPORT_ARG_CHARS));
        if first.chars().count() > REPORT_ARG_CHARS {
            shown.push('…');
        }
    }
    if rest.len() > 1 {
        shown.push_str(&format!(" … (+{})", rest.len() - 1));
    }
    shown
}

#[derive(Deserialize)]
pub(super) struct CommandArgs {
    #[serde(default)]
    db: usize,
    command: Vec<String>,
    #[serde(default)]
    every_master: bool,
}

pub(super) async fn read_command(state: &AppState, server: &RedisServer, args: CommandArgs) -> Result<Json, String> {
    if args.command.is_empty() || args.command[0].trim().is_empty() {
        return Err("command is the command and its arguments, one word each".to_string());
    }
    let frame: Vec<Vec<u8>> = args.command.iter().map(|w| w.as_bytes().to_vec()).collect();
    let (values, nodes) = run(state, server, args.db, vec![frame], Vec::new(), args.every_master).await?;
    if args.every_master {
        let replies: Vec<Json> = nodes
            .iter()
            .zip(values.iter())
            .map(|(node, value)| json!({ "node": node, "reply": render(value) }))
            .collect();
        return Ok(json!({ "replies": replies }));
    }
    Ok(json!({ "reply": values.first().map(render) }))
}

/// A reply as JSON a model can read, cut where it is long: strings at
/// [`MAX_STRING_CHARS`], arrays and maps at [`MAX_ITEMS`], with what was
/// dropped counted in place.
pub(super) fn render(value: &Value) -> Json {
    match value {
        Value::Nil => Json::Null,
        Value::Int(i) => json!(i),
        Value::Double(d) => json!(d),
        Value::Boolean(b) => json!(b),
        Value::Okay => json!("OK"),
        Value::SimpleString(s) => Json::String(cut_text(s)),
        Value::BulkString(bytes) => Json::String(cut_text(&String::from_utf8_lossy(bytes))),
        Value::VerbatimString { text, .. } => Json::String(cut_text(text)),
        Value::BigNumber(n) => json!(format!("{n:?}")),
        Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => {
            let mut out: Vec<Json> = items.iter().take(MAX_ITEMS).map(render).collect();
            if items.len() > MAX_ITEMS {
                out.push(json!(format!("…(+{} more)", items.len() - MAX_ITEMS)));
            }
            Json::Array(out)
        }
        Value::Map(entries) => {
            let mut out = Map::new();
            for (key, value) in entries.iter().take(MAX_ITEMS) {
                let key = text(key).unwrap_or_else(|| describe(key));
                out.insert(key, render(value));
            }
            if entries.len() > MAX_ITEMS {
                out.insert("…".to_string(), json!(format!("+{} more", entries.len() - MAX_ITEMS)));
            }
            Json::Object(out)
        }
        Value::Attribute { data, .. } => render(data),
        Value::ServerError(e) => json!({ "error": e.to_string() }),
        // The enum is non-exhaustive: a shape a later redis adds is shown
        // as the driver prints it rather than dropped.
        other => json!(format!("{other:?}")),
    }
}

/// A short account of a reply's shape, for a message.
fn describe(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Array(items) => format!("an array of {}", items.len()),
        Value::ServerError(e) => format!("an error: {e}"),
        other => text(other).unwrap_or_else(|| format!("{other:?}")),
    }
}

/// The string a reply holds, when it holds one.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::SimpleString(s) => Some(s.clone()),
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        Value::Okay => Some("OK".to_string()),
        Value::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

fn int(value: &Value) -> Option<i64> {
    match value {
        Value::Int(i) => Some(*i),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().ok(),
        _ => None,
    }
}

/// `s` cut at [`MAX_STRING_CHARS`], with the rest counted.
fn cut_text(s: &str) -> String {
    let chars = s.chars().count();
    if chars <= MAX_STRING_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_STRING_CHARS).collect();
    format!("{head}…(+{} chars)", chars - MAX_STRING_CHARS)
}

/// A whole result cut at [`MAX_TEXT_BYTES`], on a character boundary.
pub(super) fn cap_text(text: String) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n…(the result was cut at {} KiB)",
        &text[..end],
        MAX_TEXT_BYTES / 1024
    )
}
