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

//! The MCP entry point: an AI assistant as one more read-only account
//! (ADR 15).
//!
//! `POST /v1/mcp` speaks the Model Context Protocol's JSON-RPC over plain
//! HTTP — one request, one JSON reply, no server-initiated stream — the
//! subset a tools-only server needs. Hand-rolled: it is five methods, and
//! the bridge's policy layer, not the protocol surface, is the point.
//!
//! Nothing here is a second door. The caller signs in with one of its
//! account's MCP tokens (`Authorization: Bearer zdt_…`, `api::authorize_mcp`)
//! or the way a script does (HTTP Basic), is refused unless the account is
//! read-only, and each tool
//! turns into commands that go through `policy::check` with the read-only
//! role fixed and out through `forward_values`, the page's own path. What
//! the tools add is shape: a key described with a short preview instead of
//! a raw reply, `INFO` parsed per master, results cut so a large value does
//! not land whole in a model's context. Every call is one audit line
//! (`Event::Tool`), whatever it read — a program acting for someone is not a
//! person looking, and reads are the whole of what it does.

mod tools;

use crate::api::{ApiError, AppState, ExecRequest, authorize_mcp, forward_values, note_server, visible_to};
use crate::audit::{self, Event, Origin, ServerRef};
use crate::policy;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use redis::Value;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tools::{
    cap_text, hotkeys, inspect_key, list_servers, memory_summary, read_command, scan_keys, server_info, slowlog,
    slowlog_summary, troubleshooting_report,
};
use zedis_connection::{RedisServer, get_servers, redact_secrets};
/// The protocol revisions this server answers. A client that asks for one
/// of them is answered in kind; any other request gets the newest.
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL: &str = "2025-06-18";

/// How many tool calls one account may make in a minute. A model in a loop
/// scans a keyspace faster than a person ever clicks, and the number is
/// generous for a conversation and mean for a runaway.
pub const CALLS_PER_MINUTE: usize = 120;
const WINDOW: Duration = Duration::from_secs(60);

/// What `scan_keys` returns when not told, and the most it will.
const DEFAULT_SCAN_LIMIT: u64 = 100;
const MAX_SCAN_LIMIT: u64 = 1000;
/// `SCAN` rounds one `scan_keys` call makes before handing back a cursor:
/// a sparse pattern over a large keyspace would otherwise walk it whole.
const MAX_SCAN_ROUNDS: usize = 64;
const SCAN_COUNT: u64 = 200;
/// Elements a key's preview shows.
const PREVIEW_ITEMS: u64 = 50;
/// Where a string in a result is cut, in characters.
const MAX_STRING_CHARS: usize = 4096;
/// Where an array or map in a result is cut.
const MAX_ITEMS: usize = 1000;
/// Where a whole result is cut, in bytes.
const MAX_TEXT_BYTES: usize = 200 * 1024;
const DEFAULT_SLOWLOG: u64 = 25;
const MAX_SLOWLOG: u64 = 128;
/// Commands `slowlog_summary` ranks, and the slowest entries it quotes.
const SUMMARY_COMMANDS: usize = 30;
const SUMMARY_SLOWEST: usize = 5;
/// Keys `hotkeys` lists per metric, when not told, and at most.
const DEFAULT_HOTKEYS: usize = 20;
const MAX_HOTKEYS: usize = 100;
/// Keys `memory_summary` measures, when not told, and at most: a sample,
/// so the cost of one call on a production server stays bounded.
const DEFAULT_MEMORY_SAMPLE: u64 = 1000;
const MAX_MEMORY_SAMPLE: u64 = 5000;
/// Keys measured per pipeline to one master.
const MEMORY_BATCH: usize = 250;
/// Prefixes and keys `memory_summary` lists.
const MEMORY_TOP_PREFIXES: usize = 30;
const MEMORY_TOP_KEYS: usize = 10;
/// Slow log entries `troubleshooting_report` lists, and where a command's
/// first argument is cut.
const REPORT_SLOWLOG_ROWS: usize = 20;
const REPORT_ARG_CHARS: usize = 80;

/// What a full account is told at this door.
const WRITE_ACCOUNT_REFUSAL: &str = "the MCP entry point is for read-only accounts: set this account's role to read-only on the Account page (or give it read_only = true in the users file)";

