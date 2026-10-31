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

//! Talking to Redis through the HTTP bridge instead of a socket.
//!
//! A browser cannot open a TCP connection, so the web build sends packed
//! commands to `zedis-bridge` and gets packed replies back (ADR 9). What
//! makes that cheap is where it plugs in: [`BridgeConn`] implements
//! `redis::aio::ConnectionLike`, so it becomes one more variant of
//! [`RedisAsyncConn`](crate::RedisAsyncConn) and every existing call site
//! keeps working untouched.
//!
//! The HTTP itself is *not* here. This crate has no HTTP client and no gpui,
//! and it must keep neither: the transport is the [`BridgeTransport`] trait,
//! implemented by the caller. The desktop app hands over one built on GPUI's
//! `HttpClient`, which is the native client on the desktop and the browser's
//! `fetch` under wasm, so one implementation serves both.

#[cfg(target_family = "wasm")]
use crate::conn::RedisAsyncConn;
use futures::future::BoxFuture;
// The blocking `ConnectionLike` — the one `Pipeline::query` takes — named
// apart from `aio`'s, which the host build imports below.
use redis::ConnectionLike as BlockingConnectionLike;
use redis::{Cmd, ErrorKind, FromRedisValue, Pipeline, RedisError, RedisResult, Value};
// `aio` is what carries `ConnectionLike` and `Cmd::query_async`, and it cannot
// be built for `wasm32-unknown-unknown` — it insists on a socket runtime. The
// browser gets the same call sites from the traits at the bottom of this file
// instead (ADR 9).
#[cfg(not(target_family = "wasm"))]
use redis::{RedisFuture, aio::ConnectionLike};
use std::sync::{Arc, OnceLock};

use crate::command::{command_group, is_known_command};
use crate::diagnosis::DiagReportRow;
use crate::script_kill::{KillReply, KillTarget};

/// Why a bridge call failed, in terms the UI can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeErrorKind {
    /// The bridge could not be reached, or answered nothing usable.
    Transport,
    /// The bearer token was missing or wrong.
    Unauthorized,
    /// The server id is not in the bridge's list.
    UnknownServer,
    /// The command is destructive and the bridge wants it confirmed. The
    /// payload is what the desktop confirm dialog needs to ask the same
    /// question: a `danger.*` i18n key, and whether clicking is enough.
    ConfirmationRequired {
        danger_key: String,
        type_name_required: bool,
    },
    /// The bridge reached Redis and Redis (or the dial) failed.
    Upstream,
    /// The request is larger than the bridge accepts
    /// ([`BRIDGE_MAX_REQUEST_BYTES`]): refused before it was sent, or
    /// answered `413`. Nothing is wrong with the link, which is why it is a
    /// kind of its own — as a transport failure it read as a dropped
    /// connection, and the page reconnected over a value too large to save.
    TooLarge,
}

/// The most one request to the bridge may carry. The bridge holds every
/// route to it, and the page refuses to send more than it rather than
/// waiting for the `413`.
pub const BRIDGE_MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// The largest value a page writes in one command: the request carries the
/// command base64 — four bytes for every three — inside JSON, and the key
/// and the rest of the request need room beside it.
pub const BRIDGE_MAX_VALUE_BYTES: usize = BRIDGE_MAX_REQUEST_BYTES / 4 * 3 - 64 * 1024;

#[derive(Debug, Clone)]
pub struct BridgeError {
    pub kind: BridgeErrorKind,
    pub message: String,
}

impl BridgeError {
    pub fn new(kind: BridgeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorKind::Transport, message)
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BridgeError {}

impl From<BridgeError> for RedisError {
    fn from(err: BridgeError) -> Self {
        // `ConnectionLike` can only fail with a `RedisError`, so the shape is
        // flattened here. The kind is preserved as faithfully as redis-rs's
        // vocabulary allows so that the app's existing link-error handling
        // (`note_link_error`) still recognises a dead bridge as a dead link.
        let kind = match err.kind {
            BridgeErrorKind::Transport => ErrorKind::Io,
            BridgeErrorKind::Unauthorized
            | BridgeErrorKind::ConfirmationRequired { .. }
            | BridgeErrorKind::TooLarge => ErrorKind::Client,
            BridgeErrorKind::UnknownServer => ErrorKind::InvalidClientConfig,
            BridgeErrorKind::Upstream => ErrorKind::Extension,
        };
        RedisError::from((kind, "redis bridge", err.message))
    }
}

/// How a pipeline is framed, mirroring what redis-rs asks of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineSpec {
    /// Replies to discard before the interesting ones begin.
    pub offset: usize,
    /// Replies the caller wants.
    pub count: usize,
    /// Whether redis-rs wraps this in `MULTI`/`EXEC`.
    pub atomic: bool,
}

impl PipelineSpec {
    /// The framing redis-rs asks a connection for when it runs a pipeline
    /// of `len` commands: every reply, or — for a transaction, whose `MULTI`
    /// and `QUEUED`s arrive first — the single `EXEC` array.
    ///
    /// Both sides of the bridge use this. The page frames a pipeline exactly
    /// as the desktop does, and the bridge refuses any other framing: on a
    /// multiplexed connection the count is how replies are handed out, so a
    /// wrong one gives another caller's replies to this one.
    pub fn for_pipeline(len: usize, atomic: bool) -> Self {
        if atomic {
            Self {
                offset: len + 1,
                count: 1,
                atomic,
            }
        } else {
            Self {
                offset: 0,
                count: len,
                atomic,
            }
        }
    }
}

/// Replies a pipeline already received, played back to redis-rs's own
/// blocking `Pipeline::query`, so the request is finished by the same code
/// as on the host: a transaction's `EXEC` array unwrapped, what `.ignore()`
/// marked dropped, an error reply surfaced. That code is private to redis-rs;
/// this is the one way to reach it without a socket.
struct Replayed(Option<Vec<Value>>);

impl BlockingConnectionLike for Replayed {
    fn req_packed_command(&mut self, _cmd: &[u8]) -> RedisResult<Value> {
        Err(RedisError::from((
            ErrorKind::Client,
            "redis bridge",
            "a replayed pipeline answers pipelines only".to_string(),
        )))
    }

    fn req_packed_commands(&mut self, _cmd: &[u8], _offset: usize, _count: usize) -> RedisResult<Vec<Value>> {
        self.0.take().ok_or_else(|| {
            RedisError::from((
                ErrorKind::Client,
                "redis bridge",
                "a replayed pipeline answers once".to_string(),
            ))
        })
    }

    fn get_db(&self) -> i64 {
        0
    }

    fn check_connection(&mut self) -> bool {
        true
    }

    fn is_open(&self) -> bool {
        true
    }
}

/// One call to the bridge.
#[derive(Debug, Clone)]
pub struct BridgeRequest {
    pub server_id: String,
    pub db: usize,
    /// Pin to a connection of this caller's own, for `SELECT` / `MULTI` and
    /// anything else that is connection state rather than command semantics.
    pub session: Option<String>,
    /// The packed commands: one for a plain command, several for a pipeline.
    pub commands: Vec<Vec<u8>>,
    /// `None` for a plain command.
    pub pipeline: Option<PipelineSpec>,
    /// Run the commands on every master of the server instead of on one
    /// connection. The bridge does the fan-out, because reaching each master
    /// is the same kind of knowledge as reaching the server at all — and the
    /// browser must not be handed dialable node addresses (ADR 9).
    pub fanout_masters: bool,
    /// Which node each command is for, as a `host:port` label.
    ///
    /// Empty means "every master, padding with the first command". When set,
    /// `commands[i]` is for `fanout_nodes[i]` and nodes not listed are not
    /// asked at all.
    ///
    /// Labels, not positions, because the caller and the bridge discover the
    /// topology separately: a failover between the two discoveries would
    /// leave the lists the same length but in a different order, and the
    /// per-node SCAN cursors would then be applied to the wrong nodes and
    /// silently return the wrong keys.
    pub fanout_nodes: Vec<String>,
    /// Replayed after a [`BridgeErrorKind::ConfirmationRequired`] refusal.
    pub confirm: Option<String>,
}

/// How many commands of one request get a [`describe_packed_command`] label.
/// A key-tree round packs two thousand; the labels are for a person reading
/// the request, and the first few tell them what it is.
pub const DESCRIBED_COMMANDS: usize = 32;

/// The `commands.json` groups whose commands take a key first.
const KEY_FIRST_GROUPS: [&str; 11] = [
    "string",
    "hash",
    "list",
    "set",
    "sorted_set",
    "stream",
    "geo",
    "bitmap",
    "hyperloglog",
    "array",
    "generic",
];

