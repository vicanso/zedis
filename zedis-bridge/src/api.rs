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

use crate::accounts::{Accounts, Basic, Setup, Signup};
use crate::audit::{self, Audit, Event, Origin, ServerRef};
use crate::presence::{Client, ClientKey, ONLINE_WINDOW, Presence};
use crate::static_files::{Representation, WebBuild};
use crate::store::{KeyNoteStore, MetricsStore, PageRowStore, ScriptStore, ServerStore, StoreVersions, TrashStore};
use crate::trash::{NewEntry, PutError};
use crate::watch::{Batch, WatchError, WatchKind, Watches};
use crate::{account_api, auth, mcp, page_data, policy, resp, session::Sessions, static_files};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use redis::aio::ConnectionLike;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;
use tower_http::compression::{CompressionLayer, predicate::SizeAbove};
use uuid::Uuid;
use zedis_connection::string::{decrypt_bytes, encrypt_bytes};
use zedis_connection::{
    BRIDGE_MAX_REQUEST_BYTES, DiagReportRow, DiagStage, DiagStatus, KillReply, KillTarget, PipelineSpec, RedisServer,
    get_connection_manager, get_server, get_servers, kill_running, run_diagnostics,
};

mod commands;
mod entries;
mod page;
mod sign_in;
mod status;
mod storage;
mod watch_routes;

pub(crate) use commands::{ExecRequest, forward_values};
use commands::{close_session, exec, open_session};
use entries::{
    add_server, delete_server, diagnose_server, kill_script, lock_writes, servers, unlock_writes, update_server,
};
pub use page::normalize_base_path;
use page::web_asset;
use sign_in::{login, logout};
use status::{clients, health, ready};
use storage::{get_trash, list_trash, metrics_since, put_metrics, put_trash, remove_trash};
use watch_routes::{close_watch, open_watch, poll_watch};

#[derive(Clone)]
pub struct AppState {
    /// The registered accounts (ADR 17), in memory in front of their store.
    pub accounts: Accounts,
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
    /// Who has made a request in the last five minutes (`GET /v1/clients`).
    pub presence: Presence,
    /// What the server pushes, held for the pages that asked (`/v1/watch`).
    pub watches: Watches,
    /// The server list (`/v1/servers`) — where a change is kept; reads go
    /// through `zedis_connection`'s cache, which the store keeps in step.
    pub servers: Arc<dyn ServerStore>,
    /// The browser build's recycle bin (`/v1/servers/{id}/trash`).
    pub trash: Arc<dyn TrashStore>,
    /// The browser build's Metrics history (`/v1/servers/{id}/metrics`).
    pub metrics: Arc<dyn MetricsStore>,
    /// The page's own data (`/v1/page-data`, ADR 17): each account's rows,
    /// the key tags and notes, the saved scripts.
    pub page_rows: Arc<dyn PageRowStore>,
    pub key_notes: Arc<dyn KeyNoteStore>,
    pub scripts: Arc<dyn ScriptStore>,
    /// Failed sign-ins, setup tokens and invitation codes, per address.
    pub throttle: auth::Throttle,
    /// The token that seats an administrator (ADR 17).
    pub setup: Setup,
    /// `--signup`: whether anyone may ask for an account, for an
    /// administrator to approve (ADR 17, amended 2026-10-18).
    pub signup: Signup,
    /// The database the stores are in, asked by `/v1/ready`; `None` for the
    /// local directory, which is there while the process is.
    pub store_probe: Option<Arc<dyn StoreVersions>>,
}