const INSTRUCTIONS: &str = "Read-only access to the Redis and Valkey servers this zedis-bridge holds, as the \
signed-in account sees them. Start with list_servers; the other tools take a server by its name or id. \
scan_keys pages through keys (pass next_cursor back to continue), inspect_key describes one key with a \
short preview, server_info and slowlog read INFO and SLOWLOG GET on every master, slowlog_summary ranks the \
slow log by command, hotkeys reads the report of a HOTKEYS collection already running (Redis 8.6+), \
memory_summary measures a sample of keys and groups their memory by prefix, troubleshooting_report gathers a \
single-shot diagnostic snapshot (summary, INFO, MEMORY DOCTOR, LATENCY DOCTOR, LATENCY LATEST, redacted slow log, \
and key server settings), and read_command runs any other read-only command. Writes and administration are refused, \
results are cut to a size that fits a context, and every call is written to the bridge's audit log.";

/// The tools, in the order `tools/list` names them.
const TOOLS: [&str; 10] = [
    "list_servers",
    "scan_keys",
    "inspect_key",
    "server_info",
    "slowlog",
    "slowlog_summary",
    "hotkeys",
    "memory_summary",
    "troubleshooting_report",
    "read_command",
];

// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Calls per account in the last minute: a sliding window, swept as it is
/// asked. Cloned into the router state.
#[derive(Clone, Default)]
pub struct Limiter(Arc<Mutex<HashMap<String, VecDeque<Instant>>>>);

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `account` may make a call now; counted when it may.
    pub fn allow(&self, account: &str) -> bool {
        self.allow_at(account, Instant::now())
    }

    fn allow_at(&self, account: &str, now: Instant) -> bool {
        let mut calls = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let recent = calls.entry(account.to_string()).or_default();
        while recent.front().is_some_and(|at| now.duration_since(*at) >= WINDOW) {
            recent.pop_front();
        }
        if recent.len() >= CALLS_PER_MINUTE {
            return false;
        }
        recent.push_back(now);
        true
    }
}

/// `GET` and `DELETE` on the endpoint: the stream and the session this
/// server does not have, answered as the protocol allows.
pub async fn not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

/// One JSON-RPC request in, one reply out.
pub async fn post(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut origin = Origin::new(peer, &headers);
    origin.via = Some("mcp");
    let account = match authorize_mcp(&state, &headers, &mut origin).await {
        Ok(account) => account,
        Err(refusal) => return refusal.into_response(),
    };
    // The role is a hard condition of the door, not a per-command check
    // alone: an account that may write anywhere is not one to hand a
    // program, whatever the program is asked to do.
    if !state.accounts.is_read_only(&account) {
        state.audit.record(&account, &origin, Event::Refused { action: "mcp" });
        return ApiError::Forbidden(WRITE_ACCOUNT_REFUSAL.to_string()).into_response();
    }
    let request: Json = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => {
            return reply(
                StatusCode::BAD_REQUEST,
                rpc_error(Json::Null, PARSE_ERROR, format!("the body is not JSON: {e}")),
            );
        }
    };
    let rpc = match Rpc::parse(request) {
        Ok(rpc) => rpc,
        // The request's own id where it had one: a client matches replies
        // to requests by it, and a null answered nothing it had sent.
        Err((id, error)) => return reply(StatusCode::OK, rpc_error(id, error.code, error.message)),
    };
    // A notification has no id and gets no reply: `notifications/initialized`
    // after the handshake, `notifications/cancelled` for a call that is
    // over before the cancel arrives.
    let Some(id) = rpc.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match rpc.method.as_str() {
        "initialize" => Ok(initialize(&rpc.params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => call(&state, &account, &origin, rpc.params).await,
        other => Err(RpcError::new(METHOD_NOT_FOUND, format!("unknown method {other:?}"))),
    };
    reply(
        StatusCode::OK,
        match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => rpc_error(id, error.code, error.message),
        },
    )
}

fn reply(status: StatusCode, body: Json) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

fn rpc_error(id: Json, code: i64, message: String) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
}

/// One request, taken apart. `id` is `None` for a notification.
struct Rpc {
    id: Option<Json>,
    method: String,
    params: Json,
}

impl Rpc {
    /// The request, or why it is not one with the id to answer under —
    /// its own where it named one (a string or a number), else null.
    fn parse(request: Json) -> Result<Self, (Json, RpcError)> {
        let Json::Object(mut fields) = request else {
            let what = if request.is_array() {
                "one request at a time: JSON-RPC batches are not supported"
            } else {
                "a request is a JSON object"
            };
            return Err((Json::Null, RpcError::new(INVALID_REQUEST, what)));
        };
        let id = fields.remove("id").filter(|id| !id.is_null());
        let method = match fields.remove("method") {
            Some(Json::String(method)) => method,
            _ => {
                let answer_to = id.filter(|id| id.is_string() || id.is_number()).unwrap_or(Json::Null);
                return Err((answer_to, RpcError::new(INVALID_REQUEST, "a request names a method")));
            }
        };
        let params = fields.remove("params").unwrap_or_else(|| json!({}));
        Ok(Self { id, method, params })
    }
}