/// Module families the table does not carry, whose first argument is a key
/// (or an index name, `FT.*`) — never a credential.
const KEY_FIRST_PREFIXES: [&str; 8] = ["JSON.", "TS.", "BF.", "CF.", "CMS.", "TOPK.", "TDIGEST.", "FT."];

/// Commands outside those groups that take a key first: the vector sets, which
/// the table does not carry either, and `MEMORY USAGE`, which it files under
/// `server`.
const KEY_FIRST_COMMANDS: [&str; 14] = [
    "MEMORY USAGE",
    "VADD",
    "VCARD",
    "VDIM",
    "VEMB",
    "VGETATTR",
    "VINFO",
    "VISMEMBER",
    "VLINKS",
    "VRANDMEMBER",
    "VRANGE",
    "VREM",
    "VSETATTR",
    "VSIM",
];

/// Characters of a key a label keeps.
const LABEL_KEY_CHARS: usize = 64;

/// Readable labels for a request's packed commands — one per command up to
/// [`DESCRIBED_COMMANDS`], then one `… +N more` — sent beside them as `desc`
/// so the browser's network panel says what a request does. The bridge never
/// reads them: its policy judges the commands themselves.
pub fn describe_commands(commands: &[Vec<u8>]) -> Vec<String> {
    let mut labels: Vec<String> = commands
        .iter()
        .take(DESCRIBED_COMMANDS)
        .map(|packed| describe_packed_command(packed))
        .collect();
    if commands.len() > DESCRIBED_COMMANDS {
        labels.push(format!("… +{} more", commands.len() - DESCRIBED_COMMANDS));
    }
    labels
}

/// A packed command as a person reads it: its name — with the subcommand,
/// `ACL LIST` — the key when it has one, and how many arguments follow:
/// `HSET user:1 (+4 args)`.
///
/// The key is shown only for a command that takes one first — the keyspace
/// groups of `commands.json`, the module families, the vector sets — so an
/// argument that may be a credential (`AUTH`, `CONFIG SET requirepass`,
/// `ACL SETUSER`, `HELLO … AUTH`) or a whole script (`EVAL`) is counted and
/// never shown. So is anything the table does not know, which in the browser
/// includes every command sent before `commands.json` has arrived.
pub fn describe_packed_command(packed: &[u8]) -> String {
    let Some(args) = unpack_command(packed) else {
        return "(unreadable command)".to_string();
    };
    let Some((first, mut rest)) = args.split_first() else {
        return "(empty command)".to_string();
    };
    let mut name = String::from_utf8_lossy(first).to_ascii_uppercase();
    if let Some((sub, after)) = rest.split_first() {
        let full = format!("{name} {}", String::from_utf8_lossy(sub).to_ascii_uppercase());
        if is_known_command(&full) {
            name = full;
            rest = after;
        }
    }
    let takes_key = KEY_FIRST_COMMANDS.contains(&name.as_str())
        || match command_group(&name) {
            Some(group) => KEY_FIRST_GROUPS.contains(&group),
            None => KEY_FIRST_PREFIXES.iter().any(|prefix| name.starts_with(prefix)),
        };
    let key = rest.first().filter(|_| takes_key);
    let mut label = name;
    if let Some(key) = key {
        label.push(' ');
        label.push_str(&label_key(key));
    }
    match rest.len() - usize::from(key.is_some()) {
        0 => {}
        1 => label.push_str(" (+1 arg)"),
        n => label.push_str(&format!(" (+{n} args)")),
    }
    label
}

/// A key as a label shows it: as typed when it is plain text, quoted and
/// escaped when a space, a quote or bytes that are not UTF-8 would make it
/// ambiguous, and cut at [`LABEL_KEY_CHARS`].
fn label_key(key: &[u8]) -> String {
    let text = String::from_utf8_lossy(key);
    let mut shown: String = text.chars().take(LABEL_KEY_CHARS).collect();
    let cut = text.chars().count() > LABEL_KEY_CHARS;
    let plain = std::str::from_utf8(key).is_ok()
        && !shown.is_empty()
        && !shown
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\');
    if !plain {
        shown = format!("{shown:?}");
    }
    if cut {
        shown.push('…');
    }
    shown
}

/// The arguments of a command as `Cmd::get_packed_command` packs it: a RESP
/// array of bulk strings. `None` for anything else.
fn unpack_command(packed: &[u8]) -> Option<Vec<&[u8]>> {
    fn line(bytes: &[u8], at: usize) -> Option<(&[u8], usize)> {
        let end = at + bytes.get(at..)?.windows(2).position(|w| w == b"\r\n")?;
        Some((&bytes[at..end], end + 2))
    }
    let (header, mut at) = line(packed, 0)?;
    let count: usize = std::str::from_utf8(header.strip_prefix(b"*")?).ok()?.parse().ok()?;
    let mut args = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let (len, next) = line(packed, at)?;
        let len: usize = std::str::from_utf8(len.strip_prefix(b"$")?).ok()?.parse().ok()?;
        let arg = packed.get(next..next.checked_add(len)?)?;
        args.push(arg);
        at = next + len + 2;
    }
    Some(args)
}

/// What a bridge call produced.
#[derive(Debug, Clone, Default)]
pub struct BridgeReply {
    /// One RESP frame per reply, for `redis::parse_redis_value`.
    pub frames: Vec<Vec<u8>>,
    /// For a fan-out, which node answered each frame, as `host:port`.
    ///
    /// A display label and nothing more. The desktop shows the same string,
    /// so it discloses nothing new — unlike a connectable address paired with
    /// credentials, which stays in the bridge.
    pub nodes: Vec<String>,
}

/// What a page asks the bridge to hold for it: a subscription, or
/// `MONITOR` — what the server pushes, which a request and its reply cannot
/// carry, so the bridge keeps it and the page polls (`/v1/watch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Patterns,
    Sharded,
    KeyspaceEvents,
    Monitor,
}

impl WatchKind {
    /// As the bridge's route names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Patterns => "patterns",
            Self::Sharded => "sharded",
            Self::KeyspaceEvents => "keyspace_events",
            Self::Monitor => "monitor",
        }
    }
}

/// A watch the bridge opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOpened {
    pub token: String,
    /// The nodes a `MONITOR` reads, as `host:port`.
    pub nodes: Vec<String>,
    /// The nodes it could not start on, and why.
    pub failures: Vec<(String, String)>,
}

/// One thing a watch heard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchItem {
    Message { channel: String, payload: Vec<u8> },
    Line { node: String, line: String },
}

/// What one poll of a watch answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchBatch {
    pub items: Vec<WatchItem>,
    /// Dropped on the bridge since the last poll: the page fell behind.
    pub dropped: u64,
    /// Why it has ended, once it has.
    pub closed: Option<String>,
}