/// The smallest API answer worth compressing. Below a kilobyte the saving
/// is a few hundred bytes at most; and the small answers are the ones that
/// carry a one-time secret — an MCP token, an invitation code — which so
/// never meets a compressor (a BREACH-style length leak needs that; the
/// login cookie is also `SameSite=Strict`, so another site cannot make a
/// signed-in page ask in the first place).
const COMPRESS_FROM_BYTES: u64 = 1024;

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
    let router = Router::new()
        .route(&at("/v1/health"), get(health))
        .route(&at("/v1/ready"), get(ready))
        .route(&at("/v1/login"), post(login))
        .route(&at("/v1/logout"), post(logout))
        .route(&at("/v1/servers"), get(servers).post(add_server))
        .route(&at("/v1/servers/{id}"), delete(delete_server).put(update_server))
        .route(&at("/v1/servers/{id}/unlock"), post(unlock_writes).delete(lock_writes))
        .route(&at("/v1/servers/{id}/diagnose"), post(diagnose_server))
        .route(&at("/v1/servers/{id}/kill-script"), post(kill_script))
        .route(&at("/v1/servers/{id}/trash"), get(list_trash).post(put_trash))
        .route(&at("/v1/servers/{id}/metrics"), get(metrics_since).post(put_metrics))
        .route(
            &at("/v1/servers/{id}/trash/{entry}"),
            get(get_trash).delete(remove_trash),
        )
        .route(&at("/v1/exec"), post(exec))
        .route(&at("/v1/session"), post(open_session))
        .route(&at("/v1/session/{token}"), delete(close_session))
        .route(&at("/v1/watch"), post(open_watch))
        .route(&at("/v1/watch/{token}"), delete(close_watch))
        .route(&at("/v1/watch/{token}/poll"), post(poll_watch))
        .route(&at("/v1/clients"), get(clients))
        // The MCP entry point (ADR 15): JSON-RPC over POST alone. GET is
        // the server-initiated stream this bridge does not offer, DELETE a
        // session it does not keep; both answer 405 as the protocol allows.
        .route(
            &at("/v1/mcp"),
            post(mcp::post).get(mcp::not_allowed).delete(mcp::not_allowed),
        );
    // Registration and administration, and the page's own data (ADR 17).
    let router = account_api::routes(router, &at);
    page_data::routes(router, &at)
        // On the API routes only — the page's files need no login.
        .route_layer(middleware::from_fn_with_state(state.clone(), renew_login_cookie))
        // The API's answers compressed for a client that asks: a value
        // base64 inside JSON, a page of keys, the page's own data. zstd where
        // it is accepted — at its default level a tenth of gzip's time for a
        // smaller result — and gzip for the rest. Routes only: the page's
        // files are compressed once, at build time, and sent as they are.
        .route_layer(
            CompressionLayer::new()
                .gzip(true)
                .zstd(true)
                .compress_when(SizeAbove::new(COMPRESS_FROM_BYTES)),
        )
        // axum's default is the same number. It is said here because the
        // page sizes what it sends by it — a value it cannot save opens
        // read-only — and two copies of a limit drift apart.
        .layer(DefaultBodyLimit::max(BRIDGE_MAX_REQUEST_BYTES))
        .fallback(web_asset)
        .with_state(state)
}

/// Renew the login cookie's `Max-Age` on every request it authorized. The
/// server side counts idleness from the last request, but the browser
/// dropped the cookie `Max-Age` after it was *set* — so "signed out after 8
/// hours idle" was "signed out 8 hours after signing in", busy or not.
///
/// And before the request: a login another process opened is taken in from
/// the store, and an account it registered is read, so the route's
/// `authorize` — not async, and asked everywhere — finds them in memory
/// (ADR 18).
async fn renew_login_cookie(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let cookie = request.headers().get("cookie").and_then(|v| v.to_str().ok());
    let id = auth::cookie_value(cookie).map(str::to_string);
    if let Some(id) = &id {
        state.logins.fetch(id, &state.accounts).await;
    }
    // A script's account another process registered within the last poll:
    // read the accounts again before `authorize` turns it away.
    let basic = request.headers().get("authorization").and_then(|v| v.to_str().ok());
    if let Some((name, _)) = auth::basic_credentials(basic)
        && !state.accounts.exists(&name)
    {
        state.accounts.catch_up().await;
    }
    let mut response = next.run(request).await;
    // A route that set a cookie of its own (login, logout) has the last word.
    if let Some(id) = id
        && !response.headers().contains_key("set-cookie")
        && let Some(timeout) = state.logins.idle_timeout_of(&id)
        && let Ok(value) = HeaderValue::from_str(&state.cookie.set(&id, timeout))
    {
        response.headers_mut().insert("set-cookie", value);
    }
    response
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
    /// What the request would change changed under it first.
    Conflict(String),
    /// An account, an invitation or a token that is not there.
    NotFound(String),
    /// Too many failed attempts from this address; try later.
    TooManyAttempts,
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
            ApiError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone(), None, None),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m.clone(), None, None),
            ApiError::TooManyAttempts => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_attempts",
                "too many failed attempts from this address; try again in a quarter of an hour".to_string(),
                None,
                None,
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