fn initialize(params: &Json) -> Json {
    let asked = params.get("protocolVersion").and_then(Json::as_str);
    let version = asked.filter(|v| PROTOCOLS.contains(v)).unwrap_or(LATEST_PROTOCOL);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "zedis-bridge", "version": env!("CARGO_PKG_VERSION") },
        "instructions": format!(
            "{INSTRUCTIONS} Each account may make {CALLS_PER_MINUTE} calls a minute; past that a call answers with an error saying to wait."
        ),
    })
}

/// The `tools/list` reply: every tool with the schema of its arguments.
/// Each is marked read-only, which is what it is, so a client that shows
/// annotations need not ask before calling.
fn tool_list() -> Vec<Json> {
    let server = json!({ "type": "string", "description": "The server's name or id, as list_servers shows it." });
    let db = json!({ "type": "integer", "minimum": 0, "default": 0, "description": "The database number." });
    let read_only = json!({ "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false });
    vec![
        json!({
            "name": "list_servers",
            "title": "List servers",
            "description": "The servers this account may read, each with its id, name, address (host:port) and tag. Call it first: every other tool takes a server by the name or id shown here.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": read_only,
        }),
        json!({
            "name": "scan_keys",
            "title": "Scan keys",
            "description": "Page through the keys matching a glob pattern, across every master of a cluster. Returns the keys and a next_cursor to pass back for the next page; null when the scan is complete.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "db": db,
                    "pattern": { "type": "string", "default": "*", "description": "A glob pattern: user:*, *:session, order:?:2026*." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SCAN_LIMIT, "default": DEFAULT_SCAN_LIMIT, "description": "About how many keys a page holds; SCAN's COUNT is a hint, so a page may run a little over." },
                    "cursor": { "type": "string", "description": "The next_cursor of the previous page." },
                    "type": { "type": "string", "description": "Only keys of this type: string, hash, list, set, zset, stream." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "inspect_key",
            "title": "Inspect a key",
            "description": format!("Describe one key: type, ttl_seconds (null when it does not expire), memory_bytes, encoding, length and a preview of the value — about {PREVIEW_ITEMS} items of a collection, 20 entries of a stream, the first {MAX_STRING_CHARS} characters of a string — with preview_is_partial set when there is more. A missing key answers exists: false; memory_bytes and encoding are null where the server does not report them."),
            "inputSchema": {
                "type": "object",
                "properties": { "server": server, "db": db, "key": { "type": "string", "description": "The key's exact name, not a pattern." } },
                "required": ["server", "key"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "server_info",
            "title": "Server info",
            "description": "INFO from every master, parsed into sections. Ask for one section (server, clients, memory, persistence, stats, replication, cpu, keyspace, …) or all.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "section": { "type": "string", "default": "default", "description": "An INFO section name, or all." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "slowlog",
            "title": "Slow log",
            "description": "The newest entries of SLOWLOG GET on every master, grouped by node: each with its id, unix_time, duration_us, the command's words, and the client's address and name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "count": { "type": "integer", "minimum": 1, "maximum": MAX_SLOWLOG, "default": DEFAULT_SLOWLOG, "description": "How many entries to read from each master." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "slowlog_summary",
            "title": "Slow log by command",
            "description": format!("The slow log of every master grouped by command: for each, how many entries, the total, longest and average duration in microseconds and its share of the total — the heaviest first, at most {SUMMARY_COMMANDS} — plus the {SUMMARY_SLOWEST} slowest entries themselves."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "count": { "type": "integer", "minimum": 1, "maximum": MAX_SLOWLOG, "default": MAX_SLOWLOG, "description": "How many entries to read from each master." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "hotkeys",
            "title": "Hot keys",
            "description": "The report of the HOTKEYS collection on the server (Redis 8.6+), merged over every master: the keys using the most CPU time and the most network bytes, each with its share. It reads a collection someone started — from Zedis's Hot Keys panel, or with HOTKEYS START — and does not start one, since that changes the server; with none it says so.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_HOTKEYS, "default": DEFAULT_HOTKEYS, "description": "How many keys to list for each metric." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "memory_summary",
            "title": "Memory by key prefix",
            "description": format!("Measure a sample of keys (SCAN, then TYPE and MEMORY USAGE on each, on every master) and group their memory by key prefix — everything up to the last separator, user:profile:42 counting under user:profile:* — the heaviest {MEMORY_TOP_PREFIXES} prefixes with their key count, bytes, average and types, and the {MEMORY_TOP_KEYS} largest keys. complete says whether every matching key was measured; otherwise it is the first keys SCAN returned, and shares are of the sample."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "db": db,
                    "pattern": { "type": "string", "default": "*", "description": "Only keys matching this glob pattern." },
                    "sample": { "type": "integer", "minimum": 1, "maximum": MAX_MEMORY_SAMPLE, "default": DEFAULT_MEMORY_SAMPLE, "description": "How many keys to measure at most." },
                    "separator": { "type": "string", "default": ":", "description": "What separates the parts of a key name." }
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "troubleshooting_report",
            "title": "Troubleshooting report",
            "description": "Collect a single-shot, value-free diagnostic snapshot of the server across every master (ADR 19): server summary (flavor, version, topology, dbsize), INFO per node, MEMORY DOCTOR, LATENCY DOCTOR, LATENCY LATEST events, the newest slow log entries (redacted to command and first argument only, plus command totals), and the 12 core memory/client/persistence/slowlog/latency settings. Parts that a managed server refuses are reported individually without failing the snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "db": db
                },
                "required": ["server"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
        json!({
            "name": "read_command",
            "title": "Run a read-only command",
            "description": format!("Run one read-only Redis command, given as its words (for example [\"HGETALL\", \"user:42\"]). Refused: writes, scripts other than EVAL_RO / EVALSHA_RO / FCALL_RO, administration, reads that return credentials (CONFIG GET of a password, ACL GETUSER / LIST), and commands that change or hold the connection the tools share (SELECT — pass db instead — AUTH, HELLO, MULTI / EXEC / WATCH, blocking reads such as BLPOP and XREAD BLOCK, SUBSCRIBE, MONITOR, WAIT). On a cluster a command with a key is routed to its node; set every_master to run it on each master instead. Strings over {MAX_STRING_CHARS} characters and arrays or maps over {MAX_ITEMS} items are cut, with what was dropped counted in place."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "db": db,
                    "command": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "The command and its arguments, one word each." },
                    "every_master": { "type": "boolean", "default": false, "description": "Run the command on every master and answer one reply per node, instead of routing it by key." }
                },
                "required": ["server", "command"],
                "additionalProperties": false
            },
            "annotations": read_only,
        }),
    ]
}

/// `tools/call`: resolve the server, judge the rate, run the tool, write
/// the line. A tool that could not do what it was asked answers with
/// `isError` and a reason the model can read, which is how it learns that a
/// write is refused here rather than retrying it; a call that is not a
/// tool call at all is a protocol error.
async fn call(state: &AppState, account: &str, origin: &Origin, params: Json) -> Result<Json, RpcError> {
    let name = params
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| RpcError::params("tools/call names a tool"))?
        .to_string();
    if !TOOLS.contains(&name.as_str()) {
        return Err(RpcError::params(format!("unknown tool {name:?}")));
    }
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(RpcError::params("arguments is an object"));
    }

    let server = match arguments.get("server").and_then(Json::as_str) {
        Some(spec) => match resolve(state, account, spec) {
            Ok(server) => {
                note_server(state, origin, &server.id);
                Some(server)
            }
            Err(reason) => return Ok(finish(state, account, origin, &name, None, &arguments, Err(reason))),
        },
        None if name == "list_servers" => None,
        None => return Err(RpcError::params(format!("{name} takes a server"))),
    };
    let outcome = if state.mcp_calls.allow(account) {
        run_tool(state, account, &name, server.as_ref(), arguments.clone()).await
    } else {
        Err(format!(
            "rate limit: this account may make {CALLS_PER_MINUTE} calls a minute; wait before calling again"
        ))
    };
    Ok(finish(
        state,
        account,
        origin,
        &name,
        server.as_ref(),
        &arguments,
        outcome,
    ))
}

/// The audit line and the reply, whichever way the call went.
fn finish(
    state: &AppState,
    account: &str,
    origin: &Origin,
    tool: &str,
    server: Option<&RedisServer>,
    arguments: &Json,
    outcome: Result<Json, String>,
) -> Json {
    state.audit.record(
        account,
        origin,
        Event::Tool {
            tool: tool.to_string(),
            server: server.map(ServerRef::from),
            db: arguments.get("db").and_then(Json::as_u64).map(|db| db as usize),
            arguments: audited_arguments(tool, arguments),
            error: outcome.as_ref().err().cloned(),
        },
    );
    let (text, is_error) = match outcome {
        Ok(result) => (
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("the result could not be written: {e}")),
            false,
        ),
        Err(reason) => (reason, true),
    };
    json!({
        "content": [{ "type": "text", "text": cap_text(text) }],
        "isError": is_error,
    })
}

/// The arguments as the line keeps them: as given, except a
/// `read_command`'s command, which is redacted and cut like any command
/// line's — `AUTH`, `MIGRATE … AUTH` and the like are refused, but the
/// refusal is logged too, and the log must not be where a password lands.
fn audited_arguments(tool: &str, arguments: &Json) -> Json {
    let mut arguments = arguments.clone();
    if tool == "read_command"
        && let Some(command) = arguments.get_mut("command")
        && let Some(words) = command.as_array()
    {
        let words: Vec<String> = words
            .iter()
            .map(|w| w.as_str().map_or_else(|| w.to_string(), str::to_string))
            .collect();
        if let Some((name, rest)) = words.split_first() {
            let mut kept = vec![name.clone()];
            kept.extend(audit::cut(redact_secrets(name, rest)));
            *command = json!(kept);
        }
    }
    arguments
}

/// The entry `spec` names among those the account may see: by id, else by
/// name. A name two entries share is refused with both ids, because
/// guessing which prod the model meant is not a thing to guess.
fn resolve(state: &AppState, account: &str, spec: &str) -> Result<RedisServer, String> {
    let visible: Vec<RedisServer> = get_servers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|server| visible_to(&state.accounts, server, account))
        .collect();
    if let Some(server) = visible.iter().find(|s| s.id == spec) {
        return Ok(server.clone());
    }
    let named: Vec<&RedisServer> = visible.iter().filter(|s| s.name == spec).collect();
    match named.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "no server named {spec:?} is visible to this account; list_servers shows the names and ids"
        )),
        several => Err(format!(
            "{} servers are named {spec:?}; name one by id: {}",
            several.len(),
            several.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

async fn run_tool(
    state: &AppState,
    account: &str,
    name: &str,
    server: Option<&RedisServer>,
    arguments: Json,
) -> Result<Json, String> {
    let Some(server) = server else {
        return list_servers(state, account);
    };
    match name {
        "scan_keys" => scan_keys(state, server, parse(arguments)?).await,
        "inspect_key" => inspect_key(state, server, parse(arguments)?).await,
        "server_info" => server_info(state, server, parse(arguments)?).await,
        "slowlog" => slowlog(state, server, parse(arguments)?).await,
        "slowlog_summary" => slowlog_summary(state, server, parse(arguments)?).await,
        "hotkeys" => hotkeys(state, server, parse(arguments)?).await,
        "memory_summary" => memory_summary(state, server, parse(arguments)?).await,
        "troubleshooting_report" => troubleshooting_report(server, parse(arguments)?).await,
        "read_command" => read_command(state, server, parse(arguments)?).await,
        other => Err(format!("unknown tool {other:?}")),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: Json) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|e| format!("arguments: {e}"))
}

/// Judge and send `commands` to `server`, the way the page's own requests
/// go. The read-only role is fixed here rather than read from the account:
/// the door already admitted only a read-only account, and this makes the
/// tools read-only by construction rather than by that check alone.
/// `nodes` names the master each command is for (a fan-out with one command
/// per master); empty with `every_master` runs the first command on each
/// master; empty without runs it on one routed connection.
async fn run(
    state: &AppState,
    server: &RedisServer,
    db: usize,
    commands: Vec<Vec<Vec<u8>>>,
    nodes: Vec<String>,
    every_master: bool,
) -> Result<(Vec<Value>, Vec<String>), String> {
    for args in &commands {
        if policy::holds_connection(args) {
            return Err(format!(
                "{} changes or holds the connection the tools share; the tools select the database themselves",
                policy::command_name(args)
            ));
        }
        match policy::check(server, args, None, true, false, false) {
            policy::Verdict::Allow | policy::Verdict::Confirmed { .. } => {}
            policy::Verdict::Deny => {
                return Err(format!(
                    "{} is not a read command; this entry point only reads",
                    policy::command_name(args)
                ));
            }
            policy::Verdict::Confirm { .. } => {
                return Err(format!(
                    "{} needs a confirmation this entry point cannot give",
                    policy::command_name(args)
                ));
            }
        }
    }
    let req = ExecRequest::decoded(&server.id, db, nodes, every_master);
    forward_values(state, "", &req, &commands)
        .await
        .map_err(|e| e.describe())
}

/// One command on one routed connection, its reply.
async fn one(state: &AppState, server: &RedisServer, db: usize, args: Vec<Vec<u8>>) -> Result<Value, String> {
    let (mut values, _) = run(state, server, db, vec![args], Vec::new(), false).await?;
    values.pop().ok_or_else(|| "no reply".to_string())
}

fn words(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|part| part.as_bytes().to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::tools::{
        encode_cursor, format_troubleshooting_snapshot, pair_measurements, pairs, parse_cursor, parse_info, render,
        scan_reply, slowlog_entry,
    };
    use super::*;
    use zedis_connection::{SlowLogEntry, TroubleshootingSnapshot};

    /// Replies come back master by master, not in the order the keys were
    /// sent; each key still gets its own type and size, and a key gone
    /// since the scan or refused by MEMORY is left out.
    #[test]
    fn measurements_are_paired_back_per_master() {
        let batch: Vec<(String, String)> = [
            ("a:1", "user:1"),
            ("b:2", "cache:x"),
            ("a:1", "user:2"),
            ("b:2", "gone"),
        ]
        .iter()
        .map(|(label, key)| (label.to_string(), key.to_string()))
        .collect();
        // Master a:1 answers first, then b:2 — each in the order it was sent.
        let answered: Vec<String> = ["a:1", "a:1", "a:1", "a:1", "b:2", "b:2", "b:2", "b:2"]
            .iter()
            .map(|label| label.to_string())
            .collect();
        let values = vec![
            Value::SimpleString("hash".into()),
            Value::Int(300),
            Value::SimpleString("string".into()),
            Value::Int(80),
            Value::SimpleString("string".into()),
            Value::Int(900),
            Value::SimpleString("none".into()),
            Value::Nil,
        ];
        assert_eq!(
            pair_measurements(&batch, answered, values),
            [
                ("user:1".to_string(), "hash".to_string(), 300),
                ("user:2".to_string(), "string".to_string(), 80),
                ("cache:x".to_string(), "string".to_string(), 900),
            ]
        );
    }

    #[test]
    fn a_request_is_taken_apart_and_a_notification_has_no_id() {
        let rpc = Rpc::parse(json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" })).expect("request");
        assert_eq!(rpc.id, Some(json!(7)));
        assert_eq!(rpc.method, "ping");
        assert_eq!(rpc.params, json!({}));

        let rpc = Rpc::parse(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).expect("notification");
        assert!(rpc.id.is_none());
        // `"id": null` is no id either.
        let rpc = Rpc::parse(json!({ "jsonrpc": "2.0", "id": null, "method": "x" })).expect("null id");
        assert!(rpc.id.is_none());

        for bad in [json!([]), json!("ping"), json!({ "id": 1 }), json!({ "method": 3 })] {
            let (_, error) = Rpc::parse(bad.clone()).err().expect("refused");
            assert_eq!(error.code, INVALID_REQUEST, "{bad}");
        }
        // Refused under its own id where it named one.
        let (id, _) = Rpc::parse(json!({ "id": 1 })).err().expect("refused");
        assert_eq!(id, json!(1));
        let (id, _) = Rpc::parse(json!({ "id": "a", "method": 3 })).err().expect("refused");
        assert_eq!(id, json!("a"));
        let (id, _) = Rpc::parse(json!({ "id": [1], "method": 3 })).err().expect("refused");
        assert_eq!(id, Json::Null, "an id that is no id");
        let (_, batch) = Rpc::parse(json!([{ "method": "ping" }])).err().expect("refused");
        assert!(batch.message.contains("batches"), "{}", batch.message);
    }

    #[test]
    fn initialize_answers_the_asked_version_when_known_and_the_latest_otherwise() {
        let known = initialize(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(known["protocolVersion"], "2024-11-05");
        let unknown = initialize(&json!({ "protocolVersion": "2031-01-01" }));
        assert_eq!(unknown["protocolVersion"], LATEST_PROTOCOL);
        let none = initialize(&json!({}));
        assert_eq!(none["protocolVersion"], LATEST_PROTOCOL);
        assert!(none["capabilities"]["tools"].is_object());
        assert_eq!(none["serverInfo"]["name"], "zedis-bridge");
    }

    #[test]
    fn the_tool_list_names_every_tool_once_with_a_schema_and_the_read_only_mark() {
        let tools = tool_list();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().expect("name")).collect();
        assert_eq!(names, TOOLS);
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{}", tool["name"]);
            assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
        }
        // Every tool but the listing takes a server.
        for tool in tools.iter().filter(|t| t["name"] != "list_servers") {
            assert!(
                tool["inputSchema"]["properties"]["server"].is_object(),
                "{}",
                tool["name"]
            );
        }
    }

    #[test]
    fn a_reply_is_rendered_as_json_and_cut_where_it_is_long() {
        let long = "x".repeat(MAX_STRING_CHARS + 10);
        let value = Value::Array(vec![
            Value::Nil,
            Value::Int(3),
            Value::Okay,
            Value::BulkString(long.into_bytes()),
            Value::Map(vec![(Value::BulkString(b"k".to_vec()), Value::Double(1.5))]),
        ]);
        let json = render(&value);
        assert_eq!(json[0], Json::Null);
        assert_eq!(json[1], 3);
        assert_eq!(json[2], "OK");
        let cut = json[3].as_str().expect("string");
        assert!(cut.ends_with("…(+10 chars)"), "{}", &cut[cut.len() - 20..]);
        assert_eq!(json[4]["k"], 1.5);

        let many = Value::Array((0..MAX_ITEMS + 5).map(|i| Value::Int(i as i64)).collect());
        let json = render(&many);
        let items = json.as_array().expect("array");
        assert_eq!(items.len(), MAX_ITEMS + 1);
        assert_eq!(items[MAX_ITEMS], "…(+5 more)");

        let mut text = "y".repeat(MAX_TEXT_BYTES - 1);
        text.push('中');
        text.push_str("zz");
        let capped = cap_text(text);
        assert!(capped.contains("was cut at 200 KiB"));
        assert!(capped.starts_with(&"y".repeat(MAX_TEXT_BYTES - 1)));
    }

    #[test]
    fn a_scan_cursor_round_trips_and_drops_a_master_that_is_gone() {
        let labels = vec!["a:1".to_string(), "b:2".to_string()];
        let cursors = vec![("a:1".to_string(), 17), ("b:2".to_string(), 0)];
        let encoded = encode_cursor(&cursors);
        assert_eq!(encoded, "a:1=17;b:2=0");
        assert_eq!(parse_cursor(&encoded, &labels).expect("cursor"), cursors);
        // A node the topology no longer has is not resumed anywhere else.
        assert_eq!(
            parse_cursor("a:1=5;gone:9=8", &labels).expect("cursor"),
            vec![("a:1".to_string(), 5)]
        );
        assert!(parse_cursor("what", &labels).is_err());
        assert!(parse_cursor("a:1=x", &labels).is_err());

        let reply = Value::Array(vec![
            Value::BulkString(b"42".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"k1".to_vec()),
                Value::BulkString(b"k2".to_vec()),
            ]),
        ]);
        assert_eq!(
            scan_reply(&reply).expect("scan"),
            (42, vec!["k1".to_string(), "k2".to_string()])
        );
        assert!(scan_reply(&Value::Nil).is_err());
    }

    #[test]
    fn the_limiter_allows_the_calls_of_a_minute_and_refuses_the_next() {
        let limiter = Limiter::new();
        let start = Instant::now();
        for _ in 0..CALLS_PER_MINUTE {
            assert!(limiter.allow_at("ai", start));
        }
        assert!(!limiter.allow_at("ai", start + Duration::from_secs(30)));
        assert!(limiter.allow_at("other", start), "per account");
        // A minute on, the window has moved past the first calls.
        assert!(limiter.allow_at("ai", start + WINDOW));
    }

    #[test]
    fn info_text_is_parsed_into_sections_with_numbers_as_numbers() {
        let info = "# Server\r\nredis_version:7.2.4\r\nvalkey_version:9.0.6\r\n\r\n# Memory\r\nused_memory:1024\r\nmem_fragmentation_ratio:1.25\r\n";
        let parsed = parse_info(info);
        assert_eq!(parsed["server"]["valkey_version"], "9.0.6");
        assert_eq!(parsed["memory"]["used_memory"], 1024);
        assert_eq!(parsed["memory"]["mem_fragmentation_ratio"], 1.25);
    }

    #[test]
    fn a_slowlog_entry_is_read_by_position() {
        let entry = Value::Array(vec![
            Value::Int(9),
            Value::Int(1_790_000_000),
            Value::Int(15_000),
            Value::Array(vec![
                Value::BulkString(b"KEYS".to_vec()),
                Value::BulkString(b"*".to_vec()),
            ]),
            Value::BulkString(b"10.0.0.7:5000".to_vec()),
            Value::BulkString(b"worker".to_vec()),
        ]);
        let json = slowlog_entry(&entry);
        assert_eq!(json["id"], 9);
        assert_eq!(json["duration_us"], 15_000);
        assert_eq!(json["command"], json!(["KEYS", "*"]));
        assert_eq!(json["client_name"], "worker");
    }

    #[test]
    fn a_read_command_s_arguments_are_redacted_in_the_audit_line() {
        let line = audited_arguments(
            "read_command",
            &json!({ "server": "prod", "command": ["AUTH", "hunter2"] }),
        );
        let text = line.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert_eq!(line["command"][0], "AUTH");
        // Other tools' arguments are kept as given.
        let same = json!({ "server": "prod", "pattern": "user:*" });
        assert_eq!(audited_arguments("scan_keys", &same), same);
    }

    #[test]
    fn a_command_that_changes_the_shared_connection_is_named_whatever_the_allowlist_says() {
        // Each of these is a read to `is_read_only_command`, and each would
        // change what the next caller of the pooled connection sees.
        for cmd in [
            vec!["select", "3"],
            vec!["AUTH", "x"],
            vec!["HELLO", "3"],
            vec!["CLIENT", "setname", "ai"],
            vec!["CLIENT", "TRACKING", "ON"],
            vec!["SUBSCRIBE", "ch"],
            vec!["MONITOR"],
            vec!["WAIT", "1", "0"],
        ] {
            assert!(policy::holds_connection(&words(&cmd)), "{cmd:?}");
        }
        for cmd in [
            vec!["GET", "k"],
            vec!["SCAN", "0"],
            vec!["INFO"],
            vec!["CLIENT", "LIST"],
            vec!["CLIENT", "INFO"],
            vec!["SLOWLOG", "GET"],
            vec!["JSON.GET", "k"],
        ] {
            assert!(!policy::holds_connection(&words(&cmd)), "{cmd:?}");
        }
    }

    #[test]
    fn a_hash_preview_is_pairs() {
        let flat = json!(["name", "ann", "age", "7"]);
        assert_eq!(pairs(flat), json!({ "name": "ann", "age": "7" }));
    }

    #[test]
    fn troubleshooting_snapshot_redacts_secrets_and_preserves_partial_parts() {
        let server = RedisServer {
            id: "prod-1".to_string(),
            name: "prod".to_string(),
            host: "10.0.0.1".to_string(),
            port: 6379,
            ..Default::default()
        };
        let snapshot = TroubleshootingSnapshot {
            summary: Ok(zedis_connection::ServerSummary {
                version: "7.2.4".to_string(),
                description: zedis_connection::RedisClientDescription {
                    server_type: "standalone".to_string(),
                    flavor: zedis_core::features::ServerFlavor::Valkey,
                    flavor_version: "9.0.0".to_string(),
                    ..Default::default()
                },
                nodes: (1, 2),
                dbsize: 42,
                databases: 16,
                access_mode: zedis_connection::AccessMode::ReadWrite,
                supports_rejson: false,
                supports_search: false,
                supports_timeseries: false,
                supports_bf: false,
            }),
            info: Ok(vec![(
                "10.0.0.1:6379".to_string(),
                "# Memory\r\nused_memory:2048\r\n".to_string(),
            )]),
            memory_doctor: Err("ERR unknown command 'MEMORY'".to_string()),
            latency_doctor: Ok(vec![zedis_connection::NodeReply {
                node: "10.0.0.1:6379".to_string(),
                value: "No latency spikes".to_string(),
            }]),
            latency: Ok(Default::default()),
            slow_log: Ok(vec![
                SlowLogEntry {
                    id: 1,
                    timestamp: 1_790_000_010,
                    duration: Duration::from_micros(4500),
                    amount: 4500,
                    args: vec![
                        "CONFIG".to_string(),
                        "SET".to_string(),
                        "requirepass".to_string(),
                        "super-secret-password".to_string(),
                    ],
                    client_addr: Some("10.0.0.8:5000".to_string()),
                    client_name: Some("admin".to_string()),
                },
                SlowLogEntry {
                    id: 2,
                    timestamp: 1_790_000_020,
                    duration: Duration::from_micros(9000),
                    amount: 9000,
                    args: vec![
                        "HSET".to_string(),
                        "user:42".to_string(),
                        "token".to_string(),
                        "private-value".to_string(),
                    ],
                    client_addr: None,
                    client_name: None,
                },
            ]),
            config: Ok(vec![
                ("maxmemory".to_string(), Some("0".to_string())),
                ("maxmemory-policy".to_string(), Some("noeviction".to_string())),
            ]),
        };

        let out = format_troubleshooting_snapshot(&server, 0, &snapshot);
        let serialized = out.to_string();
        assert!(!serialized.contains("super-secret-password"), "{serialized}");
        assert!(!serialized.contains("private-value"), "{serialized}");
        assert_eq!(out["summary"]["flavor"], "Valkey");
        assert_eq!(out["summary"]["dbsize"], 42);
        assert_eq!(out["nodes"][0]["info"]["memory"]["used_memory"], 2048);
        assert_eq!(out["memory_doctor"]["error"], "ERR unknown command 'MEMORY'");
        assert_eq!(out["latency_doctor"][0]["report"], "No latency spikes");
        assert_eq!(out["slow_log"]["newest"][0]["command"], "HSET user:42 … (+2)");
        assert_eq!(out["slow_log"]["newest"][1]["command"], "CONFIG SET requirepass … (+1)");
        assert_eq!(out["config"]["maxmemory-policy"], "noeviction");
        assert!(WRITE_ACCOUNT_REFUSAL.contains("Account page"));
    }
}