/// The HTTP (or any other) carrier for [`BridgeRequest`]s.
///
/// Implemented outside this crate so that neither an HTTP client nor gpui
/// becomes a dependency here.
pub trait BridgeTransport: Send + Sync + 'static {
    fn send(&self, request: BridgeRequest) -> BoxFuture<'static, Result<BridgeReply, BridgeError>>;

    /// Ask for a backend connection of this caller's own, and get its token.
    ///
    /// Required rather than optional, because the thing it protects is not a
    /// nicety: `SELECT`, `AUTH`, `CLIENT SETNAME` and `MULTI` are *connection*
    /// state (ADR 4), so a terminal running them on a pooled connection moves
    /// the key tree to another database or scatters a transaction. A transport
    /// that cannot pin a connection has to say so here, not hand back a shared
    /// one that looks dedicated.
    fn open_session(&self, server_id: String, db: usize) -> BoxFuture<'static, Result<String, BridgeError>>;

    /// Release it. The bridge also sweeps idle sessions, which is what covers
    /// a tab that closes mid-transaction and never sends this.
    fn close_session(&self, session: String) -> BoxFuture<'static, Result<(), BridgeError>>;

    /// Open the bridge's write window on `server_id` for this account, with
    /// `confirm` — the entry's name, which is what production asks for and
    /// what the page's dialog has just had answered (ADR 14).
    fn unlock_writes(&self, server_id: String, confirm: String) -> BoxFuture<'static, Result<(), BridgeError>>;

    /// Close it before it would have closed itself.
    fn lock_writes(&self, server_id: String) -> BoxFuture<'static, Result<(), BridgeError>>;

    /// End this caller's login with the bridge and go back to where one is
    /// asked for. The bridge forgets the login, not just the page: a cookie
    /// dropped on one side alone would still be good until it lapsed.
    fn sign_out(&self) -> BoxFuture<'static, Result<(), BridgeError>>;

    /// Have the bridge subscribe, or start `MONITOR`, for this caller.
    /// `confirm` answers the question the bridge asks before `MONITOR` on
    /// an entry whose writes are locked (`ServerDb::confirmed`).
    fn open_watch(
        &self,
        server_id: String,
        db: usize,
        kind: WatchKind,
        channels: Vec<String>,
        confirm: Option<String>,
    ) -> BoxFuture<'static, Result<WatchOpened, BridgeError>>;

    /// What has arrived on `token`, waiting a while for something to.
    fn poll_watch(&self, token: String) -> BoxFuture<'static, Result<WatchBatch, BridgeError>>;

    /// End it, without waiting to hear back: this is what dropping a
    /// subscription calls, and a `Drop` cannot await. The bridge also ends
    /// a watch nobody polls, which covers a tab that closed first.
    fn release_watch(&self, token: String);

    /// Diagnose the saved entry `server_id` from the bridge — the process
    /// that dials it, and so the only one that can say which stage fails.
    /// Every stage, answered together.
    fn diagnose(&self, server_id: String) -> BoxFuture<'static, Result<Vec<DiagReportRow>, BridgeError>>;

    /// Send `SCRIPT KILL` / `FUNCTION KILL` to every data node of `server_id`
    /// from the bridge, over fresh connections that bypass a `BUSY` pool.
    fn kill_running(
        &self,
        server_id: String,
        target: KillTarget,
    ) -> BoxFuture<'static, Result<Vec<KillReply>, BridgeError>>;
}

/// The transport every bridge connection in this process uses.
///
/// A process-wide slot rather than a parameter threaded through
/// `ConnectionManager`, for the same reason `init_commands_json` is one: this
/// crate cannot construct the transport (it has no HTTP client and no gpui by
/// design), so whoever can has to hand one over, and there is exactly one per
/// process. The web entry point installs it before the first frame.
static TRANSPORT: OnceLock<Arc<dyn BridgeTransport>> = OnceLock::new();

/// Install the transport. The first call wins; a second is ignored and says
/// so, because two transports would mean two bridges and a connection would
/// silently belong to whichever installed first.
pub fn set_bridge_transport(transport: Arc<dyn BridgeTransport>) {
    if TRANSPORT.set(transport).is_err() {
        tracing::warn!("the bridge transport was already installed; keeping the first");
    }
}

/// The installed transport, or `None` before [`set_bridge_transport`].
pub fn bridge_transport() -> Option<Arc<dyn BridgeTransport>> {
    TRANSPORT.get().cloned()
}

/// Whether this process reaches Redis through a bridge it signed in to —
/// which is to say, whether it is the page. The desktop dials Redis itself
/// and has no login to end, so "Sign out" is drawn where this says so.
pub fn signed_in_to_bridge() -> bool {
    TRANSPORT.get().is_some()
}

/// End the bridge login ([`BridgeTransport::sign_out`]).
pub async fn sign_out_of_bridge() -> Result<(), BridgeError> {
    let transport = bridge_transport().ok_or_else(|| BridgeError::transport("no bridge transport"))?;
    transport.sign_out().await
}

/// Diagnose a saved entry from the bridge ([`BridgeTransport::diagnose`]).
pub async fn diagnose_on_bridge(server_id: String) -> Result<Vec<DiagReportRow>, BridgeError> {
    let transport = bridge_transport().ok_or_else(|| BridgeError::transport("no bridge transport"))?;
    transport.diagnose(server_id).await
}

/// Stop a runaway script on `server_id` from the bridge ([`BridgeTransport::kill_running`]).
pub async fn kill_on_bridge(server_id: String, target: KillTarget) -> Result<Vec<KillReply>, BridgeError> {
    let transport = bridge_transport().ok_or_else(|| BridgeError::transport("no bridge transport"))?;
    transport.kill_running(server_id, target).await
}

/// How the browser persists the server list.
///
/// `redis-servers.toml` belongs to the bridge, which is also the only process
/// allowed to hold the credentials in it (ADR 9) — so saving from a tab is an
/// HTTP request, and this crate cannot make one. The web entry point installs
/// a sink; without one the list is readable and not writable, which is an
/// error the UI can report rather than a save that quietly goes nowhere.
pub trait BridgeServerStore: Send + Sync + 'static {
    /// Persist `servers` and answer with the list as the bridge now holds it.
    ///
    /// The answer matters: a new entry leaves the browser without an id and
    /// comes back with the one the bridge stamped, and the browser's copies of
    /// existing entries carry no credentials — so what the UI caches afterwards
    /// has to be the far side's list, never its own.
    fn save(
        &self,
        servers: Vec<crate::config::RedisServer>,
    ) -> BoxFuture<'static, Result<Vec<crate::config::RedisServer>, BridgeError>>;
}

static SERVER_STORE: OnceLock<Arc<dyn BridgeServerStore>> = OnceLock::new();

/// A deleted key in the bridge's recycle bin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeTrashEntry {
    /// The bridge's id for it; empty on one being put.
    pub id: String,
    pub key: String,
    pub db: usize,
    pub pttl_ms: i64,
    pub deleted_at_ms: i64,
    pub deleted_by: String,
    /// The `DUMP`: sent with a put, answered by a fetch of one, empty in a
    /// listing.
    pub payload: Vec<u8>,
}

/// What the browser keeps on the bridge because a page keeps nothing past a
/// reload: the recycle bin and the metrics history (ADR 9, amended
/// 2026-10-17). The desktop keeps both in its own database.
pub trait BridgeStorage: Send + Sync + 'static {
    /// Keep a key that is about to be deleted.
    fn trash_put(&self, server_id: String, entry: BridgeTrashEntry) -> BoxFuture<'static, Result<(), BridgeError>>;
    /// The server's bin, newest first, without payloads.
    fn trash_list(&self, server_id: String) -> BoxFuture<'static, Result<Vec<BridgeTrashEntry>, BridgeError>>;
    /// One entry with its payload; `None` once it is gone.
    fn trash_get(
        &self,
        server_id: String,
        id: String,
    ) -> BoxFuture<'static, Result<Option<BridgeTrashEntry>, BridgeError>>;
    fn trash_remove(&self, server_id: String, id: String) -> BoxFuture<'static, Result<(), BridgeError>>;
    /// Keep one metrics sample, as JSON; the bridge keeps one a minute.
    fn metrics_put(
        &self,
        server_id: String,
        ts_ms: i64,
        sample: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), BridgeError>>;
    /// The samples since `from_ms`, oldest first, thinned to `max_points`.
    fn metrics_since(
        &self,
        server_id: String,
        from_ms: i64,
        max_points: usize,
    ) -> BoxFuture<'static, Result<Vec<Vec<u8>>, BridgeError>>;
}

static STORAGE: OnceLock<Arc<dyn BridgeStorage>> = OnceLock::new();

/// Install the bridge storage. The first call wins, as with the transport.
pub fn set_bridge_storage(storage: Arc<dyn BridgeStorage>) {
    if STORAGE.set(storage).is_err() {
        tracing::warn!("the bridge storage was already installed; keeping the first");
    }
}

/// The installed storage, or `None` — on the desktop, always.
pub fn bridge_storage() -> Option<Arc<dyn BridgeStorage>> {
    STORAGE.get().cloned()
}

/// Install the server-list sink. The first call wins, as with the transport.
pub fn set_bridge_server_store(store: Arc<dyn BridgeServerStore>) {
    if SERVER_STORE.set(store).is_err() {
        tracing::warn!("the bridge server store was already installed; keeping the first");
    }
}

/// The installed sink, or `None` before [`set_bridge_server_store`].
pub fn bridge_server_store() -> Option<Arc<dyn BridgeServerStore>> {
    SERVER_STORE.get().cloned()
}

/// A role on the bridge (ADR 17), as its account routes spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeRole {
    /// A full account that also manages the accounts and the invitations.
    Admin,
    Full,
    #[default]
    ReadOnly,
}

impl BridgeRole {
    /// Every role, in the order a picker offers them.
    pub const ALL: [BridgeRole; 3] = [Self::Admin, Self::Full, Self::ReadOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Full => "full",
            Self::ReadOnly => "read_only",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }
}

/// An MCP token of an account's (ADR 15) — its name, never the token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeToken {
    pub name: String,
    /// Unix seconds; `None` for one that does not expire.
    pub expires_at: Option<u64>,
    pub created_at: u64,
}