pub(crate) type ApiResult<T> = Result<T, ApiError>;

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
        note_client(state, ClientKey::Proxy(name.clone()), &name, origin);
        return Ok(name);
    }
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    match state.accounts.check_basic(authorization) {
        Basic::Accepted(account) => {
            let attempt = auth::attempt_key(origin.peer_ip, &account);
            if state.throttle.blocked(&attempt) {
                return Err(ApiError::TooManyAttempts);
            }
            state.throttle.succeeded(&attempt);
            let key = ClientKey::Basic {
                account: account.clone(),
                peer: origin.peer_ip,
            };
            note_client(state, key, &account, origin);
            return Ok(account);
        }
        // A script's wrong password counts like the form's: Basic is the
        // other way to guess one.
        Basic::Refused(tried) => {
            let attempt = auth::attempt_key(origin.peer_ip, &tried);
            if state.throttle.blocked(&attempt) {
                return Err(ApiError::TooManyAttempts);
            }
            state.throttle.failed(&attempt);
            return Err(ApiError::Unauthorized);
        }
        Basic::Absent => {}
    }
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    let id = auth::cookie_value(cookie).ok_or(ApiError::Unauthorized)?;
    let account = state
        .logins
        .account(id, &state.accounts)
        .ok_or(ApiError::Unauthorized)?;
    note_client(state, ClientKey::Browser(auth::login_key(id)), &account, origin);
    Ok(account)
}

/// Count the caller in for `GET /v1/clients`, and say so in the process log
/// when it has just come online.
fn note_client(state: &AppState, key: ClientKey, account: &str, origin: &mut Origin) {
    let kind = key.kind();
    if let Some(online) = state.presence.seen(key.clone(), account, origin) {
        tracing::info!(account, kind, online, "client online");
    }
    origin.client = Some(key);
}

/// Note against the caller that its request named `server_id`, for the
/// online list's *servers*: what a page beats on, a terminal's session, a
/// subscription, a tool call.
pub(crate) fn note_server(state: &AppState, origin: &Origin, server_id: &str) {
    if let Some(key) = &origin.client {
        state.presence.used(key, server_id);
    }
}

/// Who is calling at the MCP entry point (ADR 15): an account's token first
/// — `Authorization: Bearer zdt_…`, which signs in here and nowhere else —
/// then whatever [`authorize`] accepts, so a client set up with HTTP Basic
/// before tokens existed goes on working. The token's name goes into every
/// audit line of the request.
pub(crate) fn authorize_mcp(state: &AppState, headers: &HeaderMap, origin: &mut Origin) -> ApiResult<String> {
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    if let Some((account, token)) = state.accounts.account_for_token(authorization) {
        origin.auth = Some("token");
        let key = ClientKey::Token {
            account: account.clone(),
            token: token.clone(),
        };
        note_client(state, key, &account, origin);
        origin.token = Some(token);
        return Ok(account);
    }
    authorize(state, headers, origin)
}

/// What a read-only account is told, wherever it is refused. One wording,
/// because the page shows it verbatim and the CLI prints it.
pub(crate) const READ_ONLY_REFUSAL: &str = "this account is read-only";

/// The longest name a failed login, or an unknown proxy identity, is
/// recorded under: it is whatever was typed into the box or written into
/// the header, and the log is not the place for a pasted essay.
pub(crate) const MAX_TRIED_NAME: usize = 64;

