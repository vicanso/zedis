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

//! The HTTP surface: one route that forwards a frame, plus session
//! bookkeeping and a server listing so the browser knows what it may pick.
//!
//! The bridge holds the credentials. A caller names a server by its id and
//! never sees a host, a password or a key (ADR 9).

use crate::static_files::{Representation, WebBuild};
use crate::{auth, policy, resp, session::Sessions, static_files};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use redis::aio::ConnectionLike;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::Path as FsPath;
use uuid::Uuid;
use zedis_connection::{RedisServer, get_connection_manager, get_server, get_servers, save_servers};

#[derive(Clone)]
pub struct AppState {
    pub accounts: auth::Accounts,
    pub sessions: Sessions,
    pub logins: auth::Logins,
    /// Mark the login cookie `Secure`. Off only for a plain-http local run.
    pub secure_cookie: bool,
    /// `--static <dir>`: serve the web build from this directory instead of
    /// the one compiled in.
    pub web_root: Option<std::path::PathBuf>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/login", post(login))
        .route("/v1/logout", post(logout))
        .route("/v1/servers", get(servers).post(add_server))
        .route("/v1/servers/{id}", delete(delete_server).put(update_server))
        .route("/v1/exec", post(exec))
        .route("/v1/session", post(open_session))
        .route("/v1/session/{token}", delete(close_session))
        .fallback(web_asset)
        .with_state(state)
}