/// An account as the bridge describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeAccount {
    pub name: String,
    pub role: BridgeRole,
    pub disabled: bool,
    /// Locked by an administrator — a demonstration's: it changes neither
    /// its password nor its tokens, and the bridge keeps no rows of its own
    /// for it.
    pub locked: bool,
    /// `false` for an account that signs in through the trusted proxy.
    pub has_password: bool,
    /// Which shared entries it sees (ADR 13); `None` is every one.
    pub servers: Option<Vec<String>>,
    pub tokens: Vec<BridgeToken>,
    pub created_at: u64,
}

/// The signed-in account, and whether the bridge believes a sign-in proxy —
/// which is when an invitation may be marked for it — and takes requests
/// for an account, which an administrator then answers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeOwnAccount {
    pub account: BridgeAccount,
    pub proxy: bool,
    pub signup: bool,
}

/// A request for an account, waiting for an administrator (ADR 17, amended
/// 2026-10-18) — never its password.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeSignup {
    pub id: String,
    /// The name it asks for.
    pub name: String,
    /// What the person wrote for the administrator.
    pub note: String,
    /// The address it came from.
    pub peer: Option<String>,
    /// Unix seconds.
    pub requested_at: u64,
    pub expires_at: u64,
    /// An account has this name already: it cannot be approved.
    pub name_taken: bool,
}

/// What an invitation does when its link is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeInviteKind {
    /// Registers an account with this role and these rules; `proxy`: as the
    /// name the sign-in proxy asserts, with no password.
    Register {
        role: BridgeRole,
        servers: Option<Vec<String>>,
        proxy: bool,
    },
    /// Sets a new password on `account`.
    Reset { account: String },
}

/// An open invitation — never its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeInvite {
    pub id: String,
    pub kind: BridgeInviteKind,
    pub expires_at: u64,
    pub created_by: String,
    pub created_at: u64,
}

/// An invitation just made, with the code its link carries: shown once,
/// because the bridge keeps only its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeNewInvite {
    pub invite: BridgeInvite,
    pub code: String,
}

/// What an administrator changes on an account; `None` leaves a setting as
/// it is, and `servers: Some(None)` clears the rules (every shared entry).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeAccountChange {
    pub role: Option<BridgeRole>,
    pub servers: Option<Option<Vec<String>>>,
    pub disabled: Option<bool>,
    pub locked: Option<bool>,
}

/// One thing an account keeps to itself: a server entry or a saved script.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeOwnedItem {
    pub id: String,
    pub name: String,
}

/// What an account keeps to itself on the bridge — what its deletion has to
/// say what becomes of.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeOwned {
    pub servers: Vec<BridgeOwnedItem>,
    pub scripts: Vec<BridgeOwnedItem>,
}

impl BridgeOwned {
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.scripts.is_empty()
    }
}

/// What an account deleted before handovers existed left behind, seen by
/// nobody since.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeOrphans {
    pub owner: String,
    pub owned: BridgeOwned,
}

/// What an administrator asks the audit (ADR 18): every field narrows,
/// none is every line. `before` is the previous page's `next`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeAuditQuery {
    pub account: Option<String>,
    pub event: Option<String>,
    /// A server entry's id.
    pub server: Option<String>,
    pub since_ms: Option<i64>,
    pub before: Option<String>,
}

/// Someone using the bridge now, as `GET /v1/clients` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeClient {
    pub account: String,
    /// `browser`, `basic` (a script with HTTP Basic), `token` (an MCP token)
    /// or `proxy` (signed in by the trusted proxy).
    pub kind: String,
    /// The MCP token's name, for a token client.
    pub token: Option<String>,
    /// The address of its last request, and what a proxy in front said.
    pub peer: Option<String>,
    pub forwarded_for: Option<String>,
    /// Unix seconds.
    pub seen_at: u64,
    /// The server entries it used within the window, the latest first, by
    /// name — only those the asker may see.
    pub servers: Vec<String>,
}

/// Who is using the bridge: every client with a request in the last
/// `window_secs`, the most recently seen first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeClients {
    /// The bridge process that answered. Several bridges on one database
    /// each count their own.
    pub process: String,
    pub window_secs: u64,
    pub clients: Vec<BridgeClient>,
}

/// A page of the audit, newest first: each line the JSON object the bridge
/// kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeAuditPage {
    pub lines: Vec<String>,
    /// Where the next, older page starts; `None` when there is none.
    pub next: Option<String>,
    /// The bridge read its file only so far back for this page.
    pub stopped_early: bool,
}

/// What becomes of what an account kept to itself (ADR 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeDisposition {
    /// It becomes this account's.
    HandTo(String),
    /// It becomes every account's — passwords saved in it included.
    Share,
    /// It goes too.
    Delete,
}

/// Why an account request was refused, in the terms its page words it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeAccountErrorKind {
    /// Not allowed (`403`): a wrong current password, or not an administrator.
    Refused,
    /// In the way of something (`409`): the last administrator, a token name
    /// already taken.
    Conflict,
    /// Not acceptable as asked (`400`): a short password, a bad rule.
    Invalid,
    /// Not there (`404`): an account or invitation someone else removed.
    NotFound,
    /// Too many failed attempts from this address (`429`).
    TooManyAttempts,
    /// Anything else, the bridge unreachable included.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAccountError {
    pub kind: BridgeAccountErrorKind,
    /// The bridge's own words, for a refusal the page has no wording of.
    pub message: String,
}

impl BridgeAccountError {
    pub fn new(kind: BridgeAccountErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BridgeAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BridgeAccountError {}

pub type BridgeAccountResult<T> = BoxFuture<'static, Result<T, BridgeAccountError>>;

/// The account page's requests (ADR 17, amended 2026-10-18): one's own
/// account, and for an administrator everyone's, the invitations and the
/// requests for an account. The
/// page draws them; the browser entry point carries them, and the desktop,
/// which has no accounts, never installs one.
pub trait BridgeAccounts: Send + Sync + 'static {
    /// `GET /v1/account`.
    fn account(&self) -> BridgeAccountResult<BridgeOwnAccount>;
    /// A new password, given the current one. Every other login of the
    /// account ends; this one carries on with a new cookie.
    fn change_password(&self, current: String, new: String) -> BridgeAccountResult<()>;
    /// A new MCP token, answered once: on the signed-in account (`None`, a
    /// read-only account's own) or, by an administrator, on `account`.
    /// `expires` is a day, `2027-01-01`.
    fn issue_token(
        &self,
        account: Option<String>,
        name: String,
        expires: Option<String>,
    ) -> BridgeAccountResult<String>;
    fn revoke_token(&self, account: Option<String>, name: String) -> BridgeAccountResult<()>;
    /// Every account, for an administrator.
    fn accounts(&self) -> BridgeAccountResult<Vec<BridgeAccount>>;
    /// The account as it is after `change`.
    fn update_account(&self, name: String, change: BridgeAccountChange) -> BridgeAccountResult<BridgeAccount>;
    /// What `name` keeps to itself, which its deletion has to settle.
    fn owned(&self, name: String) -> BridgeAccountResult<BridgeOwned>;
    /// Delete `name`, settling what it kept to itself as `private` says.
    fn delete_account(&self, name: String, private: BridgeDisposition) -> BridgeAccountResult<()>;
    /// What accounts deleted before handovers left behind.
    fn orphans(&self) -> BridgeAccountResult<Vec<BridgeOrphans>>;
    /// Settle what `owner`, an account no more, left behind.
    fn settle_orphans(&self, owner: String, private: BridgeDisposition) -> BridgeAccountResult<()>;
    /// A page of the audit, for an administrator (ADR 18).
    fn audit(&self, query: BridgeAuditQuery) -> BridgeAccountResult<BridgeAuditPage>;
    /// Who is using the bridge now, and on which servers; a full
    /// account's question (`GET /v1/clients`).
    fn clients(&self) -> BridgeAccountResult<BridgeClients>;
    /// A link that sets a new password on `name`, once.
    fn reset_link(&self, name: String) -> BridgeAccountResult<BridgeNewInvite>;
    /// The open invitations.
    fn invites(&self) -> BridgeAccountResult<Vec<BridgeInvite>>;
    /// An invitation to register one account, valid for `days`.
    fn create_invite(
        &self,
        role: BridgeRole,
        servers: Option<Vec<String>>,
        proxy: bool,
        days: u64,
    ) -> BridgeAccountResult<BridgeNewInvite>;
    fn revoke_invite(&self, id: String) -> BridgeAccountResult<()>;
    /// The requests for an account waiting for an answer, oldest first.
    fn signups(&self) -> BridgeAccountResult<Vec<BridgeSignup>>;
    /// Make the account the request `id` asks for, with `role` and
    /// `servers`; answers it.
    fn approve_signup(
        &self,
        id: String,
        role: BridgeRole,
        servers: Option<Vec<String>>,
    ) -> BridgeAccountResult<BridgeAccount>;
    fn reject_signup(&self, id: String) -> BridgeAccountResult<()>;
    /// The address an invitation's `code` is sent as: the sign-in page,
    /// with the code in the part after `#` that no request carries.
    fn invite_link(&self, code: &str) -> String;
}

static ACCOUNTS: OnceLock<Arc<dyn BridgeAccounts>> = OnceLock::new();

/// Install the account requests. The first call wins, as with the transport.
pub fn set_bridge_accounts(accounts: Arc<dyn BridgeAccounts>) {
    if ACCOUNTS.set(accounts).is_err() {
        tracing::warn!("the bridge accounts were already installed; keeping the first");
    }
}

/// The installed account requests, or `None` — on the desktop, always.
pub fn bridge_accounts() -> Option<Arc<dyn BridgeAccounts>> {
    ACCOUNTS.get().cloned()
}

/// One of an account's own rows of the page's data (ADR 17): a list of key
/// names under a server and database, or the preferences file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgePageRow {
    /// `favorites`, `recent_keys`, `search_history` or `preferences`.
    pub table: String,
    pub key: String,
    pub value: String,
}