/// Who is calling, for a route that *changes* something — the server list,
/// or Redis itself. Same check as [`authorize`], plus the account's role.
///
/// Every mutating route goes through this one rather than through
/// [`authorize`] with a flag test bolted on, so adding a route and
/// forgetting the test is a compile error's worth of obvious: the route
/// either asks this function or it does not change anything. `action` is
/// what the audit log says was refused.
pub(crate) fn authorize_write(
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
pub(crate) fn visible_to(accounts: &Accounts, server: &RedisServer, account: &str) -> bool {
    match server.owner.as_deref() {
        None | Some("") => accounts.may_see_shared(account, server),
        Some(owner) => owner == account,
    }
}

/// The entry `id` names, if `account` may see it. An entry that exists but
/// is someone else's answers exactly like one that does not exist, so the
/// routes never confirm what another account has.
pub(crate) fn visible_server(accounts: &Accounts, id: &str, account: &str) -> ApiResult<RedisServer> {
    get_server(id)
        .ok()
        .filter(|server| visible_to(accounts, server, account))
        .ok_or_else(|| ApiError::UnknownServer(id.to_string()))
}

/// Refuse a change to `server` by an account that may not write there —
/// read-only outright, or read-only on this entry by its rules — the way
/// `authorize_write` refuses one that may write nowhere.
pub(crate) fn refuse_read_only_on(
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

#[cfg(test)]
mod mcp_auth_tests {
    use super::*;

    pub(super) fn state(accounts: &str) -> AppState {
        AppState {
            accounts: Accounts::from_toml(accounts).expect("accounts"),
            sessions: Sessions::new(),
            logins: auth::Logins::new(),
            audit: Audit::off(),
            trusted: None,
            unlocks: policy::Unlocks::new(),
            cookie: auth::CookiePolicy::new(false, ""),
            base_path: String::new(),
            web_root: None,
            mcp_calls: mcp::Limiter::new(),
            presence: Presence::new(),
            watches: Watches::new(),
            servers: Arc::new(crate::store::LocalServers),
            trash: Arc::new(crate::trash::LocalTrash::new(
                std::env::temp_dir().join("zedis-bridge-test-trash"),
            )),
            metrics: Arc::new(crate::metrics::LocalMetrics::new(
                std::env::temp_dir().join("zedis-bridge-test-metrics"),
            )),
            page_rows: Arc::new(crate::page_data::LocalPageRows::new(
                std::env::temp_dir().join("zedis-bridge-test-rows"),
            )),
            key_notes: Arc::new(crate::page_data::LocalKeyNotes::new(
                std::env::temp_dir().join("zedis-bridge-test-notes"),
            )),
            scripts: Arc::new(crate::page_data::LocalScripts::new(
                std::env::temp_dir().join("zedis-bridge-test-scripts.json"),
            )),
            throttle: auth::Throttle::new(),
            setup: Setup::in_dir(&std::env::temp_dir().join("zedis-bridge-test-setup")),
            signup: Signup::Off,
            store_probe: None,
        }
    }

    pub(super) fn headers(authorization: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", authorization.parse().expect("header"));
        headers
    }

    /// The MCP entry point takes an account's token and says which one in
    /// its audit lines; HTTP Basic still works there; and the token opens no
    /// other route (ADR 15).
    #[test]
    fn a_token_signs_in_at_the_mcp_entry_point_and_nowhere_else() {
        let (token, sha256) = auth::new_mcp_token();
        let state = state(&format!(
            "[[users]]\nname = \"ai\"\npassword = \"secret\"\nread_only = true\n\
             tokens = [{{ name = \"claude-laptop\", sha256 = \"{sha256}\" }}]"
        ));
        let bearer = headers(&format!("Bearer {token}"));

        let mut origin = Origin::default();
        let Ok(account) = authorize_mcp(&state, &bearer, &mut origin) else {
            panic!("the token did not sign in");
        };
        assert_eq!(account, "ai");
        assert_eq!(origin.auth, Some("token"));
        assert_eq!(origin.token.as_deref(), Some("claude-laptop"));

        let basic = headers(&format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("ai:secret")
        ));
        let mut origin = Origin::default();
        let Ok(account) = authorize_mcp(&state, &basic, &mut origin) else {
            panic!("Basic did not sign in");
        };
        assert_eq!(account, "ai");
        assert_eq!((origin.auth, origin.token), (None, None));

        let mut origin = Origin::default();
        assert!(matches!(
            authorize(&state, &bearer, &mut origin),
            Err(ApiError::Unauthorized)
        ));
        let mut origin = Origin::default();
        assert!(matches!(
            authorize_mcp(&state, &headers("Bearer zdt_nobody"), &mut origin),
            Err(ApiError::Unauthorized)
        ));
    }
}

#[cfg(test)]
mod body_limit_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The status line the bridge answers a `POST /v1/login` whose body is
    /// `len` bytes long.
    async fn status_for_a_body_of(addr: SocketAddr, len: usize) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let head = format!(
            "POST /v1/login HTTP/1.1\r\nhost: bridge\r\ncontent-type: application/json\r\n\
             content-length: {len}\r\nconnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.expect("head");
        // The bridge may answer from the header alone and close; what it
        // says is the point, not whether all of the body got through.
        let _ = stream.write_all(&vec![b' '; len]).await;
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply).await;
        String::from_utf8_lossy(&reply)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The bridge takes a request up to `BRIDGE_MAX_REQUEST_BYTES` and no
    /// more — the number the page sizes its writes by, so a value it opens
    /// as editable is one it can save.
    #[tokio::test]
    async fn the_bridge_takes_a_body_up_to_the_limit_the_page_sizes_its_writes_by() {
        let state = super::mcp_auth_tests::state("[[users]]\nname = \"alice\"\npassword = \"alice-password\"\n");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let over = status_for_a_body_of(addr, BRIDGE_MAX_REQUEST_BYTES + 1).await;
        assert!(over.contains(" 413 "), "{over}");
        // Spaces are not a login, so the answer is a refusal of another kind.
        let at = status_for_a_body_of(addr, BRIDGE_MAX_REQUEST_BYTES).await;
        assert!(at.starts_with("HTTP/1.1 4") && !at.contains(" 413 "), "{at}");
        server.abort();
    }
}

