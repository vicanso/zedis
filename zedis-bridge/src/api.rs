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

use crate::audit::{self, Audit, Event, Origin, ServerRef};
use crate::static_files::{Representation, WebBuild};
use crate::{auth, mcp, policy, resp, session::Sessions, static_files};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use redis::aio::ConnectionLike;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::net::SocketAddr;
use std::path::Path as FsPath;
use uuid::Uuid;
use zedis_connection::{RedisServer, get_connection_manager, get_server, get_servers, save_servers};

#[derive(Clone)]
pub struct AppState {
    pub accounts: auth::Accounts,
    pub sessions: Sessions,
    pub logins: auth::Logins,
    /// `--audit-log`: the record of what went through, or nothing.
    pub audit: Audit,
    /// `--trusted-header` + `--trusted-proxy`: identities a reverse proxy
    /// asserts, or nothing — then the header is never read.
    pub trusted: Option<auth::TrustedProxy>,
    /// The open write windows on locked entries, per account (ADR 14).
    pub unlocks: policy::Unlocks,
    /// The login cookie's `Secure` and `Path`, fixed at startup.
    pub cookie: auth::CookiePolicy,
    /// `--base-path`: where the bridge is mounted, as [`normalize_base_path`]
    /// leaves it — empty for the root, else `/prefix`.
    pub base_path: String,
    /// `--static <dir>`: serve the web build from this directory instead of
    /// the one compiled in.
    pub web_root: Option<std::path::PathBuf>,
    /// How often each account may call an MCP tool (ADR 15).
    pub mcp_calls: mcp::Limiter,
}

/// The page and the API, under `state.base_path` when there is one.
///
/// A bridge that shares a host name with other applications gets a path of
/// its own there (`https://tools.example.com/zedis/`). Everything moves
/// under it — the page, its files and `/v1/…` alike — and nothing outside
/// it answers, so the reverse proxy in front forwards the prefix as it is
/// and the neighbours' paths stay theirs. The page needs no telling: it
/// addresses everything relative to where it was loaded from.
///
/// The routes are registered at their full paths rather than through
/// `Router::nest`: axum 0.8's nest hands `/zedis` to the inner router but
/// not `/zedis/`, which is the one address the page lives at.
pub fn router(state: AppState) -> Router {
    let at = |path: &str| format!("{}{path}", state.base_path);
    Router::new()
        .route(&at("/v1/health"), get(health))
        .route(&at("/v1/login"), post(login))
        .route(&at("/v1/logout"), post(logout))
        .route(&at("/v1/servers"), get(servers).post(add_server))
        .route(&at("/v1/servers/{id}"), delete(delete_server).put(update_server))
        .route(&at("/v1/servers/{id}/unlock"), post(unlock_writes).delete(lock_writes))
        .route(&at("/v1/exec"), post(exec))
        .route(&at("/v1/session"), post(open_session))
        .route(&at("/v1/session/{token}"), delete(close_session))
        // The MCP entry point (ADR 15): JSON-RPC over POST alone. GET is
        // the server-initiated stream this bridge does not offer, DELETE a
        // session it does not keep; both answer 405 as the protocol allows.
        .route(
            &at("/v1/mcp"),
            post(mcp::post).get(mcp::not_allowed).delete(mcp::not_allowed),
        )
        .fallback(web_asset)
        .with_state(state)
}

/// What a request path names below `base_path`.
#[derive(Debug, PartialEq, Eq)]
enum Mounted<'a> {
    /// A path of the web build, prefix removed (`/`, `/wasm/zedis_web.js`).
    File(&'a str),
    /// The base path without its trailing slash. Only `/zedis/` is a
    /// directory to the browser: from `/zedis` the page's `./wasm/…` and
    /// `v1/…` would resolve against the site root — the neighbours' paths.
    NeedsSlash,
    /// Not under the base path: someone else's, and not confirmed to exist.
    Outside,
}

fn mounted<'a>(base_path: &str, request_path: &'a str) -> Mounted<'a> {
    if base_path.is_empty() {
        return Mounted::File(request_path);
    }
    match request_path.strip_prefix(base_path) {
        Some("") => Mounted::NeedsSlash,
        // `/zedisx` also starts with `/zedis`: the rest has to be a path.
        Some(rest) if rest.starts_with('/') => Mounted::File(rest),
        _ => Mounted::Outside,
    }
}