/// A key's tag and note on one server, and who left them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeKeyNote {
    pub server_id: String,
    pub key: String,
    pub tag: Option<String>,
    pub note: String,
    pub by: String,
    pub at: u64,
}

/// A saved Lua script: the page's own JSON for it, and whether it is shared.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeScript {
    pub id: String,
    pub json: String,
    pub shared: bool,
}

/// What the page keeps on the bridge, as it is when the page starts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgePageSeed {
    /// Who is signed in.
    pub account: String,
    /// Whether that account may change nothing anybody else sees — the
    /// script library's saves, a key's tag and note.
    pub read_only: bool,
    /// Whether the bridge keeps the account's own rows. Not a locked
    /// account's: its favorites, history and preferences stay in the page.
    pub rows_kept: bool,
    pub rows: Vec<BridgePageRow>,
    pub key_notes: Vec<BridgeKeyNote>,
    pub scripts: Vec<BridgeScript>,
}

/// The page's own data, sent to the bridge as it changes (ADR 17): what the
/// browser used to keep in the tab until a reload emptied it. Sent and not
/// waited for — the callers are a store's commit and a preferences save,
/// which have nothing to wait with — and in the order they were made, so a
/// later change never lands before an earlier one.
pub trait BridgePageData: Send + Sync + 'static {
    /// One of the account's rows, or `None` to forget it.
    fn put_row(&self, table: String, key: String, value: Option<String>);
    /// A key's tag and note on a server; neither forgets them.
    fn put_key_note(&self, server_id: String, key: String, tag: Option<String>, note: String);
    fn put_script(&self, script: BridgeScript);
    fn delete_script(&self, id: String);
    /// Whether a change made here has not reached the bridge yet. A refresh
    /// read while one is on its way would put back what it replaces.
    fn writes_pending(&self) -> bool;
    /// Read what other accounts changed now rather than at the next round:
    /// a server was opened, the script library was, the page came back.
    fn refresh_soon(&self);
}

static PAGE_DATA: OnceLock<Arc<dyn BridgePageData>> = OnceLock::new();

/// Install where the page's data goes. The first call wins, as with the
/// transport.
pub fn set_bridge_page_data(page_data: Arc<dyn BridgePageData>) {
    if PAGE_DATA.set(page_data).is_err() {
        tracing::warn!("the bridge page data was already installed; keeping the first");
    }
}

/// Where the page's data goes, or `None` — on the desktop, always.
pub fn bridge_page_data() -> Option<Arc<dyn BridgePageData>> {
    PAGE_DATA.get().cloned()
}

/// A Redis connection whose transport is the bridge.
#[derive(Clone)]
pub struct BridgeConn {
    transport: Arc<dyn BridgeTransport>,
    server_id: String,
    db: usize,
    session: Option<String>,
    /// Sent with every request: the answer to the question the bridge's
    /// policy would otherwise ask of a destructive command (`ServerDb::confirmed`).
    confirm: Option<String>,
}

impl std::fmt::Debug for BridgeConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConn")
            .field("server_id", &self.server_id)
            .field("db", &self.db)
            .field("session", &self.session.is_some())
            .finish()
    }
}

impl BridgeConn {
    /// A pooled connection: the bridge picks whichever backend connection is
    /// free, which is right for everything that carries no session state.
    pub fn new(transport: Arc<dyn BridgeTransport>, server_id: impl Into<String>, db: usize) -> Self {
        Self {
            transport,
            server_id: server_id.into(),
            db,
            session: None,
            confirm: None,
        }
    }

    /// Carry `token` as the confirmation on every request from here on.
    pub fn with_confirmation(mut self, token: Option<String>) -> Self {
        self.confirm = token;
        self
    }

    /// Pin every command to one backend connection. `session` is the token
    /// the bridge handed out; the caller is responsible for releasing it.
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// The transport behind this connection, for opening a second one to the
    /// same bridge without threading it through the caller.
    pub fn transport(&self) -> Arc<dyn BridgeTransport> {
        Arc::clone(&self.transport)
    }

    fn request(&self, commands: Vec<Vec<u8>>, pipeline: Option<PipelineSpec>) -> BridgeRequest {
        BridgeRequest {
            server_id: self.server_id.clone(),
            db: self.db,
            session: self.session.clone(),
            commands,
            pipeline,
            fanout_masters: false,
            fanout_nodes: Vec::new(),
            confirm: self.confirm.clone(),
        }
    }

    /// Run `commands` on every master of this server, through the bridge.
    ///
    /// The replies come back in the bridge's node order, paired with the
    /// `host:port` label of the node that produced each.
    pub async fn fanout_masters(&self, commands: Vec<Vec<u8>>) -> Result<(Vec<String>, Vec<Value>), BridgeError> {
        self.fanout(commands, Vec::new()).await
    }

    /// Fan out to named nodes only: `commands[i]` goes to `nodes[i]`.
    ///
    /// The replies come back in the bridge's own node order with its own
    /// labels, so the caller re-aligns by label rather than trusting that two
    /// independent topology discoveries agree.
    pub async fn fanout_nodes(
        &self,
        commands: Vec<Vec<u8>>,
        nodes: Vec<String>,
    ) -> Result<(Vec<String>, Vec<Value>), BridgeError> {
        self.fanout(commands, nodes).await
    }