#[cfg(test)]
mod compression_tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// What one request answered: the status line, the `content-encoding`
    /// header if any, and the body as sent.
    struct Answer {
        status: String,
        encoding: Option<String>,
        body: Vec<u8>,
    }

    async fn ask(addr: SocketAddr, head: &str, body: &str) -> Answer {
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "{head}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.expect("request");
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.expect("reply");
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n").expect("a header block");
        let head = String::from_utf8_lossy(&reply[..split]).to_string();
        let encoding = head
            .lines()
            .find_map(|line| line.strip_prefix("content-encoding: "))
            .map(str::to_string);
        let raw = &reply[split + 4..];
        // A compressed answer's length is not known ahead: it is chunked.
        let body = if head.lines().any(|line| line == "transfer-encoding: chunked") {
            dechunk(raw)
        } else {
            raw.to_vec()
        };
        Answer {
            status: head.lines().next().unwrap_or_default().to_string(),
            encoding,
            body,
        }
    }

    /// A chunked body's data: a hex length, its bytes, again, until 0.
    fn dechunk(mut raw: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        loop {
            let line_end = raw.windows(2).position(|w| w == b"\r\n").expect("a chunk size");
            let size = usize::from_str_radix(String::from_utf8_lossy(&raw[..line_end]).trim(), 16).expect("hex");
            if size == 0 {
                return body;
            }
            let start = line_end + 2;
            body.extend_from_slice(&raw[start..start + size]);
            raw = &raw[start + size + 2..];
        }
    }

    /// The API's answers are compressed for a client that asks — zstd where
    /// it is accepted, gzip otherwise, nothing for a coding not offered or an
    /// answer under a kilobyte — and the page's own files are left as the
    /// build made them.
    #[tokio::test]
    async fn the_api_answers_compressed_and_the_page_is_left_alone() {
        let state = super::mcp_auth_tests::state("[[users]]\nname = \"ai\"\npassword = \"secret\"\nread_only = true\n");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let basic = format!("authorization: Basic {}\r\n", B64.encode("ai:secret"));
        let tools = |accept: &str| {
            format!(
                "POST /v1/mcp HTTP/1.1\r\nhost: bridge\r\ncontent-type: application/json\r\n\
                 accept: application/json, text/event-stream\r\n{basic}accept-encoding: {accept}\r\n"
            )
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        let zstd = ask(addr, &tools("zstd, gzip"), list).await;
        assert!(zstd.status.contains(" 200 "), "{}", zstd.status);
        assert_eq!(zstd.encoding.as_deref(), Some("zstd"));

        let gzip = ask(addr, &tools("gzip"), list).await;
        assert_eq!(gzip.encoding.as_deref(), Some("gzip"));
        let mut text = String::new();
        GzDecoder::new(gzip.body.as_slice())
            .read_to_string(&mut text)
            .expect("gzip");
        assert!(text.contains("\"scan_keys\""), "the tool list, whole");
        assert!(zstd.body.len() < text.len() && gzip.body.len() < text.len());

        let brotli = ask(addr, &tools("br"), list).await;
        assert_eq!(brotli.encoding, None, "brotli is not offered");

        let small = ask(
            addr,
            "GET /v1/health HTTP/1.1\r\nhost: bridge\r\naccept-encoding: zstd\r\n",
            "",
        )
        .await;
        assert!(small.status.contains(" 200 "), "{}", small.status);
        assert_eq!(small.encoding, None, "a few bytes are not worth it");

        let page = ask(
            addr,
            "GET / HTTP/1.1\r\nhost: bridge\r\naccept-encoding: zstd, gzip\r\n",
            "",
        )
        .await;
        assert!(page.status.contains(" 200 "), "{}", page.status);
        assert!(page.body.len() > 1024, "the page is larger than the threshold");
        assert_eq!(
            page.encoding, None,
            "the page's files are compressed at build time, not here"
        );
        server.abort();
    }
}
