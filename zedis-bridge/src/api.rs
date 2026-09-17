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

use crate::{auth, policy, resp, session::Sessions, static_files};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use redis::aio::ConnectionLike;
use serde::{Deserialize, Serialize};
use zedis_connection::{get_connection_manager, get_server, get_servers};

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub sessions: Sessions,
    pub logins: auth::Logins,
    /// Mark the login cookie `Secure`. Off only for a plain-http local run.
    pub secure_cookie: bool,
    /// Where the web build lives, when this bridge also serves it.
    pub web_root: Option<std::path::PathBuf>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/login", post(login))
        .route("/v1/logout", post(logout))
        .route("/v1/servers", get(servers))
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
        let (status, error, message, kind, strictness) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", String::new(), None, None),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m, None, None),
            ApiError::UnknownServer(m) => (StatusCode::NOT_FOUND, "unknown_server", m, None, None),
            ApiError::ConfirmationRequired { kind, strictness } => (
                // 428: the request is fine, it just needs a precondition met.
                StatusCode::PRECONDITION_REQUIRED,
                "confirmation_required",
                "this command needs an explicit confirmation".to_string(),
                Some(kind),
                Some(strictness),
            ),
            ApiError::Upstream(m) => (StatusCode::BAD_GATEWAY, "upstream", m, None, None),
        };
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

/// Either credential opens a route: the bearer token for scripts and the CLI,
/// a login cookie for a browser. The cookie is checked second because it is
/// the cheaper failure — an expired one is the common case.
fn authorize(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth::presented(bearer, &state.token) {
        return Ok(());
    }
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    match auth::cookie_value(cookie) {
        Some(id) if state.logins.accepts(id) => Ok(()),
        _ => Err(ApiError::Unauthorized),
    }
}

#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

/// Trade the bearer token for a cookie.
///
/// This is the only place the token is accepted in a body, and the only way a
/// browser gets in: afterwards the page holds no credential a script can read.
async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> ApiResult<Response> {
    if !auth::presented(Some(&format!("Bearer {}", req.token)), &state.token) {
        return Err(ApiError::Unauthorized);
    }
    let id = state.logins.open();
    Ok((
        [("set-cookie", auth::set_cookie(&id, state.secure_cookie))],
        Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response())
}

/// Drop this browser's login. Idempotent, and it always clears the cookie so a
/// stale one cannot linger after the server forgot it.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    if let Some(id) = auth::cookie_value(cookie) {
        state.logins.close(id);
    }
    (
        [("set-cookie", auth::clear_cookie(state.secure_cookie))],
        StatusCode::NO_CONTENT,
    )
        .into_response()
}

/// Anything that is not an API route is a file of the web build, when one is
/// configured.
///
/// Unauthenticated on purpose: the page has to load before anyone can log in,
/// and it is a client application, not data. Everything it then asks for is
/// behind the cookie.
async fn web_asset(State(state): State<AppState>, uri: axum::http::Uri) -> Response {
    let Some(root) = &state.web_root else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Some(path) = static_files::resolve(root, uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([("content-type", static_files::content_type(&path))], bytes).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "a web asset could not be read");
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(Serialize)]
struct ServerSummary {
    id: String,
    name: String,
}

/// The server list, names and ids only. Hosts, passwords, keys and SSH
/// settings stay in this process.
async fn servers(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<ServerSummary>>> {
    authorize(&state, &headers)?;
    let list = get_servers().map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(Json(
        list.into_iter()
            .map(|s| ServerSummary { id: s.id, name: s.name })
            .collect(),
    ))
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
    authorize(&state, &headers)?;
    get_server(&req.server).map_err(|_| ApiError::UnknownServer(req.server.clone()))?;
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

async fn exec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<ExecResponse>> {
    authorize(&state, &headers)?;
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

    let server = get_server(&req.server).map_err(|_| ApiError::UnknownServer(req.server.clone()))?;
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
            let masters = client.master_servers();
            let mut aimed: Vec<Option<redis::Cmd>> = masters.iter().map(|_| None).collect();
            for (label, args) in req.fanout_nodes.iter().zip(decoded.iter()) {
                if let Some(index) = masters.iter().position(|s| format!("{}:{}", s.host, s.port) == *label) {
                    aimed[index] = Some(resp::command_from_args(args));
                }
            }
            let replies: Vec<Option<redis::Value>> = client
                .query_async_masters_with_option(aimed)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            let mut answered = Vec::new();
            let mut values = Vec::new();
            for (server, reply) in masters.into_iter().zip(replies) {
                if let Some(value) = reply {
                    answered.push(server);
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