/// `--base-path` / `ZEDIS_BRIDGE_BASE_PATH` as the router and the cookie
/// want it: empty for the root (`""`, `/`), else `/prefix` with a leading
/// slash and no trailing one. Refused rather than repaired when it could
/// not be a plain path: it ends up in a route pattern and in a `Set-Cookie`
/// header, where `{`, `;` or a space would each mean something else.
pub fn normalize_base_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~');
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || !segment.chars().all(plain) {
            return Err(format!(
                "base path {raw:?}: each segment may hold letters, digits and - _ . ~ only"
            ));
        }
    }
    Ok(format!("/{trimmed}"))
}

/// Every failure a caller can see. The wording is deliberately thin: an
/// unauthenticated caller learns whether the bridge is up and nothing else.
pub enum ApiError {
    Unauthorized,
    /// Signed in, and not allowed to do this: a read-only account asked to
    /// change something. Distinct from [`ApiError::Unauthorized`] on purpose
    /// — re-authenticating would not help, and a page that retried the login
    /// on a 401 would loop.
    Forbidden(String),
    BadRequest(String),
    UnknownServer(String),
    ConfirmationRequired {
        kind: String,
        strictness: &'static str,
    },
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

impl ApiError {
    /// The status, the machine-readable `error` and the message the caller
    /// is sent — and the confirmation details, for the one error that has
    /// them.
    fn parts(&self) -> (StatusCode, &'static str, String, Option<String>, Option<&'static str>) {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", String::new(), None, None),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m.clone(), None, None),
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
        }
    }

