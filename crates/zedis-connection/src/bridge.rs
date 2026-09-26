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
#[cfg(target_family = "wasm")]
use redis::FromRedisValue;
use redis::{Cmd, ErrorKind, Pipeline, RedisError, Value};
// `aio` is what carries `ConnectionLike` and `Cmd::query_async`, and it cannot
// be built for `wasm32-unknown-unknown` — it insists on a socket runtime. The
// browser gets the same call sites from the traits at the bottom of this file
// instead (ADR 9).
#[cfg(not(target_family = "wasm"))]
use redis::{RedisFuture, aio::ConnectionLike};
use std::sync::{Arc, OnceLock};

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
}

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
            BridgeErrorKind::Unauthorized | BridgeErrorKind::ConfirmationRequired { .. } => ErrorKind::Client,
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
        let value = conn.send_command(self).await?;
        T::from_redis_value(value).map_err(RedisError::from)
    }

    async fn exec_async(&self, conn: &mut RedisAsyncConn) -> Result<(), RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        conn.send_command(self).await?;
        Ok(())
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
        let count = self.len();
        let values = conn.send_pipeline(self, 0, count).await?;
        T::from_redis_value(Value::Array(values)).map_err(RedisError::from)
    }

    async fn exec_async(&self, conn: &mut RedisAsyncConn) -> Result<(), RedisError> {
        let RedisAsyncConn::Bridge(conn) = conn;
        let count = self.len();
        conn.send_pipeline(self, 0, count).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
