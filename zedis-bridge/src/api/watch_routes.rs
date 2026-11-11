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

//! `/v1/watch`: what the server pushes, held by the bridge for the page that
//! asked and polled for (`crate::watch` does the holding).

use super::*;

#[derive(Deserialize)]
pub(super) struct WatchRequest {
    server: String,
    #[serde(default)]
    db: usize,
    /// `patterns`, `sharded`, `keyspace_events` or `monitor`.
    kind: String,
    /// The patterns or channels, for a subscription.
    #[serde(default)]
    channels: Vec<String>,
    /// The answer to the lock's question, for `MONITOR` on an entry whose
    /// writes are locked: the entry's name on production.
    #[serde(default)]
    confirm: Option<String>,
}

#[derive(Serialize)]
pub(super) struct WatchResponse {
    token: String,
    /// The nodes a `MONITOR` reads, as `host:port`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    nodes: Vec<String>,
    /// The nodes it could not start on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<WatchFailure>,
}

#[derive(Serialize)]
struct WatchFailure {
    node: String,
    error: String,
}

#[derive(Deserialize, Default)]
pub(super) struct PollRequest {
    /// How long to wait for something to arrive, capped at `watch::MAX_WAIT`.
    #[serde(default)]
    wait_ms: u64,
}

fn watch_error(e: WatchError) -> ApiError {
    match e {
        WatchError::TooMany => ApiError::BadRequest(format!(
            "at most {} subscriptions and monitors at once; close one first",
            crate::watch::MAX_PER_ACCOUNT
        )),
        // Gone (ended, swept, closed) answers like never there: a page that
        // asks after the end is told so with `closed`, not with an error.
        WatchError::Unknown => ApiError::BadRequest("no such watch".to_string()),
        WatchError::Invalid(m) => ApiError::BadRequest(m),
        WatchError::Upstream(m) => ApiError::Upstream(m),
    }
}

/// Subscribe to channels or keyspace notifications, or start `MONITOR`, and
/// get the token that polls for what arrives (`watch.rs`).
///
/// A subscription is observation, and a read-only account may watch: what
/// it hears is what it could read. `MONITOR` is not — it shows every other
/// client's commands, values and all, and slows the server — so it is a full
/// account's; on an entry whose writes are locked it takes the lock's
/// question answered (the entry's name on production) unless the write
/// window is open (ADR 14); and every start is a line of the audit log.
pub(super) async fn open_watch(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<WatchRequest>,
) -> ApiResult<Json<WatchResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    let kind = WatchKind::from_name(&req.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown watch kind {:?}", req.kind)))?;
    let server = visible_server(&state.accounts, &req.server, &account)?;
    note_server(&state, &origin, &server.id);
    let mut confirmed = None;
    if kind == WatchKind::Monitor {
        refuse_read_only_on(&state, &account, &server, &origin, "monitor")?;
        if server.write_locked() && !state.unlocks.active(&account, &server.id) {
            let strictness = zedis_connection::confirm_strictness(&server, &zedis_connection::DangerKind::WriteLocked);
            let name = match strictness {
                zedis_connection::ConfirmStrictness::Click => "click",
                zedis_connection::ConfirmStrictness::TypeName => "type_name",
            };
            let answer = req.confirm.as_deref().map(str::trim).unwrap_or_default();
            let answered = match strictness {
                zedis_connection::ConfirmStrictness::Click => !answer.is_empty(),
                zedis_connection::ConfirmStrictness::TypeName => answer == server.name.trim(),
            };
            if !answered {
                return Err(ApiError::ConfirmationRequired {
                    kind: zedis_connection::DangerKind::WriteLocked.i18n_key().to_string(),
                    strictness: name,
                });
            }
            confirmed = Some(name);
        }
    }
    let opened = state
        .watches
        .open(&account, &server.id, req.db, kind, &req.channels)
        .await;
    if kind == WatchKind::Monitor {
        state.audit.record(
            &account,
            &origin,
            Event::Command(audit::CommandLine {
                server: ServerRef::from(&server),
                db: req.db,
                command: "MONITOR".to_string(),
                args: Vec::new(),
                count: 1,
                outcome: if confirmed.is_some() {
                    audit::Outcome::Confirmed
                } else {
                    audit::Outcome::Allowed
                },
                kind: confirmed.map(|_| "write_locked"),
                confirm: confirmed,
                error: opened.as_ref().err().map(|e| format!("{e:?}")),
            }),
        );
    }
    let opened = opened.map_err(watch_error)?;
    tracing::info!(account, kind = req.kind, server = server.name, "watch opened");
    Ok(Json(WatchResponse {
        token: opened.token,
        nodes: opened.nodes,
        failures: opened
            .failures
            .into_iter()
            .map(|(node, error)| WatchFailure { node, error })
            .collect(),
    }))
}

fn verify_watch_access(
    state: &AppState,
    account: &str,
    server_id: &str,
    kind: WatchKind,
    origin: &Origin,
) -> ApiResult<()> {
    let server = visible_server(&state.accounts, server_id, account)?;
    if kind == WatchKind::Monitor {
        refuse_read_only_on(state, account, &server, origin, "monitor")?;
    }
    Ok(())
}

/// What has arrived on a watch, waiting up to `wait_ms` for something to.
pub(super) async fn poll_watch(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    body: Option<Json<PollRequest>>,
) -> ApiResult<Json<Batch>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    let (server_id, kind) = match state.watches.inspect(&token, &account) {
        Ok(meta) => meta,
        Err(WatchError::Unknown) => {
            return Ok(Json(Batch {
                closed: Some("gone".to_string()),
                ..Batch::default()
            }));
        }
        Err(e) => return Err(watch_error(e)),
    };
    if let Err(refusal) = verify_watch_access(&state, &account, &server_id, kind, &origin) {
        state.watches.close(&token, &account);
        return Err(refusal);
    }
    note_server(&state, &origin, &server_id);
    let wait = std::time::Duration::from_millis(body.map(|Json(b)| b.wait_ms).unwrap_or_default());
    match state.watches.poll(&token, &account, wait).await {
        Ok(batch) => {
            if !batch.items.is_empty()
                && let Err(refusal) = verify_watch_access(&state, &account, &server_id, kind, &origin)
            {
                state.watches.close(&token, &account);
                return Err(refusal);
            }
            Ok(Json(batch))
        }
        // Ended and gone, or swept while the page was away: it has ended,
        // which is an answer the page acts on rather than an error.
        Err(WatchError::Unknown) => Ok(Json(Batch {
            closed: Some("gone".to_string()),
            ..Batch::default()
        })),
        Err(e) => Err(watch_error(e)),
    }
}

/// End a watch now. Idempotent, like closing a session.
pub(super) async fn close_watch(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> ApiResult<StatusCode> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    state.watches.close(&token, &account);
    Ok(StatusCode::NO_CONTENT)
}