    async fn fanout(
        &self,
        commands: Vec<Vec<u8>>,
        nodes: Vec<String>,
    ) -> Result<(Vec<String>, Vec<Value>), BridgeError> {
        let mut request = self.request(commands, None);
        request.fanout_masters = true;
        request.fanout_nodes = nodes;
        let reply = self.transport.send(request).await?;
        let values = reply
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                redis::parse_redis_value(frame)
                    .map_err(|e| BridgeError::transport(format!("fan-out reply {index} is not RESP: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((reply.nodes, values))
    }
}

/// Turn one reply frame into a value, or say which frame was unreadable.
fn parse_frame(index: usize, frame: &[u8]) -> Result<Value, RedisError> {
    redis::parse_redis_value(frame).map_err(|e| {
        RedisError::from((
            ErrorKind::Parse,
            "redis bridge",
            format!("reply {index} from the bridge is not RESP: {e}"),
        ))
    })
}

impl BridgeConn {
    /// One command, one reply. The shared body behind `ConnectionLike` on the
    /// host and [`BridgeQuery`] in the browser.
    pub async fn send_command(&self, cmd: &Cmd) -> Result<Value, RedisError> {
        let request = self.request(vec![cmd.get_packed_command()], None);
        let reply = self.transport.send(request).await.map_err(RedisError::from)?;
        let frame = reply.frames.first().ok_or_else(|| {
            RedisError::from((
                ErrorKind::Parse,
                "redis bridge",
                "the bridge returned no reply".to_string(),
            ))
        })?;
        parse_frame(0, frame)
    }

    /// One command's reply as `T`, the way redis-rs's `Cmd::query_async`
    /// hands it over: an error reply — at the top, or inside a collection —
    /// is an `Err` carrying the server's kind, never a value. What
    /// [`BridgeQuery`] answers with in the browser.
    ///
    /// [`send_command`](Self::send_command) is the raw reply on purpose (the
    /// terminal shows an error as the server said it); redis-rs applies
    /// `extract_error` after its own `req_packed_command` too. The browser's
    /// stand-in once skipped it, and every error reply there became a value
    /// or an untyped parse failure: a script's `NOSCRIPT` read as a hit with
    /// the error as its result and no `SCRIPT LOAD`, a refused write as done.
    pub async fn query_command<T: FromRedisValue>(&self, cmd: &Cmd) -> Result<T, RedisError> {
        let value = self.send_command(cmd).await?;
        T::from_redis_value(value.extract_error()?).map_err(RedisError::from)
    }

    /// A pipeline's `count` replies, after `offset` are discarded.
    pub async fn send_pipeline(
        &self,
        pipeline: &Pipeline,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Value>, RedisError> {
        // The commands, not the packed pipeline: redis-rs adds `MULTI`/`EXEC`
        // at pack time from `is_transaction`, so the bridge rebuilds the same
        // pipeline from the same parts and the offset stays meaningful.
        let commands: Vec<Vec<u8>> = pipeline.cmd_iter().map(|c| c.get_packed_command()).collect();
        let request = self.request(
            commands,
            Some(PipelineSpec {
                offset,
                count,
                atomic: pipeline.is_transaction(),
            }),
        );
        let reply = self.transport.send(request).await.map_err(RedisError::from)?;
        let frames = reply.frames;
        if frames.len() != count {
            return Err(RedisError::from((
                ErrorKind::Parse,
                "redis bridge",
                format!("expected {count} replies from the bridge, got {}", frames.len()),
            )));
        }
        frames
            .iter()
            .enumerate()
            .map(|(index, frame)| parse_frame(index, frame))
            .collect()
    }

    /// Run `pipeline` and finish it the way redis-rs's `Pipeline::query_async`
    /// does on the host — the framing from [`PipelineSpec::for_pipeline`], the
    /// replies through [`Replayed`]. A transaction yields its commands'
    /// results rather than the raw `EXEC` frame, and an `.ignore()`d command
    /// yields none.
    pub async fn query_pipeline<T: FromRedisValue>(&self, pipeline: &Pipeline) -> Result<T, RedisError> {
        if pipeline.is_empty() {
            return T::from_redis_value(Value::Array(Vec::new())).map_err(RedisError::from);
        }
        let spec = PipelineSpec::for_pipeline(pipeline.len(), pipeline.is_transaction());
        let replies = self.send_pipeline(pipeline, spec.offset, spec.count).await?;
        pipeline.query(&mut Replayed(Some(replies)))
    }

    fn db(&self) -> i64 {
        self.db as i64
    }
}

/// On the host the enum variant is reached through redis-rs's own trait, so
/// every `cmd(...).query_async(&mut conn)` in the workspace keeps working.
#[cfg(not(target_family = "wasm"))]
impl ConnectionLike for BridgeConn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(self.send_command(cmd))
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(self.send_pipeline(pipeline, offset, count))
    }

    fn get_db(&self) -> i64 {
        self.db()
    }
}

/// What `aio` would have put on `Cmd`, for the browser where it cannot.
///
/// An inherent method wins over a trait method, so on the host redis-rs's own
/// `query_async` is chosen and this is never consulted; in the browser the
/// inherent one does not exist and this fills in under the same name. Call
/// sites are therefore identical on both targets — only the `use` differs.
#[cfg(target_family = "wasm")]
pub trait BridgeQuery {
    fn query_async<T: FromRedisValue>(&self, conn: &mut RedisAsyncConn) -> impl Future<Output = Result<T, RedisError>>;
    fn exec_async(&self, conn: &mut RedisAsyncConn) -> impl Future<Output = Result<(), RedisError>>;
}

#[cfg(target_family = "wasm")]
impl BridgeQuery for Cmd {
    async fn query_async<T: FromRedisValue>(&self, conn: &mut RedisAsyncConn) -> Result<T, RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        conn.query_command(self).await
    }

    /// redis-rs's is `query_async::<()>`: whatever the reply, unless it is
    /// an error.
    async fn exec_async(&self, conn: &mut RedisAsyncConn) -> Result<(), RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        conn.query_command::<()>(self).await
    }
}

/// The same for a pipeline.
#[cfg(target_family = "wasm")]
pub trait BridgePipeline {
    fn query_async<T: FromRedisValue>(&self, conn: &mut RedisAsyncConn) -> impl Future<Output = Result<T, RedisError>>;
    fn exec_async(&self, conn: &mut RedisAsyncConn) -> impl Future<Output = Result<(), RedisError>>;
}