    /// What the audit log writes in `error`: the `error` word, and the
    /// message when there is one.
    pub(crate) fn describe(&self) -> String {
        let (_, error, message, _, _) = self.parts();
        if message.is_empty() {
            error.to_string()
        } else {
            format!("{error}: {message}")
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Every refusal is logged here, once, whichever handler produced it:
        // a browser that is told "401" or "400" leaves no other trace on this
        // side, and the first hours of the web build were spent inferring a
        // refusal from the absence of the log line that a success would have
        // written. `warn`, because a refused request is worth a look and a
        // wrong password on a busy deployment is not an error of this process.
        let (status, error, message, kind, strictness) = self.parts();
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

/// Who is calling: the identity the reverse proxy asserted (when there is
/// one, and the request came from it), HTTP Basic from a script or the CLI,
/// a login cookie from a browser. The name is the answer rather than a yes,
/// because the routes below need it to decide which entries the caller sees.
///
/// A proxy identity has to be an account here — the role and the entries
/// hang on the name — and one that is not is refused outright rather than
/// falling through to the password paths: the proxy already said who this
/// is, and a login form behind single sign-on would be the wrong answer.
pub(crate) fn authorize(state: &AppState, headers: &HeaderMap, origin: &mut Origin) -> ApiResult<String> {
    if let (Some(proxy), Some(peer)) = (&state.trusted, origin.peer_ip)
        && let Some(name) = proxy.asserted(peer, headers)
    {
        if !state.accounts.has(&name) {
            let tried: String = name.chars().take(MAX_TRIED_NAME).collect();
            state.audit.record(&tried, origin, Event::NoAccount);
            return Err(ApiError::Forbidden(format!(
                "the proxy signed in \"{tried}\", which has no account here"
            )));
        }
        origin.auth = Some("proxy");
        return Ok(name);
    }
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    if let Some(account) = state.accounts.account_for_header(authorization) {
        return Ok(account);
    }
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    auth::cookie_value(cookie)
        .and_then(|id| state.logins.account(id))
        .ok_or(ApiError::Unauthorized)
}

/// What a read-only account is told, wherever it is refused. One wording,
/// because the page shows it verbatim and the CLI prints it.
pub(crate) const READ_ONLY_REFUSAL: &str = "this account is read-only";

/// The longest name a failed login, or an unknown proxy identity, is
/// recorded under: it is whatever was typed into the box or written into
/// the header, and the log is not the place for a pasted essay.
const MAX_TRIED_NAME: usize = 64;

/// Who is calling, for a route that *changes* something — the server list,
/// or Redis itself. Same check as [`authorize`], plus the account's role.
///
/// Every mutating route goes through this one rather than through
/// [`authorize`] with a flag test bolted on, so adding a route and
/// forgetting the test is a compile error's worth of obvious: the route
/// either asks this function or it does not change anything. `action` is
/// what the audit log says was refused.
fn authorize_write(
    state: &AppState,
    headers: &HeaderMap,
    origin: &mut Origin,
    action: &'static str,
) -> ApiResult<String> {
    let account = authorize(state, headers, origin)?;
    if state.accounts.is_read_only(&account) {
        state.audit.record(&account, origin, Event::Refused { action });
        return Err(ApiError::Forbidden(READ_ONLY_REFUSAL.to_string()));
    }
    Ok(account)
}

/// Whether `account` may see `server` — and so use it, and edit or delete
/// it unless its rules say read-only there: its own entries, and the shared
/// ones (those with no owner; entries written before there were owners have
/// none, so they stay everyone's) that the account's `servers` rules admit,
/// which without rules is all of them.
pub(crate) fn visible_to(accounts: &auth::Accounts, server: &RedisServer, account: &str) -> bool {
    match server.owner.as_deref() {
        None | Some("") => accounts.may_see_shared(account, server),
        Some(owner) => owner == account,
    }
}

/// The entry `id` names, if `account` may see it. An entry that exists but
/// is someone else's answers exactly like one that does not exist, so the
/// routes never confirm what another account has.
fn visible_server(accounts: &auth::Accounts, id: &str, account: &str) -> ApiResult<RedisServer> {
    get_server(id)
        .ok()
        .filter(|server| visible_to(accounts, server, account))
        .ok_or_else(|| ApiError::UnknownServer(id.to_string()))
}

/// Refuse a change to `server` by an account that may not write there —
/// read-only outright, or read-only on this entry by its rules — the way
/// `authorize_write` refuses one that may write nowhere.
fn refuse_read_only_on(
    state: &AppState,
    account: &str,
    server: &RedisServer,
    origin: &Origin,
    action: &'static str,
) -> ApiResult<()> {
    if state.accounts.is_read_only_on(account, server) {
        state.audit.record(account, origin, Event::Refused { action });
        return Err(ApiError::Forbidden(READ_ONLY_REFUSAL.to_string()));
    }
    Ok(())
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
async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Response> {
    let origin = Origin::new(peer, &headers);
    if !state.accounts.accepts_password(&req.username, &req.password) {
        let tried: String = req.username.trim().chars().take(MAX_TRIED_NAME).collect();
        state.audit.record(&tried, &origin, Event::LoginFailed);
        return Err(ApiError::Unauthorized);
    }
    let account = req.username;
    let id = state
        .logins
        .open(&account, &state.accounts, req.remember)
        .ok_or(ApiError::Unauthorized)?;
    tracing::info!(account, remember = req.remember, "login");
    state
        .audit
        .record(&account, &origin, Event::Login { remember: req.remember });
    Ok((
        [("set-cookie", state.cookie.set(&id, auth::idle_timeout(req.remember)))],
        Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response())
}

/// Drop this browser's login. Idempotent, and it always clears the cookie so a
/// stale one cannot linger after the server forgot it.
async fn logout(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    if let Some(account) = auth::cookie_value(cookie).and_then(|id| state.logins.close(id)) {
        tracing::info!(account, "logout");
        state
            .audit
            .record(&account, &Origin::new(peer, &headers), Event::Logout);
    }
    ([("set-cookie", state.cookie.clear())], StatusCode::NO_CONTENT).into_response()
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
/// And every answer carries an `ETag`. The page stays `no-cache` (it carries
/// the content hashes) so a new bundle is noticed; a `?v=<md5>` URL is
/// immutable for a year. The validator is what lets a `no-cache` revalidation
/// be a `304` with no body rather than the whole module again.
async fn web_asset(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "not found").into_response();
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let file_path = match mounted(&state.base_path, uri.path()) {
        Mounted::File(path) => path,
        Mounted::NeedsSlash => {
            let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
            return Redirect::temporary(&format!("{}/{query}", state.base_path)).into_response();
        }
        Mounted::Outside => return not_found(),
    };
    let Some(key) = static_files::request_key(file_path) else {
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
        .header("cache-control", static_files::cache_control(uri.query()))
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
    /// Whether the *account* asking is read-only, as opposed to the entry
    /// being marked read-only — which `readonly` says, and which the page
    /// lets the user switch off. The page cannot switch this one off.
    ///
    /// A fact about the caller, repeated on every entry, because the list is
    /// a bare array and a caller reads it one entry at a time. An account
    /// with no entries needs no answer: there is nothing to grey out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    account_read_only: bool,
}

impl ServerEntry {
    fn new(mut server: RedisServer, account_read_only: bool) -> Self {
        let secrets_set = server.take_secrets();
        Self {
            server,
            secrets_set,
            account_read_only,
        }
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
/// Only the entries the caller may see: its own, and the shared ones its
/// rules admit. `owner` comes back as stored — the caller's own name or
/// nothing — which is how the page knows whether its form's *shared* box
/// starts ticked.
async fn servers(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<ServerEntry>>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    // An entry the account may not write to is marked read-only, which is
    // the signal the page already understands (`RedisServer::readonly` —
    // the desktop's own "safe mode" switch): every entry for a read-only
    // account, the `:ro` ones for an account with rules. The refusal does
    // not depend on it: the bridge refuses the write whatever the page
    // believes. This is so that the buttons are grey *before* the round
    // trip rather than after it.
    let list = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(Json(
        list.into_iter()
            .filter(|server| visible_to(&state.accounts, server, &account))
            .map(|mut server| {
                let read_only = state.accounts.is_read_only_on(&account, &server);
                if read_only {
                    server.readonly = Some(true);
                }
                ServerEntry::new(server, read_only)
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct AddServerRequest {
    /// A `redis(s)://` or `valkey(s)://` connection string, or any of the export
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AddServerRequest>,
) -> ApiResult<Json<AddServerResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "server_add")?;
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
    if server.id.trim().is_empty()
        || get_server(&server.id).is_ok_and(|taken| !visible_to(&state.accounts, &taken, &account))
    {
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
    // The facts before the entry is moved; the line after the outcome is
    // known, since an entry that could not be dialled is not added.
    let (reference, shared, settings) = (
        ServerRef::from(&server),
        audit::is_shared(&server),
        audit::settings(&server),
    );
    let result = save_and_dial(server, None).await;
    state.audit.record(
        &account,
        &origin,
        Event::ServerAdded {
            server: reference,
            shared,
            settings,
            error: result.as_ref().err().map(ApiError::describe),
        },
    );
    result.map(Json)
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateServerRequest>,
) -> ApiResult<Json<AddServerResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "server_update")?;
    let stored = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &stored, &origin, "server_update")?;
    let mut server = merge_update(&stored, req.server, &req.keep_secrets).map_err(ApiError::BadRequest)?;
    assign_owner(&mut server, &account, req.shared, Some(&stored));
    let reference = ServerRef::from(&server);
    let changed = audit::changed(&stored, &server);
    let secrets_changed = audit::secrets_changed(&stored, &server);
    let (was_shared, is_shared) = (audit::is_shared(&stored), audit::is_shared(&server));
    let result = save_and_dial(server, Some(stored)).await;
    state.audit.record(
        &account,
        &origin,
        Event::ServerUpdated {
            server: reference,
            changed,
            secrets_changed,
            shared: (was_shared != is_shared).then(|| audit::Change {
                from: was_shared.into(),
                to: is_shared.into(),
            }),
            error: result.as_ref().err().map(ApiError::describe),
        },
    );
    result.map(Json)
}

/// Drop an entry. Idempotent: an id that is already gone is not an error,
/// because the browser reconciles by diffing and may ask twice.
async fn delete_server(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "server_delete")?;
    let list = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    // Someone else's entry is left alone and answered like one already gone;
    // one the account may see but not write to is refused.
    let removed = list
        .iter()
        .find(|s| s.id == id && visible_to(&state.accounts, s, &account));
    if let Some(server) = removed {
        refuse_read_only_on(&state, &account, server, &origin, "server_delete")?;
    }
    let removed = removed.map(ServerRef::from);
    let kept: Vec<_> = list
        .into_iter()
        .filter(|s| s.id != id || !visible_to(&state.accounts, s, &account))
        .collect();
    save_servers(kept)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    // Only a deletion that deleted something: the second of two asks for
    // the same id changed nothing.
    if let Some(server) = removed {
        state.audit.record(&account, &origin, Event::ServerDeleted { server });
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UnlockRequest {
    /// The answer to the lock's question: anything for a plain entry, the
    /// entry's name for production — the same rule `policy::check` applies
    /// to a script that confirms one command.
    confirm: String,
}

#[derive(Serialize)]
struct UnlockResponse {
    /// How long the window is, so the page's countdown says what the
    /// bridge will do.
    seconds: u64,
}

/// Open the caller's write window on a locked entry (ADR 14). Refused for
/// an account that may not write there at all, and for an entry that is
/// not locked — there is nothing to open — and asked again, with the
/// strictness, when the confirmation does not satisfy it.
async fn unlock_writes(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UnlockRequest>,
) -> ApiResult<Json<UnlockResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    let server = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &server, &origin, "unlock")?;
    if !server.write_locked() {
        return Err(ApiError::BadRequest("writes on this entry are not locked".to_string()));
    }
    let strictness = zedis_connection::confirm_strictness(&server, &zedis_connection::DangerKind::WriteLocked);
    let answered = match strictness {
        zedis_connection::ConfirmStrictness::Click => !req.confirm.trim().is_empty(),
        zedis_connection::ConfirmStrictness::TypeName => req.confirm.trim() == server.name.trim(),
    };
    if !answered {
        return Err(ApiError::ConfirmationRequired {
            kind: zedis_connection::DangerKind::WriteLocked.i18n_key().to_string(),
            strictness: match strictness {
                zedis_connection::ConfirmStrictness::Click => "click",
                zedis_connection::ConfirmStrictness::TypeName => "type_name",
            },
        });
    }
    let seconds = state.unlocks.unlock(&account, &server.id).as_secs();
    state.audit.record(
        &account,
        &origin,
        Event::Unlocked {
            server: ServerRef::from(&server),
            seconds,
        },
    );
    Ok(Json(UnlockResponse { seconds }))
}

/// Close the caller's window early. Idempotent: closing one that has ended
/// is not an error, and only a window that was open makes an audit line.
async fn lock_writes(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    let server = visible_server(&state.accounts, &id, &account)?;
    if state.unlocks.lock(&account, &server.id) {
        state.audit.record(
            &account,
            &origin,
            Event::Locked {
                server: ServerRef::from(&server),
            },
        );
    }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SessionRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    visible_server(&state.accounts, &req.server, &account)?;
    let session = state
        .sessions
        .open(&req.server, req.db)
        .await
        .map_err(ApiError::Upstream)?;
    Ok(Json(SessionResponse { session }))
}

async fn close_session(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> ApiResult<StatusCode> {
    authorize(&state, &headers, &mut Origin::new(peer, &headers))?;
    state.sessions.close(&token).await;
    // Idempotent: closing a session that has already expired is not an error.
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Deserialize)]
pub(crate) struct PipelineSpecDto {
    offset: usize,
    count: usize,
    #[serde(default)]
    atomic: bool,
}

#[derive(Deserialize, Default)]
pub(crate) struct ExecRequest {
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

impl ExecRequest {
    /// The request an MCP tool makes (`mcp.rs`), whose commands arrive
    /// already decoded — so no frames — for one routed connection, every
    /// master, or the masters `fanout_nodes` names, one command each.
    pub(crate) fn decoded(server: &str, db: usize, fanout_nodes: Vec<String>, every_master: bool) -> Self {
        Self {
            server: server.to_string(),
            db,
            fanout: (every_master || !fanout_nodes.is_empty()).then(|| "masters".to_string()),
            fanout_nodes,
            ..Default::default()
        }
    }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<ExecResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
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

    let server = visible_server(&state.accounts, &req.server, &account)?;
    // Not `authorize_write`: most of what reaches this route *is* a read, and
    // a read-only account has to keep running those. The role — on *this*
    // entry, since an account's rules may grant it read-only here and full
    // elsewhere — is carried down to `policy::check`, which judges each
    // command.
    let read_only = state.accounts.is_read_only_on(&account, &server);
    let unlocked = state.unlocks.active(&account, &server.id);
    // Every command in a batch is judged. Gating only the first would let a
    // pipeline smuggle a FLUSHALL in behind a GET.
    let verdicts: Vec<policy::Verdict> = decoded
        .iter()
        .map(|args| {
            policy::check(
                &server,
                args,
                req.confirm.as_deref(),
                read_only,
                unlocked,
                req.session.is_some(),
            )
        })
        .collect();
    for (args, verdict) in decoded.iter().zip(&verdicts) {
        let refusal = match verdict {
            policy::Verdict::Allow | policy::Verdict::Confirmed { .. } => continue,
            policy::Verdict::Deny => ApiError::Forbidden(READ_ONLY_REFUSAL.to_string()),
            policy::Verdict::Confirm { kind, strictness } => ApiError::ConfirmationRequired {
                kind: kind.i18n_key().to_string(),
                strictness: match strictness {
                    zedis_connection::ConfirmStrictness::Click => "click",
                    zedis_connection::ConfirmStrictness::TypeName => "type_name",
                },
            },
        };
        // The refused command alone: the batch went nowhere.
        for line in audit::command_lines(&server, req.db, [(args, verdict)], false) {
            state.audit.record(&account, &origin, Event::Command(line));
        }
        return Err(refusal);
    }

    // What the log keeps of this batch, decided before it goes out and
    // written after, with the outcome.
    let lines = audit::command_lines(
        &server,
        req.db,
        decoded.iter().zip(&verdicts),
        state.audit.logs_writes(),
    );
    let result = forward(&state, &req, &decoded).await;
    let error = result.as_ref().err().map(ApiError::describe);
    for mut line in lines {
        line.error.clone_from(&error);
        state.audit.record(&account, &origin, Event::Command(line));
    }
    result.map(Json)
}

/// Send the judged commands of `req` and collect the replies as frames.
async fn forward(state: &AppState, req: &ExecRequest, decoded: &[Vec<Vec<u8>>]) -> ApiResult<ExecResponse> {
    let (values, nodes) = forward_values(state, req, decoded).await?;
    let replies = values
        .iter()
        .map(|value| {
            resp::encode_to_vec(value)
                .map(|bytes| B64.encode(bytes))
                .map_err(|e| ApiError::Upstream(e.to_string()))
        })
        .collect::<ApiResult<Vec<String>>>()?;
    Ok(ExecResponse { replies, nodes })
}

/// Send the judged commands of `req` and collect the replies as values,
/// with the `host:port` of the node behind each one for a fan-out. The
/// page's route encodes them as frames; the MCP tools (`mcp.rs`) read them.
pub(crate) async fn forward_values(
    state: &AppState,
    req: &ExecRequest,
    decoded: &[Vec<Vec<u8>>],
) -> ApiResult<(Vec<redis::Value>, Vec<String>)> {
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
        let nodes = servers.iter().map(|s| format!("{}:{}", s.host, s.port)).collect();
        return Ok((values, nodes));
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
            for args in decoded {
                pipe.add_command(resp::command_from_args(args));
            }
            conn.req_packed_commands(&pipe, spec.offset, spec.count)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?
        }
    };

    Ok((values, Vec::new()))
}

#[cfg(test)]
mod base_path_tests {
    use super::{Mounted, mounted, normalize_base_path};

    #[test]
    fn a_request_is_a_file_below_the_base_path_or_it_is_not_ours() {
        // At the root every path is a file of the build, as before.
        assert_eq!(mounted("", "/"), Mounted::File("/"));
        assert_eq!(mounted("", "/wasm/zedis_web.js"), Mounted::File("/wasm/zedis_web.js"));

        assert_eq!(mounted("/zedis", "/zedis/"), Mounted::File("/"));
        assert_eq!(mounted("/zedis", "/zedis/fonts/a.ttf"), Mounted::File("/fonts/a.ttf"));
        assert_eq!(mounted("/zedis", "/zedis"), Mounted::NeedsSlash);
        // A neighbour whose name merely starts the same way, and the site
        // root, are not this bridge's to answer.
        assert_eq!(mounted("/zedis", "/zedisx/"), Mounted::Outside);
        assert_eq!(mounted("/zedis", "/"), Mounted::Outside);
        assert_eq!(mounted("/zedis", "/v1/health"), Mounted::Outside);
    }

    #[test]
    fn a_base_path_is_a_leading_slash_and_no_trailing_one() {
        for root in ["", "/", "  ", "///"] {
            assert_eq!(normalize_base_path(root), Ok(String::new()), "{root:?} is the root");
        }
        for spelling in ["zedis", "/zedis", "zedis/", "/zedis/", " /zedis/ "] {
            assert_eq!(normalize_base_path(spelling), Ok("/zedis".to_string()), "{spelling:?}");
        }
        assert_eq!(
            normalize_base_path("/tools/zedis-web_1.0~x"),
            Ok("/tools/zedis-web_1.0~x".to_string())
        );
    }

    #[test]
    fn a_base_path_that_means_something_else_in_a_route_or_a_cookie_is_refused() {
        // `{id}` is a capture to the router; `;` and a space end the cookie's
        // `Path` attribute; `..` and an empty segment are not a place.
        for bad in [
            "/{id}", "/a;b", "/a b", "/a/../b", "/a//b", "/./a", "/a?b", "/a#b", "/中",
        ] {
            assert!(normalize_base_path(bad).is_err(), "{bad:?} was accepted");
        }
    }
}

#[cfg(test)]
mod server_tests {
    use super::{RedisServer, ServerEntry, assign_owner, auth, merge_update, visible_to};

    fn owned_by(owner: Option<&str>) -> RedisServer {
        RedisServer {
            name: "prod-eu".to_string(),
            owner: owner.map(str::to_string),
            ..Default::default()
        }
    }

    fn accounts(toml: &str) -> auth::Accounts {
        auth::Accounts::from_toml(toml).expect("accounts")
    }

    #[test]
    fn an_account_sees_its_own_entries_and_the_shared_ones() {
        let all =
            accounts("[[users]]\nname = \"alice\"\npassword = \"a\"\n[[users]]\nname = \"bob\"\npassword = \"b\"\n");
        assert!(visible_to(&all, &owned_by(Some("alice")), "alice"));
        assert!(
            !visible_to(&all, &owned_by(Some("alice")), "bob"),
            "a private entry is its owner's alone"
        );
        assert!(visible_to(&all, &owned_by(None), "bob"), "no owner: shared");
        assert!(
            visible_to(&all, &owned_by(Some("")), "bob"),
            "an empty owner is no owner"
        );
        // Names are compared whole: a prefix of someone's name is someone else.
        assert!(!visible_to(&all, &owned_by(Some("alice")), "alic"));
        // A name that is no account sees nothing shared — and its own it
        // cannot have.
        assert!(!visible_to(&all, &owned_by(None), "nobody"));
    }

    #[test]
    fn a_servers_list_narrows_the_shared_entries_and_never_the_own_ones() {
        let ruled = accounts("[[users]]\nname = \"bob\"\npassword = \"b\"\nservers = [\"staging*\"]\n");
        assert!(!visible_to(&ruled, &owned_by(None), "bob"), "prod-eu is not staging*");
        assert!(
            visible_to(&ruled, &owned_by(Some("bob")), "bob"),
            "its own, whatever its name"
        );
        let mut staging = owned_by(None);
        staging.name = "staging-1".to_string();
        assert!(visible_to(&ruled, &staging, "bob"));
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
        let json = serde_json::to_value(ServerEntry::new(stored(), false)).expect("json");
        assert_eq!(json["host"], "10.0.0.5");
        assert_eq!(json["port"], 6379);
        assert_eq!(json["username"], "app");
        assert_eq!(json["secrets_set"], serde_json::json!(["password", "ssh_key"]));
        let text = json.to_string();
        assert!(!text.contains("hunter2") && !text.contains("BEGIN KEY"), "{text}");
    }

    /// The account's role reaches the page as its own field, and is absent
    /// for a full account — an older page reading a newer bridge then sees
    /// exactly what it saw before.
    #[test]
    fn a_read_only_account_is_told_so_on_every_entry() {
        let full = serde_json::to_value(ServerEntry::new(stored(), false)).expect("json");
        assert!(full.get("account_read_only").is_none(), "absent for a full account");

        let limited = serde_json::to_value(ServerEntry::new(stored(), true)).expect("json");
        assert_eq!(limited["account_read_only"], true);
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