/// Every failure a caller can see. The wording is deliberately thin: an
/// unauthenticated caller learns whether the bridge is up and nothing else.
pub enum ApiError {
    Unauthorized,
    BadRequest(String),
    UnknownServer(String),
    ConfirmationRequired { kind: String, strictness: &'static str },
    Upstream(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
    /// The `danger.*` i18n key, so the web client renders the same wording
    /// the desktop confirm dialog uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    /// `click` or `type_name`: how the caller must confirm to proceed.
    #[serde(skip_serializing_if = "Option::is_none")]
    strictness: Option<&'static str>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Every refusal is logged here, once, whichever handler produced it:
        // a browser that is told "401" or "400" leaves no other trace on this
        // side, and the first hours of the web build were spent inferring a
        // refusal from the absence of the log line that a success would have
        // written. `warn`, because a refused request is worth a look and a
        // wrong password on a busy deployment is not an error of this process.
        let (status, error, message, kind, strictness) = match &self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", String::new(), None, None),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone(), None, None),
            ApiError::UnknownServer(m) => (StatusCode::NOT_FOUND, "unknown_server", m.clone(), None, None),
            ApiError::ConfirmationRequired { kind, strictness } => (
                // 428: the request is fine, it just needs a precondition met.
                StatusCode::PRECONDITION_REQUIRED,
                "confirmation_required",
                "this command needs an explicit confirmation".to_string(),
                Some(kind.clone()),
                Some(*strictness),
            ),
            ApiError::Upstream(m) => (StatusCode::BAD_GATEWAY, "upstream", m.clone(), None, None),
        };
        tracing::warn!(status = status.as_u16(), error, message = %message, "request refused");
        (
            status,
            Json(ErrorBody {
                error,
                message,
                kind,
                strictness,
            }),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Who is calling: HTTP Basic from a script or the CLI, a login cookie from
/// a browser. The name is the answer rather than a yes, because the routes
/// below need it to decide which entries the caller sees.
fn authorize(state: &AppState, headers: &HeaderMap) -> ApiResult<String> {
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    if let Some(account) = state.accounts.account_for_header(authorization) {
        return Ok(account);
    }
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    auth::cookie_value(cookie)
        .and_then(|id| state.logins.account(id))
        .ok_or(ApiError::Unauthorized)
}

/// Whether `account` may see `server` — and so use, edit and delete it: its
/// own entries, and the shared ones, which are those with no owner. Entries
/// written before there were owners have none, so they stay everyone's.
fn visible_to(server: &RedisServer, account: &str) -> bool {
    server
        .owner
        .as_deref()
        .is_none_or(|owner| owner.is_empty() || owner == account)
}

/// The entry `id` names, if `account` may see it. An entry that exists but
/// is someone else's answers exactly like one that does not exist, so the
/// routes never confirm what another account has.
fn visible_server(id: &str, account: &str) -> ApiResult<RedisServer> {
    get_server(id)
        .ok()
        .filter(|server| visible_to(server, account))
        .ok_or_else(|| ApiError::UnknownServer(id.to_string()))
}

/// Settle who owns `server`: nobody (shared) or `account`. Never what the
/// request wrote in `owner` — a caller can say *private* or *shared* and
/// cannot say *whose*.
///
/// A request says it with `shared`, or with the entry's `owner` field:
/// `RedisServer::OWNER_SHARED` for shared, any other name for private. One
/// that says **neither** — an import, a `redis://` link, a script's bare
/// `{server}` — did not come past the form's checkbox, and gets the safe
/// answer: a new entry is private, because it carries credentials, and an
/// edited one (`stored`) stays whatever it was.
fn assign_owner(server: &mut RedisServer, account: &str, shared: Option<bool>, stored: Option<&RedisServer>) {
    let said = shared.or_else(|| match server.owner.as_deref() {
        None | Some("") => None,
        Some(owner) => Some(owner == RedisServer::OWNER_SHARED),
    });
    let shared = said.unwrap_or_else(|| stored.is_some_and(|stored| stored.owner.as_deref().is_none_or(str::is_empty)));
    server.owner = (!shared).then(|| account.to_string());
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginRequest {
    username: String,
    password: String,
    /// "Keep me signed in on this device": thirty idle days, not a working one.
    remember: bool,
}

/// Trade a username and password for a cookie.
///
/// This is the only place a password is accepted in a body, and the only way
/// a browser gets in: afterwards the page holds nothing a script can read.
async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> ApiResult<Response> {
    if !state.accounts.accepts_password(&req.username, &req.password) {
        return Err(ApiError::Unauthorized);
    }
    let account = req.username;
    let id = state
        .logins
        .open(&account, &state.accounts, req.remember)
        .ok_or(ApiError::Unauthorized)?;
    tracing::info!(account, remember = req.remember, "login");
    Ok((
        [(
            "set-cookie",
            auth::set_cookie(&id, state.secure_cookie, auth::idle_timeout(req.remember)),
        )],
        Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response())
}

/// Drop this browser's login. Idempotent, and it always clears the cookie so a
/// stale one cannot linger after the server forgot it.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    if let Some(account) = auth::cookie_value(cookie).and_then(|id| state.logins.close(id)) {
        tracing::info!(account, "logout");
    }
    (
        [("set-cookie", auth::clear_cookie(state.secure_cookie))],
        StatusCode::NO_CONTENT,
    )
        .into_response()
}

/// A file of the web build as it is about to be sent.
struct StoredFile {
    /// Borrowed when it is embedded: the binary's own read-only pages.
    bytes: Cow<'static, [u8]>,
    /// Its SHA-256 where one is already known (rust-embed computes it at
    /// build time); `None` for a file off disk or an inflated body, which
    /// are hashed when the `ETag` is made.
    hash: Option<[u8; 32]>,
}

/// Anything that is not an API route is a file of the web build: the one
/// compiled into this binary, or — with `--static <dir>` — a directory on
/// disk instead.
///
/// Unauthenticated on purpose: the page has to load before anyone can log in,
/// and it is a client application, not data. Everything it then asks for is
/// behind the cookie.
///
/// Two things keep the 20 MB module from being 20 MB on the wire. It is sent
/// in the best coding the caller accepts that is stored beside it (`.br`,
/// `.gz` — see `static_files::negotiate`), which is a quarter of the bytes.
/// And every answer carries an `ETag`: `Cache-Control: no-cache` means
/// "revalidate", so that a rebuilt bundle is picked up on the next load, and
/// a validator is what lets that revalidation be a `304` with no body rather
/// than the whole module again on every reload.
async fn web_asset(State(state): State<AppState>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "not found").into_response();
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let Some(key) = static_files::request_key(uri.path()) else {
        return not_found();
    };
    let accept_encoding = header("accept-encoding").unwrap_or_default();

    // The stored file a key names, and its hash when one is already known.
    let root = state.web_root.as_deref();
    let representation = match root {
        Some(root) => static_files::negotiate(&key, accept_encoding, |k| static_files::resolve(root, k).is_some()),
        None => static_files::negotiate(&key, accept_encoding, static_files::embedded_has),
    };
    let Some(representation) = representation else {
        return not_found();
    };
    let (stored_key, encoding, inflate) = match &representation {
        Representation::Stored { key, encoding } => (key, *encoding, false),
        Representation::Inflated { key } => (key, None, true),
    };
    let stored = match root {
        Some(root) => match static_files::resolve(root, stored_key) {
            Some(path) => tokio::fs::read(&path).await.ok().map(|bytes| StoredFile {
                bytes: Cow::Owned(bytes),
                hash: None,
            }),
            None => None,
        },
        None => WebBuild::get(stored_key).map(|file| StoredFile {
            hash: Some(file.metadata.sha256_hash()),
            bytes: file.data,
        }),
    };
    let Some(stored) = stored else {
        return not_found();
    };
    let StoredFile { bytes, hash } = if inflate {
        match static_files::inflate(&stored.bytes) {
            Ok(raw) => StoredFile {
                bytes: Cow::Owned(raw),
                hash: None,
            },
            Err(e) => {
                tracing::error!(error = %e, key = stored_key.as_str(), "a stored gzip could not be inflated");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    } else {
        stored
    };
    let etag = hash
        .map(static_files::etag)
        .unwrap_or_else(|| static_files::etag_of(&bytes));

    let mut response = Response::builder()
        .header("etag", &etag)
        .header("cache-control", "no-cache")
        .header("vary", "Accept-Encoding");
    if static_files::matches_etag(header("if-none-match"), &etag) {
        return response
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    // The type is the requested file's, not the `.gz` beside it: the coding
    // is a property of the transfer, which is what `Content-Encoding` says.
    response = response.header("content-type", static_files::content_type(FsPath::new(&key)));
    if let Some(encoding) = encoding {
        response = response.header("content-encoding", encoding);
    }
    let body = match bytes {
        // Borrowed is the release case: the bytes are the binary's own
        // read-only pages, and `from_static` sends them without a copy.
        Cow::Borrowed(bytes) => Body::from(Bytes::from_static(bytes)),
        Cow::Owned(bytes) => Body::from(bytes),
    };
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Unauthenticated, and says only that the bridge is up.
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// One entry as a caller sees it: every setting, no secret.
#[derive(Serialize)]
struct ServerEntry {
    /// The entry with its secrets taken out (`RedisServer::take_secrets`).
    #[serde(flatten)]
    server: RedisServer,
    /// Which secrets hold a value here — enough for a form to show that a
    /// password is stored, and for an edit to ask that it be kept.
    secrets_set: Vec<&'static str>,
}

impl ServerEntry {
    fn new(mut server: RedisServer) -> Self {
        let secrets_set = server.take_secrets();
        Self { server, secrets_set }
    }
}

/// The server list: each entry's settings, and which of its secrets are set.
///
/// The settings are here because an entry that cannot be read cannot be
/// edited: the first version answered ids and names only, the page's form
/// therefore opened with a name and nothing else, and saving it changed
/// nothing. Passwords, keys and passphrases still never leave this process —
/// a caller learns that one is stored, not what it is (ADR 9).
///
/// Only the entries the caller may see: its own, and the shared ones. `owner`
/// comes back as stored — the caller's own name or nothing — which is how
/// the page knows whether its form's *shared* box starts ticked.
async fn servers(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<ServerEntry>>> {
    let account = authorize(&state, &headers)?;
    let list = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(Json(
        list.into_iter()
            .filter(|server| visible_to(server, &account))
            .map(ServerEntry::new)
            .collect(),
    ))
}

#[derive(Deserialize)]
struct AddServerRequest {
    /// A `redis://` or `rediss://` connection string, or any of the export
    /// formats `RedisServer::from_import` understands.
    #[serde(default)]
    url: Option<String>,
    /// The entry as the desktop's server form builds it — what the web build
    /// sends, since its form is the same form. Exactly one of `url` and
    /// `server` is given. An empty `id` is stamped here, so a browser never
    /// has to invent one.
    #[serde(default)]
    server: Option<RedisServer>,
    #[serde(default)]
    name: Option<String>,
    /// Whether every account sees the new entry. Left out, a `server` may say
    /// it with its `owner` field (the page's checkbox); said by neither, the
    /// entry is private.
    #[serde(default)]
    shared: Option<bool>,
}

#[derive(Serialize)]
struct AddServerResponse {
    id: String,
    name: String,
    /// What the server said about itself once the bridge reached it, so the
    /// page can show that the connection is real and not merely saved.
    version: String,
    server_type: String,
    databases: usize,
}

/// Save a server and immediately prove it reachable.
///
/// Saving without connecting would let a typo sit in the shared list looking
/// healthy, so the dial is part of the request: a server that cannot be
/// reached is not added.
async fn add_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddServerRequest>,
) -> ApiResult<Json<AddServerResponse>> {
    let account = authorize(&state, &headers)?;
    let (mut server, shared) = match (req.url, req.server) {
        (Some(url), None) => {
            let server = RedisServer::from_import(&url)
                .map_err(|e| ApiError::BadRequest(format!("could not read that connection string: {e:?}")))?;
            (server, req.shared)
        }
        (None, Some(server)) => (server, req.shared),
        _ => return Err(ApiError::BadRequest("give either `url` or `server`".to_string())),
    };
    // An id the caller cannot see is either free or someone else's, and the
    // two must look the same: a fresh id, never an overwrite and never a
    // refusal that would confirm the other entry exists.
    if server.id.trim().is_empty() || get_server(&server.id).is_ok_and(|taken| !visible_to(&taken, &account)) {
        server.id = Uuid::now_v7().to_string();
    }
    assign_owner(&mut server, &account, shared, None);
    if server.host.trim().is_empty() {
        return Err(ApiError::BadRequest("the server has no host".to_string()));
    }
    if let Some(name) = req.name.filter(|n| !n.trim().is_empty()) {
        server.name = name.trim().to_string();
    }
    if server.name.trim().is_empty() {
        server.name = format!("{}:{}", server.host, server.port);
    }
    save_and_dial(server, None).await.map(Json)
}

/// Put `server` into the shared list and prove it reachable, or put the list
/// back the way it was: `previous` is the entry it replaces (an edit), `None`
/// when it is new.
///
/// Saving without connecting would let a typo sit in the list looking
/// healthy, and for an edit it would do worse — break an entry that worked.
/// So the dial is part of the request either way, and a failure leaves no
/// trace in a list everyone shares.
async fn save_and_dial(server: RedisServer, previous: Option<RedisServer>) -> ApiResult<AddServerResponse> {
    let id = server.id.clone();
    let name = server.name.clone();
    // The pool is keyed by the entry's whole content, so the edited entry
    // gets a client of its own; this drops the one the old content had.
    if previous.is_some() {
        get_connection_manager().remove_client(&id, 0);
    }

    let mut servers = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    // In place, so an edit does not move the entry to the end of the list.
    match servers.iter_mut().find(|s| s.id == id) {
        Some(slot) => *slot = server,
        None => servers.push(server),
    }
    save_servers(servers)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let client = match get_connection_manager().get_client(&id, 0).await {
        Ok(client) => client,
        Err(e) => {
            if let Ok(mut list) = get_servers() {
                match previous {
                    Some(previous) => {
                        if let Some(slot) = list.iter_mut().find(|s| s.id == id) {
                            *slot = previous;
                        }
                    }
                    None => list.retain(|s| s.id != id),
                }
                let _ = save_servers(list).await;
            }
            return Err(ApiError::Upstream(e.to_string()));
        }
    };
    let description = client.nodes_description();
    Ok(AddServerResponse {
        id,
        name,
        version: client.version(),
        server_type: description.server_type,
        databases: client.databases(),
    })
}

#[derive(Deserialize)]
struct UpdateServerRequest {
    /// The entry as the form holds it after the edit.
    server: RedisServer,
    /// The secrets to leave as stored. A caller never saw them, so "the
    /// password field was not touched" cannot be said by sending it back;
    /// it is said by naming it here. A secret that is neither named nor
    /// given a value is cleared, which is how a password is removed.
    #[serde(default)]
    keep_secrets: Vec<String>,
    /// As for a new entry, except that an edit which says neither keeps the
    /// entry as it was. Making a shared entry private takes it for the caller.
    #[serde(default)]
    shared: Option<bool>,
}

/// The entry an edit produces: `incoming`'s settings under `stored`'s id,
/// with each secret in `keep` carried over from `stored`.
///
/// `keep` wins over a value sent for the same field: a caller that names a
/// secret is saying it does not know it.
fn merge_update(stored: &RedisServer, mut incoming: RedisServer, keep: &[String]) -> Result<RedisServer, String> {
    incoming.id = stored.id.clone();
    let mut stored = stored.clone();
    for name in keep {
        let (Some(kept), Some(slot)) = (stored.secret_mut(name).map(|s| s.take()), incoming.secret_mut(name)) else {
            return Err(format!("`{name}` is not a secret field"));
        };
        *slot = kept;
    }
    if incoming.host.trim().is_empty() {
        return Err("the server has no host".to_string());
    }
    if incoming.name.trim().is_empty() {
        incoming.name = format!("{}:{}", incoming.host, incoming.port);
    }
    Ok(incoming)
}

/// Edit an entry, and prove the edit reachable before it is kept.
async fn update_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateServerRequest>,
) -> ApiResult<Json<AddServerResponse>> {
    let account = authorize(&state, &headers)?;
    let stored = visible_server(&id, &account)?;
    let mut server = merge_update(&stored, req.server, &req.keep_secrets).map_err(ApiError::BadRequest)?;
    assign_owner(&mut server, &account, req.shared, Some(&stored));
    save_and_dial(server, Some(stored)).await.map(Json)
}

/// Drop an entry. Idempotent: an id that is already gone is not an error,
/// because the browser reconciles by diffing and may ask twice.
async fn delete_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let account = authorize(&state, &headers)?;
    let list = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    // Someone else's entry is left alone and answered like one already gone.
    let kept: Vec<_> = list
        .into_iter()
        .filter(|s| s.id != id || !visible_to(s, &account))
        .collect();
    save_servers(kept)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SessionRequest {
    server: String,
    #[serde(default)]
    db: usize,
}

#[derive(Serialize)]
struct SessionResponse {
    session: String,
}

async fn open_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SessionRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let account = authorize(&state, &headers)?;
    visible_server(&req.server, &account)?;
    let session = state
        .sessions
        .open(&req.server, req.db)
        .await
        .map_err(ApiError::Upstream)?;
    Ok(Json(SessionResponse { session }))
}

async fn close_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> ApiResult<StatusCode> {
    authorize(&state, &headers)?;
    state.sessions.close(&token).await;
    // Idempotent: closing a session that has already expired is not an error.
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PipelineSpecDto {
    offset: usize,
    count: usize,
    #[serde(default)]
    atomic: bool,
}

#[derive(Deserialize)]
struct ExecRequest {
    server: String,
    #[serde(default)]
    db: usize,
    /// The packed commands, base64 of what `Cmd::get_packed_command()` made.
    /// One entry is a plain command; several are a pipeline.
    commands: Vec<String>,
    /// Present only for a pipeline, carrying the framing redis-rs asks a
    /// connection for. `MULTI`/`EXEC` are added here from `atomic`, exactly
    /// as redis-rs adds them when it packs a transaction, so the caller's
    /// `offset` still counts the replies it expects to skip.
    #[serde(default)]
    pipeline: Option<PipelineSpecDto>,
    /// Run the commands on every master of the server instead of on one
    /// connection. The bridge owns the topology, so it does the fan-out and
    /// the caller never holds a dialable node address.
    #[serde(default)]
    fanout: Option<String>,
    /// Which node each command is for, as a `host:port` label. Empty means
    /// every master, padding with the first command. When set, `commands[i]`
    /// is for `fanout_nodes[i]` and unlisted nodes are not asked.
    ///
    /// A label may repeat: the commands carrying it run as one pipeline on
    /// that node, in the order given. That is what the key tree's `TYPE` /
    /// `TTL` round needs, and it cannot be done as a single pipeline over one
    /// connection — redis-rs refuses a cluster pipeline whose commands span
    /// slots, which a page of scanned keys always does.
    #[serde(default)]
    fanout_nodes: Vec<String>,
    /// Pin the commands to a session's own connection.
    #[serde(default)]
    session: Option<String>,
    /// What the caller sends after being refused once.
    #[serde(default)]
    confirm: Option<String>,
}

#[derive(Serialize)]
struct ExecResponse {
    /// One base64 RESP frame per reply, for `parse_redis_value`.
    replies: Vec<String>,
    /// For a fan-out, the `host:port` of the node behind each reply. A label
    /// for the caller to print; the connection details stay here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    nodes: Vec<String>,
}

/// Which commands each master is being asked for, by label.
///
/// One entry per master, holding the *indexes* of the commands aimed at it in
/// the order the caller sent them — order is load-bearing, because the caller
/// puts the replies back together by walking its own batches. A label that
/// names no master this process knows is dropped rather than guessed at: the
/// two sides discover the topology separately, and a reply attributed to the
/// wrong node would resume a scan from another node's cursor (ADR 9).
fn aim_at_nodes(master_labels: &[String], wanted: &[String]) -> Vec<Vec<usize>> {
    let mut aimed: Vec<Vec<usize>> = master_labels.iter().map(|_| Vec::new()).collect();
    for (index, label) in wanted.iter().enumerate() {
        if let Some(master) = master_labels.iter().position(|m| m == label) {
            aimed[master].push(index);
        }
    }
    aimed
}

async fn exec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<ExecResponse>> {
    let account = authorize(&state, &headers)?;
    if req.commands.is_empty() {
        return Err(ApiError::BadRequest("no commands".to_string()));
    }

    let mut decoded = Vec::with_capacity(req.commands.len());
    for (index, frame) in req.commands.iter().enumerate() {
        let bytes = B64
            .decode(frame.as_bytes())
            .map_err(|e| ApiError::BadRequest(format!("command {index} is not base64: {e}")))?;
        decoded.push(resp::decode_command(&bytes).map_err(|e| ApiError::BadRequest(format!("command {index}: {e}")))?);
    }

    let server = visible_server(&req.server, &account)?;
    // Every command in a batch is judged. Gating only the first would let a
    // pipeline smuggle a FLUSHALL in behind a GET.
    for args in &decoded {
        if let policy::Verdict::Confirm { kind, strictness } = policy::check(&server, args, req.confirm.as_deref()) {
            return Err(ApiError::ConfirmationRequired {
                kind: kind.i18n_key().to_string(),
                strictness: match strictness {
                    zedis_connection::ConfirmStrictness::Click => "click",
                    zedis_connection::ConfirmStrictness::TypeName => "type_name",
                },
            });
        }
    }

    // Fan-out is its own path: it needs the client, not a connection, because
    // reaching every master is topology knowledge this process owns.
    if let Some(mode) = &req.fanout {
        if mode != "masters" {
            return Err(ApiError::BadRequest(format!("unknown fanout \"{mode}\"")));
        }
        if req.pipeline.is_some() {
            return Err(ApiError::BadRequest("a fan-out cannot also be a pipeline".to_string()));
        }
        let client = get_connection_manager()
            .get_client(&req.server, req.db)
            .await
            .map_err(|e| ApiError::Upstream(e.to_string()))?;
        let (servers, values): (Vec<zedis_connection::RedisServer>, Vec<redis::Value>) = if req.fanout_nodes.is_empty()
        {
            let cmds: Vec<redis::Cmd> = decoded.iter().map(|args| resp::command_from_args(args)).collect();
            // Reuses the desktop's own fan-out, so the padding rule that
            // repeats the first command across the remaining masters is
            // identical.
            client
                .query_async_masters(cmds)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?
        } else {
            if req.fanout_nodes.len() != decoded.len() {
                return Err(ApiError::BadRequest(
                    "fanout_nodes and commands must be the same length".to_string(),
                ));
            }
            // Re-aim the caller's labels onto *this* process's master list.
            // A label that is not here is dropped rather than guessed at: the
            // reply set says which nodes actually answered, and the caller
            // re-aligns on that.
            //
            // Everything goes out as a pipeline, including a batch of one —
            // redis-rs sends the same bytes either way, and one path is one
            // thing to get right.
            let masters = client.master_servers();
            let master_labels: Vec<String> = masters.iter().map(|s| format!("{}:{}", s.host, s.port)).collect();
            let aimed: Vec<Option<redis::Pipeline>> = aim_at_nodes(&master_labels, &req.fanout_nodes)
                .into_iter()
                .map(|indexes| {
                    (!indexes.is_empty()).then(|| {
                        let mut pipe = redis::pipe();
                        for index in indexes {
                            pipe.add_command(resp::command_from_args(&decoded[index]));
                        }
                        pipe
                    })
                })
                .collect();
            let replies: Vec<Option<Vec<redis::Value>>> = client
                .query_async_masters_pipelines(aimed)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            // One label per reply frame, so a caller that sent several
            // commands to one node can put the answers back together.
            let mut answered = Vec::new();
            let mut values = Vec::new();
            for (server, reply) in masters.into_iter().zip(replies) {
                let Some(batch) = reply else { continue };
                for value in batch {
                    answered.push(server.clone());
                    values.push(value);
                }
            }
            (answered, values)
        };
        let replies = values
            .iter()
            .map(|value| {
                resp::encode_to_vec(value)
                    .map(|bytes| B64.encode(bytes))
                    .map_err(|e| ApiError::Upstream(e.to_string()))
            })
            .collect::<ApiResult<Vec<String>>>()?;
        let nodes = servers.iter().map(|s| format!("{}:{}", s.host, s.port)).collect();
        return Ok(Json(ExecResponse { replies, nodes }));
    }

    let mut conn = match &req.session {
        Some(token) => state
            .sessions
            .take(token, &req.server, req.db)
            .await
            .ok_or_else(|| ApiError::BadRequest("unknown or mismatched session".to_string()))?,
        None => get_connection_manager()
            .get_connection(&req.server, req.db)
            .await
            .map_err(|e| ApiError::Upstream(e.to_string()))?,
    };

    let values = match &req.pipeline {
        None => {
            let cmd = resp::command_from_args(&decoded[0]);
            vec![
                conn.req_packed_command(&cmd)
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))?,
            ]
        }
        Some(spec) => {
            let mut pipe = redis::Pipeline::with_capacity(decoded.len());
            if spec.atomic {
                pipe.atomic();
            }
            for args in &decoded {
                pipe.add_command(resp::command_from_args(args));
            }
            conn.req_packed_commands(&pipe, spec.offset, spec.count)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?
        }
    };

    let replies = values
        .iter()
        .map(|value| {
            resp::encode_to_vec(value)
                .map(|bytes| B64.encode(bytes))
                .map_err(|e| ApiError::Upstream(e.to_string()))
        })
        .collect::<ApiResult<Vec<String>>>()?;
    Ok(Json(ExecResponse {
        replies,
        nodes: Vec::new(),
    }))
}

#[cfg(test)]
mod server_tests {
    use super::{RedisServer, ServerEntry, assign_owner, merge_update, visible_to};

    fn owned_by(owner: Option<&str>) -> RedisServer {
        RedisServer {
            owner: owner.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn an_account_sees_its_own_entries_and_the_shared_ones() {
        assert!(visible_to(&owned_by(Some("alice")), "alice"));
        assert!(
            !visible_to(&owned_by(Some("alice")), "bob"),
            "a private entry is its owner's alone"
        );
        assert!(visible_to(&owned_by(None), "bob"), "no owner: shared");
        assert!(visible_to(&owned_by(Some("")), "bob"), "an empty owner is no owner");
        // Names are compared whole: a prefix of someone's name is someone else.
        assert!(!visible_to(&owned_by(Some("alice")), "alic"));
    }

    #[test]
    fn the_owner_is_the_caller_or_nobody_never_what_the_request_wrote() {
        // The page's unticked box: private, to whoever is signed in.
        let mut server = owned_by(Some(RedisServer::OWNER_SELF));
        assign_owner(&mut server, "alice", None, None);
        assert_eq!(server.owner.as_deref(), Some("alice"));

        // A caller cannot hand an entry to someone else, or pose as them.
        let mut server = owned_by(Some("bob"));
        assign_owner(&mut server, "alice", None, None);
        assert_eq!(server.owner.as_deref(), Some("alice"));

        // The ticked box. The marker is never what gets stored.
        let mut server = owned_by(Some(RedisServer::OWNER_SHARED));
        assign_owner(&mut server, "alice", None, None);
        assert_eq!(server.owner, None);

        // `shared` in the request settles it either way, over the field.
        let mut server = owned_by(Some(RedisServer::OWNER_SHARED));
        assign_owner(&mut server, "alice", Some(false), None);
        assert_eq!(server.owner.as_deref(), Some("alice"));
        let mut server = owned_by(Some("alice"));
        assign_owner(&mut server, "alice", Some(true), None);
        assert_eq!(server.owner, None);
    }

    #[test]
    fn an_entry_that_says_nothing_is_private_when_new_and_unchanged_when_edited() {
        // An import, a redis:// link, a script's bare {server}: no checkbox
        // was passed on the way. Reading that as "shared" is how an imported
        // entry, credentials and all, reached every account.
        for unsaid in [None, Some("")] {
            let mut server = owned_by(unsaid);
            assign_owner(&mut server, "alice", None, None);
            assert_eq!(server.owner.as_deref(), Some("alice"), "new: private");
        }

        // An edit that does not say keeps what is stored — a reorder or a tag
        // change must neither publish a private entry nor privatise a shared one.
        let mut server = owned_by(None);
        assign_owner(&mut server, "alice", None, Some(&owned_by(None)));
        assert_eq!(server.owner, None, "was shared, stays shared");
        let mut server = owned_by(None);
        assign_owner(&mut server, "alice", None, Some(&owned_by(Some("alice"))));
        assert_eq!(server.owner.as_deref(), Some("alice"), "was private, stays private");
    }

    fn stored() -> RedisServer {
        RedisServer {
            id: "srv-1".to_string(),
            name: "prod".to_string(),
            host: "10.0.0.5".to_string(),
            port: 6379,
            username: Some("app".to_string()),
            password: Some("hunter2".to_string()),
            ssh_key: Some("-----BEGIN KEY-----".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_listed_entry_carries_its_settings_and_no_secret() {
        let json = serde_json::to_value(ServerEntry::new(stored())).expect("json");
        assert_eq!(json["host"], "10.0.0.5");
        assert_eq!(json["port"], 6379);
        assert_eq!(json["username"], "app");
        assert_eq!(json["secrets_set"], serde_json::json!(["password", "ssh_key"]));
        let text = json.to_string();
        assert!(!text.contains("hunter2") && !text.contains("BEGIN KEY"), "{text}");
    }

    #[test]
    fn an_edit_keeps_the_secrets_it_names_and_clears_the_ones_it_does_not() {
        let edit = RedisServer {
            id: "whatever-the-caller-sent".to_string(),
            name: "prod-renamed".to_string(),
            host: "10.0.0.6".to_string(),
            port: 6380,
            ..Default::default()
        };
        let merged = merge_update(&stored(), edit, &["password".to_string()]).expect("merge");
        assert_eq!(merged.id, "srv-1", "the path names the entry, not the body");
        assert_eq!(
            (merged.name.as_str(), merged.host.as_str(), merged.port),
            ("prod-renamed", "10.0.0.6", 6380)
        );
        assert_eq!(merged.password.as_deref(), Some("hunter2"), "named: kept as stored");
        assert_eq!(merged.ssh_key, None, "neither named nor given: cleared");
        assert_eq!(merged.username, None, "a setting is simply what the edit says");
    }

    #[test]
    fn an_edit_may_replace_a_secret_and_keep_wins_over_a_value_sent_with_it() {
        let edit = RedisServer {
            host: "h".to_string(),
            password: Some("new-password".to_string()),
            ssh_key: Some("not-the-real-key".to_string()),
            ..Default::default()
        };
        let merged = merge_update(&stored(), edit, &["ssh_key".to_string()]).expect("merge");
        assert_eq!(merged.password.as_deref(), Some("new-password"));
        assert_eq!(merged.ssh_key.as_deref(), Some("-----BEGIN KEY-----"));
        assert_eq!(merged.name, "h:0", "an empty name falls back to the address");
    }

    #[test]
    fn an_edit_is_refused_for_a_name_that_is_not_a_secret_or_a_missing_host() {
        let edit = RedisServer {
            host: "h".to_string(),
            ..Default::default()
        };
        let err = merge_update(&stored(), edit, &["host".to_string()]).expect_err("host is not a secret");
        assert!(err.contains("`host`"), "{err}");
        let err = merge_update(&stored(), RedisServer::default(), &[]).expect_err("no host");
        assert!(err.contains("no host"), "{err}");
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::aim_at_nodes;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_command_per_node_is_the_ordinary_case() {
        let masters = labels(&["a:1", "b:2", "c:3"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "b:2", "c:3"])),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn a_repeated_label_becomes_one_batch_in_the_order_it_was_sent() {
        // What the key tree's TYPE round needs: every key a node returned
        // from SCAN goes back to that node, together.
        let masters = labels(&["a:1", "b:2"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "b:2", "a:1", "a:1"])),
            vec![vec![0, 2, 3], vec![1]],
            "order within a batch is what lets the caller re-pair replies with keys"
        );
    }

    #[test]
    fn a_master_nobody_asked_for_gets_nothing() {
        let masters = labels(&["a:1", "b:2", "c:3"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["c:3", "c:3"])),
            vec![vec![], vec![], vec![0, 1]]
        );
    }

    #[test]
    fn a_label_this_process_does_not_know_is_dropped_not_guessed_at() {
        // The caller's topology moved between its discovery and ours. The
        // reply set says which nodes answered; inventing a home for this one
        // would resume a scan from the wrong cursor.
        let masters = labels(&["a:1", "b:2"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "gone:9", "b:2"])),
            vec![vec![0], vec![2]]
        );
    }

    #[test]
    fn no_masters_means_nothing_to_aim_at() {
        assert!(aim_at_nodes(&[], &labels(&["a:1"])).is_empty());
    }
}