#[cfg(target_family = "wasm")]
impl BridgePipeline for Pipeline {
    async fn query_async<T: FromRedisValue>(&self, conn: &mut RedisAsyncConn) -> Result<T, RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        conn.query_pipeline(self).await
    }

    async fn exec_async(&self, conn: &mut RedisAsyncConn) -> Result<(), RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        conn.query_pipeline::<()>(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account page is the web version's (ADR 17): a process that never
    /// installed a transport — the desktop, and every native test — has no
    /// accounts to manage, and draws no menu item that would open them.
    #[test]
    fn the_desktop_has_no_accounts_to_manage() {
        assert!(!signed_in_to_bridge());
        assert!(bridge_accounts().is_none());
        assert!(bridge_page_data().is_none());
    }

    #[test]
    fn a_role_is_spelled_as_the_bridge_spells_it() {
        for role in BridgeRole::ALL {
            assert_eq!(BridgeRole::from_name(role.as_str()), Some(role));
        }
        assert_eq!(BridgeRole::ReadOnly.as_str(), "read_only");
        assert_eq!(BridgeRole::from_name("owner"), None);
    }
    use std::sync::Mutex;

    /// `args` packed the way redis-rs packs a command. The name goes in as a
    /// variable so the `cmd("…")` scans in `tests/` do not count these.
    fn packed(args: &[&[u8]]) -> Vec<u8> {
        let mut command = redis::cmd(std::str::from_utf8(args[0]).expect("name"));
        for arg in &args[1..] {
            command.arg(*arg);
        }
        command.get_packed_command()
    }

    fn label(args: &[&str]) -> String {
        crate::init_commands_json(include_bytes!("../../../assets/commands.json").to_vec());
        let args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        describe_packed_command(&packed(&args))
    }

    #[test]
    fn a_label_names_the_command_its_key_and_how_many_arguments_follow() {
        assert_eq!(label(&["get", "user:42"]), "GET user:42");
        assert_eq!(
            label(&["HSET", "user:1", "name", "alice", "age", "30"]),
            "HSET user:1 (+4 args)"
        );
        assert_eq!(label(&["ACL", "LIST"]), "ACL LIST");
        assert_eq!(label(&["OBJECT", "ENCODING", "user:1"]), "OBJECT ENCODING user:1");
        assert_eq!(
            label(&["MEMORY", "USAGE", "user:1", "SAMPLES", "0"]),
            "MEMORY USAGE user:1 (+2 args)"
        );
        assert_eq!(label(&["JSON.GET", "doc", "$.a"]), "JSON.GET doc (+1 arg)");
        assert_eq!(label(&["VSIM", "vs", "ELE", "a"]), "VSIM vs (+2 args)");
        assert_eq!(label(&["PING"]), "PING");
    }

    #[test]
    fn a_label_never_shows_an_argument_that_may_be_a_credential() {
        for (args, expected) in [
            (vec!["AUTH", "hunter2"], "AUTH (+1 arg)"),
            (vec!["AUTH", "alice", "hunter2"], "AUTH (+2 args)"),
            (vec!["CONFIG", "SET", "requirepass", "hunter2"], "CONFIG SET (+2 args)"),
            (
                vec!["ACL", "SETUSER", "alice", "on", ">hunter2"],
                "ACL SETUSER (+3 args)",
            ),
            (vec!["HELLO", "3", "AUTH", "alice", "hunter2"], "HELLO (+4 args)"),
            (vec!["CLIENT", "SETNAME", "hunter2"], "CLIENT SETNAME (+1 arg)"),
            (vec!["EVAL", "return 'hunter2'", "0"], "EVAL (+2 args)"),
            // Unknown to the table: counted, not shown.
            (vec!["VENDOR.LOGIN", "hunter2"], "VENDOR.LOGIN (+1 arg)"),
        ] {
            let shown = label(&args);
            assert_eq!(shown, expected, "{args:?}");
            assert!(!shown.contains("hunter2"), "{shown}");
        }
    }

    #[test]
    fn a_key_that_would_read_ambiguously_is_quoted_and_a_long_one_cut() {
        assert_eq!(label(&["GET", "my key"]), r#"GET "my key""#);
        assert_eq!(label(&["GET", ""]), r#"GET """#);
        let long = "k".repeat(100);
        assert_eq!(label(&["GET", &long]), format!("GET {}…", "k".repeat(LABEL_KEY_CHARS)));
        crate::init_commands_json(include_bytes!("../../../assets/commands.json").to_vec());
        let binary = describe_packed_command(&packed(&[b"GET", b"\xff\x00k"]));
        // Not UTF-8: replaced, quoted, and a NUL shown as `\0`.
        assert_eq!(binary, "GET \"\u{fffd}\\0k\"");
    }

    #[test]
    fn a_long_batch_is_labelled_up_to_the_cap_and_then_counted() {
        let commands: Vec<Vec<u8>> = (0..DESCRIBED_COMMANDS + 8)
            .map(|i| packed(&[b"TYPE", format!("k{i}").as_bytes()]))
            .collect();
        crate::init_commands_json(include_bytes!("../../../assets/commands.json").to_vec());
        let labels = describe_commands(&commands);
        assert_eq!(labels.len(), DESCRIBED_COMMANDS + 1);
        assert_eq!(labels[0], "TYPE k0");
        assert_eq!(labels[DESCRIBED_COMMANDS], "… +8 more");
        assert_eq!(describe_commands(&commands[..2]), ["TYPE k0", "TYPE k1"]);
    }

    #[test]
    fn a_frame_that_is_not_a_packed_command_is_said_to_be_one() {
        assert_eq!(describe_packed_command(b"+OK\r\n"), "(unreadable command)");
        assert_eq!(
            describe_packed_command(b"*2\r\n$3\r\nGET\r\n$9\r\nshort"),
            "(unreadable command)"
        );
        assert_eq!(describe_packed_command(b"*0\r\n"), "(empty command)");
    }

    /// Records what it was asked to send and replays canned frames.
    struct Recorder {
        seen: Mutex<Vec<BridgeRequest>>,
        reply: Result<BridgeReply, BridgeError>,
    }

    impl BridgeTransport for Recorder {
        fn send(&self, request: BridgeRequest) -> BoxFuture<'static, Result<BridgeReply, BridgeError>> {
            self.seen.lock().expect("lock").push(request);
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }

        /// These tests are about what crosses the wire for a command, not
        /// about sessions; a fixed token keeps them out of the way.
        fn open_session(&self, _server_id: String, _db: usize) -> BoxFuture<'static, Result<String, BridgeError>> {
            Box::pin(async { Ok("test-session".to_string()) })
        }

        fn close_session(&self, _session: String) -> BoxFuture<'static, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }

        fn unlock_writes(&self, _server_id: String, _confirm: String) -> BoxFuture<'static, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }

        fn lock_writes(&self, _server_id: String) -> BoxFuture<'static, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }

        fn sign_out(&self) -> BoxFuture<'static, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }

        fn open_watch(
            &self,
            _server_id: String,
            _db: usize,
            _kind: WatchKind,
            _channels: Vec<String>,
            _confirm: Option<String>,
        ) -> BoxFuture<'static, Result<WatchOpened, BridgeError>> {
            Box::pin(async { Err(BridgeError::transport("no watches here")) })
        }

        fn poll_watch(&self, _token: String) -> BoxFuture<'static, Result<WatchBatch, BridgeError>> {
            Box::pin(async { Ok(WatchBatch::default()) })
        }

        fn release_watch(&self, _token: String) {}

        fn diagnose(&self, _server_id: String) -> BoxFuture<'static, Result<Vec<DiagReportRow>, BridgeError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn kill_running(
            &self,
            _server_id: String,
            _target: KillTarget,
        ) -> BoxFuture<'static, Result<Vec<KillReply>, BridgeError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    /// The desktop dials Redis itself: with no transport installed there is
    /// no bridge login, so nothing offers "Sign out" and asking for one is
    /// an error rather than a silent success. (No test in this crate
    /// installs a transport process-wide — they hand theirs to `BridgeConn`.)
    #[test]
    fn without_a_bridge_there_is_no_login_to_end() {
        assert!(!signed_in_to_bridge());
        let err = smol::block_on(sign_out_of_bridge()).expect_err("nothing to sign out of");
        assert_eq!(err.kind, BridgeErrorKind::Transport);
    }

    /// A request too large for the bridge says nothing about the link: as a
    /// transport failure it was a dropped connection, which drops the
    /// pooled client and puts "connection lost" over a save that only had
    /// to be smaller.
    #[test]
    fn a_request_too_large_is_not_a_lost_link() {
        use crate::error::{ConnectionErrorKind, Error};
        let kind = |kind: BridgeErrorKind| Error::from(RedisError::from(BridgeError::new(kind, "x"))).connection_kind();
        assert_eq!(kind(BridgeErrorKind::Transport), ConnectionErrorKind::Network);
        assert_ne!(kind(BridgeErrorKind::TooLarge), ConnectionErrorKind::Network);
    }

    fn frames(frames: Vec<Vec<u8>>) -> Result<BridgeReply, BridgeError> {
        Ok(BridgeReply {
            frames,
            nodes: Vec::new(),
        })
    }

    fn recorder(reply: Result<BridgeReply, BridgeError>) -> Arc<Recorder> {
        Arc::new(Recorder {
            seen: Mutex::new(Vec::new()),
            reply,
        })
    }

    #[test]
    fn a_command_is_sent_packed_and_its_reply_parsed() {
        let rec = recorder(frames(vec![b"+PONG\r\n".to_vec()]));
        let mut conn = BridgeConn::new(rec.clone(), "srv", 2);
        let value = smol::block_on(conn.req_packed_command(&Cmd::new().arg("PING").clone())).expect("reply");
        assert_eq!(value, Value::SimpleString("PONG".to_string()));

        let seen = rec.seen.lock().expect("lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].server_id, "srv");
        assert_eq!(seen[0].db, 2);
        assert!(seen[0].pipeline.is_none());
        assert_eq!(seen[0].commands, vec![Cmd::new().arg("PING").get_packed_command()]);
    }

    /// An error reply is an error, with the server's kind — as redis-rs's
    /// own `query_async` gives it on the desktop — whatever type was asked
    /// for, `()` included. The browser's stand-in used to hand `NOSCRIPT`
    /// back as a value: a script that was not cached ran as a "hit" whose
    /// result was the error, and nothing loaded it.
    #[test]
    fn an_error_reply_is_an_error_whatever_type_was_asked_for() {
        let rec = recorder(frames(vec![
            b"-NOSCRIPT No matching script. Please use EVAL.\r\n".to_vec(),
        ]));
        let conn = BridgeConn::new(rec, "srv", 0);
        let evalsha = Cmd::new().arg("EVALSHA").arg("deadbeef").arg(0).clone();
        let as_value = smol::block_on(conn.query_command::<Value>(&evalsha)).expect_err("an error, not a value");
        assert_eq!(as_value.code(), Some("NOSCRIPT"));
        let as_text = smol::block_on(conn.query_command::<String>(&evalsha)).expect_err("an error");
        assert_eq!(
            as_text.code(),
            Some("NOSCRIPT"),
            "the server's kind, not a parse failure"
        );
        let as_unit = smol::block_on(conn.query_command::<()>(&evalsha)).expect_err("a refused write is not done");
        assert_eq!(as_unit.code(), Some("NOSCRIPT"));

        // Inside a collection too, as redis-rs does it.
        let rec = recorder(frames(vec![
            b"*2\r\n:1\r\n-WRONGTYPE Operation against a key\r\n".to_vec(),
        ]));
        let conn = BridgeConn::new(rec, "srv", 0);
        let nested = smol::block_on(conn.query_command::<Value>(&Cmd::new().arg("X").clone())).expect_err("nested");
        assert_eq!(nested.code(), Some("WRONGTYPE"));

        // And a reply that is not an error is the value it always was.
        let rec = recorder(frames(vec![b"$3\r\nabc\r\n".to_vec()]));
        let conn = BridgeConn::new(rec, "srv", 0);
        let ok: String = smol::block_on(conn.query_command(&Cmd::new().arg("GET").arg("k").clone())).expect("value");
        assert_eq!(ok, "abc");
    }

    #[test]
    fn a_session_rides_along_with_every_command() {
        let rec = recorder(frames(vec![b"+OK\r\n".to_vec()]));
        let mut conn = BridgeConn::new(rec.clone(), "srv", 0).with_session("tok");
        assert_eq!(conn.session(), Some("tok"));
        let _ = smol::block_on(conn.req_packed_command(&Cmd::new().arg("MULTI").clone()));
        assert_eq!(rec.seen.lock().expect("lock")[0].session.as_deref(), Some("tok"));
    }

    #[test]
    fn a_pipeline_travels_as_its_commands_plus_its_framing() {
        let rec = recorder(frames(vec![b":1\r\n".to_vec(), b":2\r\n".to_vec()]));
        let mut conn = BridgeConn::new(rec.clone(), "srv", 0);
        let mut pipe = redis::pipe();
        pipe.atomic().cmd("INCR").arg("a").cmd("INCR").arg("b");
        let values = smol::block_on(conn.req_packed_commands(&pipe, 1, 2)).expect("replies");
        assert_eq!(values, vec![Value::Int(1), Value::Int(2)]);

        let seen = rec.seen.lock().expect("lock");
        let spec = seen[0].pipeline.expect("pipeline spec");
        assert_eq!(spec.offset, 1);
        assert_eq!(spec.count, 2);
        assert!(spec.atomic, "an atomic pipeline must say so, or EXEC is lost");
        assert_eq!(seen[0].commands.len(), 2, "MULTI/EXEC are added by the far side");
    }

    #[test]
    fn a_transaction_is_framed_and_finished_the_way_redis_rs_does_it() {
        // The bridge answers a transaction with the one EXEC array; the page
        // gets the commands' results, minus what `.ignore()` marked.
        let rec = recorder(frames(vec![b"*2\r\n+OK\r\n$1\r\n7\r\n".to_vec()]));
        let conn = BridgeConn::new(rec.clone(), "srv", 0);
        let mut pipe = redis::pipe();
        pipe.atomic().cmd("SET").arg("a").arg(7).ignore().cmd("GET").arg("a");
        let (got,): (String,) = smol::block_on(conn.query_pipeline(&pipe)).expect("transaction");
        assert_eq!(got, "7");

        let seen = rec.seen.lock().expect("lock");
        let spec = seen[0].pipeline.expect("pipeline spec");
        // `MULTI` and two `QUEUED`s skipped, then the one `EXEC` reply — any
        // other count hands replies to the wrong caller of a shared connection.
        assert_eq!(spec, PipelineSpec::for_pipeline(2, true));
        assert_eq!((spec.offset, spec.count), (3, 1));
    }

    #[test]
    fn a_plain_pipeline_drops_what_was_ignored() {
        let rec = recorder(frames(vec![b"+OK\r\n".to_vec(), b":5\r\n".to_vec()]));
        let conn = BridgeConn::new(rec.clone(), "srv", 0);
        let mut pipe = redis::pipe();
        pipe.cmd("SET").arg("n").arg(4).ignore().cmd("INCR").arg("n");
        let (n,): (i64,) = smol::block_on(conn.query_pipeline(&pipe)).expect("pipeline");
        assert_eq!(n, 5);
        let spec = rec.seen.lock().expect("lock")[0].pipeline.expect("pipeline spec");
        assert_eq!((spec.offset, spec.count, spec.atomic), (0, 2, false));
    }

    #[test]
    fn a_short_pipeline_reply_is_an_error_not_a_silent_truncation() {
        let rec = recorder(frames(vec![b":1\r\n".to_vec()]));
        let mut conn = BridgeConn::new(rec, "srv", 0);
        let mut pipe = redis::pipe();
        pipe.cmd("INCR").arg("a").cmd("INCR").arg("b");
        assert!(smol::block_on(conn.req_packed_commands(&pipe, 0, 2)).is_err());
    }

    #[test]
    fn a_fan_out_asks_the_far_side_and_keeps_the_node_labels() {
        let rec = Arc::new(Recorder {
            seen: Mutex::new(Vec::new()),
            reply: Ok(BridgeReply {
                frames: vec![b"+OK\r\n".to_vec(), b"+OK\r\n".to_vec()],
                nodes: vec!["10.0.0.1:6379".to_string(), "10.0.0.2:6379".to_string()],
            }),
        });
        let conn = BridgeConn::new(rec.clone(), "srv", 0);
        let (nodes, values) =
            smol::block_on(conn.fanout_masters(vec![Cmd::new().arg("BGSAVE").get_packed_command()])).expect("fan-out");
        assert_eq!(nodes, vec!["10.0.0.1:6379", "10.0.0.2:6379"]);
        assert_eq!(values, vec![Value::Okay, Value::Okay]);
        // The request says fan out; it never names a node itself, because the
        // browser has no business holding dialable addresses.
        let seen = rec.seen.lock().expect("lock");
        assert!(seen[0].fanout_masters);
        assert!(seen[0].pipeline.is_none());
    }

    #[test]
    fn aiming_at_named_nodes_sends_the_labels_and_nothing_positional() {
        let rec = Arc::new(Recorder {
            seen: Mutex::new(Vec::new()),
            reply: Ok(BridgeReply {
                frames: vec![b":1\r\n".to_vec()],
                nodes: vec!["10.0.0.2:6379".to_string()],
            }),
        });
        let conn = BridgeConn::new(rec.clone(), "srv", 0);
        let (nodes, values) = smol::block_on(conn.fanout_nodes(
            vec![Cmd::new().arg("SCAN").arg(0).get_packed_command()],
            vec!["10.0.0.2:6379".to_string()],
        ))
        .expect("fan-out");
        assert_eq!(nodes, vec!["10.0.0.2:6379"]);
        assert_eq!(values, vec![Value::Int(1)]);

        let seen = rec.seen.lock().expect("lock");
        assert!(seen[0].fanout_masters);
        assert_eq!(seen[0].fanout_nodes, vec!["10.0.0.2:6379"]);
    }

    #[test]
    fn a_plain_fan_out_names_no_nodes() {
        let rec = recorder(frames(vec![b"+OK\r\n".to_vec()]));
        let conn = BridgeConn::new(rec.clone(), "srv", 0);
        let _ = smol::block_on(conn.fanout_masters(vec![Cmd::new().arg("BGSAVE").get_packed_command()]));
        assert!(rec.seen.lock().expect("lock")[0].fanout_nodes.is_empty());
    }

    /// What a dialog answered rides on every request of a confirmed
    /// connection — a pipeline's and a fan-out's too — and nothing else's.
    #[test]
    fn a_confirmed_connection_sends_its_answer_with_every_request() {
        let rec = recorder(frames(vec![b"+OK\r\n".to_vec()]));
        let mut plain = BridgeConn::new(rec.clone(), "srv", 0);
        let _ = smol::block_on(plain.req_packed_command(&Cmd::new().arg("FLUSHDB").clone()));
        let mut confirmed = BridgeConn::new(rec.clone(), "srv", 0).with_confirmation(Some("production".to_string()));
        let _ = smol::block_on(confirmed.req_packed_command(&Cmd::new().arg("FLUSHDB").clone()));
        let _ = smol::block_on(confirmed.fanout_masters(vec![Cmd::new().arg("FLUSHALL").get_packed_command()]));
        let seen = rec.seen.lock().expect("lock");
        assert_eq!(seen[0].confirm, None);
        assert_eq!(seen[1].confirm.as_deref(), Some("production"));
        assert_eq!(seen[2].confirm.as_deref(), Some("production"));
    }

    #[test]
    fn a_confirmation_refusal_reaches_the_caller_as_an_error() {
        let rec = recorder(Err(BridgeError::new(
            BridgeErrorKind::ConfirmationRequired {
                danger_key: "danger.flushall".to_string(),
                type_name_required: false,
            },
            "needs confirmation",
        )));
        let mut conn = BridgeConn::new(rec, "srv", 0);
        let err =
            smol::block_on(conn.req_packed_command(&Cmd::new().arg("FLUSHALL").clone())).expect_err("must not succeed");
        assert!(err.to_string().contains("needs confirmation"), "got {err}");
    }

    #[test]
    fn a_garbled_reply_is_reported_with_its_index() {
        let rec = recorder(frames(vec![b"not resp".to_vec()]));
        let mut conn = BridgeConn::new(rec, "srv", 0);
        let err =
            smol::block_on(conn.req_packed_command(&Cmd::new().arg("PING").clone())).expect_err("must not succeed");
        assert!(err.to_string().contains("not RESP"), "got {err}");
    }
}
