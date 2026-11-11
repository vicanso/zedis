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

//! The routes of registration and administration (ADR 17): the setup token,
//! invitations, requests for an account where the bridge takes them, an
//! account's own page and the administrators' one. The
//! pages are HTML beside the sign-in form (`zedis-web/www`); these are what
//! they call.
//!
//! A code — the setup token, an invitation's — always travels in a request
//! body, never in a URL: a reverse proxy's access log keeps URLs. The page's
//! links carry an invitation in the fragment (`#invite=…`), which a browser
//! never sends anywhere. Every attempt with a code or a password is counted
//! per address ([`auth::Throttle`]), and the answer to a wrong one is the
//! same whatever was wrong about it.

use crate::accounts::{
    AccountChange, AccountError, AccountView, INVITE_DAYS, InviteKind, InviteView, Role, SignupView, check_new_name,
    check_password, hash_password,
};
use crate::api::{ApiError, ApiResult, AppState, MAX_TRIED_NAME, authorize};
use crate::audit::{Change, Event, Origin};
use crate::auth::{self, attempt_key, parse_expiry};
use crate::handover::{self, Disposition, Owned};
use crate::store::{AuditPage, AuditQuery};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;

/// The routes, at `at(path)` — the same mounting as every other.
pub(crate) fn routes(router: Router<AppState>, at: &impl Fn(&str) -> String) -> Router<AppState> {
    router
        .route(&at("/v1/setup"), get(setup_state).post(setup))
        .route(&at("/v1/invitation"), post(invitation))
        .route(&at("/v1/register"), post(register))
        .route(&at("/v1/signup"), post(signup))
        .route(&at("/v1/account"), get(own_account))
        .route(&at("/v1/account/password"), post(change_password))
        .route(&at("/v1/account/tokens"), post(issue_own_token))
        .route(&at("/v1/account/tokens/{token}"), delete(revoke_own_token))
        .route(&at("/v1/admin/accounts"), get(list_accounts))
        .route(
            &at("/v1/admin/accounts/{name}"),
            patch(update_account).delete(delete_account),
        )
        .route(&at("/v1/admin/accounts/{name}/owned"), get(owned_of))
        .route(&at("/v1/admin/orphans"), get(list_orphans))
        .route(&at("/v1/admin/orphans/{owner}"), post(settle_orphans))
        .route(&at("/v1/admin/accounts/{name}/reset"), post(reset_link))
        .route(&at("/v1/admin/accounts/{name}/tokens"), post(issue_token_for))
        .route(&at("/v1/admin/accounts/{name}/tokens/{token}"), delete(revoke_token_of))
        .route(&at("/v1/admin/invites"), get(list_invites).post(create_invite))
        .route(&at("/v1/admin/invites/{id}"), delete(revoke_invite))
        .route(&at("/v1/admin/signups"), get(list_signups))
        .route(&at("/v1/admin/signups/{id}"), delete(reject_signup))
        .route(&at("/v1/admin/signups/{id}/approve"), post(approve_signup))
        .route(&at("/v1/admin/audit"), get(audit_lines))
}

impl From<AccountError> for ApiError {
    fn from(e: AccountError) -> Self {
        match e {
            AccountError::NotFound => ApiError::NotFound(e.to_string()),
            AccountError::NameTaken
            | AccountError::LastAdmin
            | AccountError::TokenNameTaken
            | AccountError::TooManySignups => ApiError::Conflict(e.to_string()),
            AccountError::BadName(_)
            | AccountError::BadPassword(_)
            | AccountError::BadRule(_)
            | AccountError::InviteInvalid
            | AccountError::NotReadOnly
            | AccountError::LockedAdmin => ApiError::BadRequest(e.to_string()),
            AccountError::Store(_) => ApiError::Upstream(e.to_string()),
        }
    }
}

/// What a page is answered on a sign-in it caused — setup, registration, a
/// new password: the login cookie, as `/v1/login` sets it.
async fn signed_in(state: &AppState, account: &str, remember: bool, origin: &Origin) -> ApiResult<Response> {
    let id = state
        .logins
        .open_saved(account, &state.accounts, remember)
        .await
        .ok_or(ApiError::Unauthorized)?;
    state.audit.record(account, origin, Event::Login { remember });
    Ok((
        [("set-cookie", state.cookie.set(&id, auth::idle_timeout(remember)))],
        Json(serde_json::json!({ "status": "ok", "account": account })),
    )
        .into_response())
}

/// Turn away an address that has failed `what` too often, before anything
/// is checked.
fn not_throttled(state: &AppState, key: &str) -> ApiResult<()> {
    if state.throttle.blocked(key) {
        return Err(ApiError::TooManyAttempts);
    }
    Ok(())
}

/// Hash `password` off the async workers.
async fn hashed(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?
        .map_err(ApiError::from)
}

/// Whether `password` is `name`'s, off the async workers.
async fn password_is(state: &AppState, name: &str, password: &str) -> bool {
    let (accounts, name, password) = (state.accounts.clone(), name.to_string(), password.to_string());
    tokio::task::spawn_blocking(move || accounts.accepts_password(&name, &password))
        .await
        .unwrap_or(false)
}

/// Who is calling, for a route only an administrator may use. A refusal is
/// an audit line, as a read-only account's is.
pub(crate) async fn authorize_admin(
    state: &AppState,
    headers: &HeaderMap,
    origin: &mut Origin,
    action: &'static str,
) -> ApiResult<String> {
    let account = authorize(state, headers, origin).await?;
    if !state.accounts.is_admin(&account) {
        state.audit.record(&account, origin, Event::Refused { action });
        return Err(ApiError::Forbidden("administrators only".to_string()));
    }
    Ok(account)
}

/// Turn away a locked account's change to its own credentials — what one
/// visitor of a shared demonstration account would otherwise change for all
/// of them. A refusal is an audit line, as a read-only account's is.
fn refuse_locked(state: &AppState, account: &str, origin: &Origin, action: &'static str) -> ApiResult<()> {
    if state.accounts.is_locked(account) {
        state.audit.record(account, origin, Event::Refused { action });
        return Err(ApiError::Forbidden(
            "this account is locked: an administrator keeps its password and tokens".to_string(),
        ));
    }
    Ok(())
}

/// `"2027-01-01"` → the first second of that day, if it is still to come.
fn expiry(text: Option<&str>) -> ApiResult<Option<u64>> {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let at = parse_expiry(text).map_err(ApiError::BadRequest)?;
    if at <= auth::now_secs() {
        return Err(ApiError::BadRequest(format!("{text} is already past")));
    }
    Ok(Some(at))
}

// ---- setup -------------------------------------------------------------

/// What the sign-in page offers besides signing in.
#[derive(Serialize)]
struct SetupState {
    /// Whether the page offers *Create the administrator*.
    open: bool,
    /// Whether it offers *Request an account* (`--signup approval`).
    signup: bool,
}

async fn setup_state(State(state): State<AppState>) -> Json<SetupState> {
    Json(SetupState {
        open: state.setup.open(&state.accounts).await,
        signup: state.signup.is_open(),
    })
}

#[derive(Deserialize)]
struct SetupRequest {
    token: String,
    name: String,
    password: String,
    #[serde(default)]
    remember: bool,
}

/// Seat an administrator with the setup token: a new account, or an
/// existing one that gives its password.
async fn setup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SetupRequest>,
) -> ApiResult<Response> {
    let origin = Origin::new(peer, &headers);
    let attempt = attempt_key(origin.peer_ip, "setup");
    not_throttled(&state, &attempt)?;
    if !state.setup.open(&state.accounts).await {
        return Err(ApiError::NotFound("setup is done: an administrator exists".to_string()));
    }
    let name = req.name.trim().to_string();
    let tried: String = name.chars().take(MAX_TRIED_NAME).collect();
    if !state.setup.accepts(&req.token, &state.accounts).await {
        state.throttle.failed(&attempt);
        state.audit.record(&tried, &origin, Event::Refused { action: "setup" });
        return Err(ApiError::Forbidden("that is not the setup token".to_string()));
    }
    let promoted = if state.accounts.exists(&name) {
        if !password_is(&state, &name, &req.password).await {
            state.throttle.failed(&attempt);
            state.audit.record(&tried, &origin, Event::LoginFailed);
            return Err(ApiError::Forbidden(
                "that account exists: its own password makes it the administrator".to_string(),
            ));
        }
        state.accounts.make_admin(&name, None).await?
    } else {
        crate::accounts::check_new_name(&name)?;
        check_password(&req.password)?;
        let hash = hashed(req.password).await?;
        state.accounts.make_admin(&name, Some(hash)).await?
    };
    state.setup.used().await;
    state.throttle.succeeded(&attempt);
    state.audit.record(&name, &origin, Event::SetupCompleted { promoted });
    tracing::info!(account = name, promoted, "administrator seated with the setup token");
    signed_in(&state, &name, req.remember, &origin).await
}

// ---- invitations and registration ----------------------------------------

#[derive(Deserialize)]
struct CodeRequest {
    code: String,
}

#[derive(Serialize)]
struct InvitationInfo {
    #[serde(flatten)]
    invite: InviteView,
    /// For an invitation marked for the proxy: the name it would register,
    /// when the link was opened behind the proxy.
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy_name: Option<String>,
}

/// What an invitation link is for, so the page can draw its form.
async fn invitation(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CodeRequest>,
) -> ApiResult<Json<InvitationInfo>> {
    let origin = Origin::new(peer, &headers);
    let attempt = attempt_key(origin.peer_ip, "invitation");
    not_throttled(&state, &attempt)?;
    let Some(invite) = state.accounts.invite(&req.code).await else {
        state.throttle.failed(&attempt);
        return Err(AccountError::InviteInvalid.into());
    };
    let proxy_name = match &invite.kind {
        InviteKind::Register { proxy: true, .. } => asserted(&state, &origin, &headers),
        _ => None,
    };
    Ok(Json(InvitationInfo { invite, proxy_name }))
}

/// The name a trusted reverse proxy asserted for this request, if any.
fn asserted(state: &AppState, origin: &Origin, headers: &HeaderMap) -> Option<String> {
    let (proxy, peer) = (state.trusted.as_ref()?, origin.peer_ip?);
    proxy.asserted(peer, headers)
}

#[derive(Deserialize)]
struct RegisterRequest {
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    remember: bool,
}

/// Register with an invitation — or set a new password with a reset one —
/// and sign in.
async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Response> {
    let origin = Origin::new(peer, &headers);
    let attempt = attempt_key(origin.peer_ip, "invitation");
    not_throttled(&state, &attempt)?;
    let Some(invite) = state.accounts.invite(&req.code).await else {
        state.throttle.failed(&attempt);
        return Err(AccountError::InviteInvalid.into());
    };
    let name = match invite.kind {
        InviteKind::Register { role, proxy, .. } => {
            let (name, hash) = if proxy {
                let name = asserted(&state, &origin, &headers).ok_or_else(|| {
                    ApiError::BadRequest("open this invitation through the sign-in proxy".to_string())
                })?;
                (name, None)
            } else {
                check_password(&req.password)?;
                (req.name.trim().to_string(), Some(hashed(req.password).await?))
            };
            let (view, invite) = state.accounts.register(&req.code, &name, hash).await?;
            state.audit.record(
                &view.name,
                &origin,
                Event::AccountRegistered {
                    invite,
                    role: role.name(),
                },
            );
            tracing::info!(account = view.name, role = role.name(), "account registered");
            view.name
        }
        InviteKind::Reset { .. } => {
            check_password(&req.password)?;
            let hash = hashed(req.password).await?;
            let (name, _) = state.accounts.reset_password(&req.code, hash).await?;
            state.audit.record(
                &name,
                &origin,
                Event::PasswordChanged {
                    target: name.clone(),
                    reset: true,
                },
            );
            name
        }
    };
    state.throttle.succeeded(&attempt);
    signed_in(&state, &name, req.remember, &origin).await
}

// ---- asking for an account ---------------------------------------------------

#[derive(Deserialize)]
struct SignupRequest {
    name: String,
    password: String,
    /// What the person tells the administrator.
    #[serde(default)]
    note: String,
}

/// Ask for an account (ADR 17, amended 2026-10-18), where the bridge takes
/// requests. The answer is the same whatever the name: whether one is taken
/// is for the administrator to see at approval, not for whoever asks.
async fn signup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SignupRequest>,
) -> ApiResult<Response> {
    if !state.signup.is_open() {
        return Err(ApiError::NotFound(
            "this bridge takes no requests for an account: ask an administrator for an invitation".to_string(),
        ));
    }
    let origin = Origin::new(peer, &headers);
    let attempt = attempt_key(origin.peer_ip, "signup");
    not_throttled(&state, &attempt)?;
    // Every request counts, not only a refused one: that is what keeps one
    // address from filling the list, and the hash below is paid only
    // within it.
    state.throttle.failed(&attempt);
    let name = req.name.trim().to_string();
    check_new_name(&name)?;
    check_password(&req.password)?;
    let hash = hashed(req.password).await?;
    let peer_ip = origin.peer_ip.map(|ip| ip.to_string());
    let request = state.accounts.request_signup(&name, hash, &req.note, peer_ip).await?;
    state.audit.record(&name, &origin, Event::SignupRequested { request });
    tracing::info!(account = name, "an account was asked for");
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "requested" })),
    )
        .into_response())
}

// ---- one's own account -----------------------------------------------------

#[derive(Serialize)]
struct OwnAccount {
    #[serde(flatten)]
    account: AccountView,
    /// Whether this bridge believes a reverse proxy: the page then does not
    /// offer a password to an account that has none.
    proxy: bool,
    /// Whether it takes requests for an account: an administrator's page
    /// then has them to answer.
    signup: bool,
}

async fn own_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<OwnAccount>> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    let view = state
        .accounts
        .view(&account)
        .ok_or_else(|| ApiError::NotFound(account.clone()))?;
    Ok(Json(OwnAccount {
        account: view,
        proxy: state.trusted.is_some(),
        signup: state.signup.is_open(),
    }))
}

#[derive(Deserialize)]
struct PasswordRequest {
    current: String,
    new: String,
}

/// A new password, given the current one. It ends every login the account
/// had — this browser's too, which gets a fresh one in the answer.
async fn change_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PasswordRequest>,
) -> ApiResult<Response> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    refuse_locked(&state, &account, &origin, "password_change")?;
    if !state.accounts.view(&account).is_some_and(|v| v.has_password) {
        // A password would let it sign in around the proxy that is its door.
        return Err(ApiError::BadRequest(
            "this account signs in through the proxy and has no password".to_string(),
        ));
    }
    let attempt = attempt_key(origin.peer_ip, &account);
    not_throttled(&state, &attempt)?;
    if !password_is(&state, &account, &req.current).await {
        state.throttle.failed(&attempt);
        return Err(ApiError::Forbidden("the current password is not right".to_string()));
    }
    check_password(&req.new)?;
    let hash = hashed(req.new).await?;
    state.accounts.set_password(&account, hash).await?;
    state.audit.record(
        &account,
        &origin,
        Event::PasswordChanged {
            target: account.clone(),
            reset: false,
        },
    );
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
    let remember = match auth::cookie_value(cookie) {
        Some(id) => {
            let remember = state.logins.idle_timeout_of(id) == Some(auth::REMEMBERED_IDLE_TIMEOUT);
            state.logins.close(id);
            remember
        }
        None => return Ok(Json(serde_json::json!({ "status": "ok" })).into_response()),
    };
    signed_in(&state, &account, remember, &origin).await
}

#[derive(Deserialize)]
struct TokenRequest {
    name: String,
    /// `"2027-01-01"`, or nothing for a token that does not expire.
    #[serde(default)]
    expires: Option<String>,
}

#[derive(Serialize)]
struct NewToken {
    /// Shown once: the bridge keeps only its hash.
    token: String,
}

async fn issue_token(
    state: &AppState,
    by: &str,
    target: &str,
    req: TokenRequest,
    origin: &Origin,
) -> ApiResult<Json<NewToken>> {
    let expires_at = expiry(req.expires.as_deref())?;
    let token = state.accounts.issue_token(target, &req.name, expires_at).await?;
    state.audit.record(
        by,
        origin,
        Event::TokenIssued {
            target: target.to_string(),
            token: req.name.trim().to_string(),
            expires_at,
        },
    );
    Ok(Json(NewToken { token }))
}

async fn revoke_token(
    state: &AppState,
    by: &str,
    target: &str,
    token: &str,
    origin: &Origin,
) -> ApiResult<axum::http::StatusCode> {
    state.accounts.revoke_token(target, token).await?;
    state.audit.record(
        by,
        origin,
        Event::TokenRevoked {
            target: target.to_string(),
            token: token.to_string(),
        },
    );
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// A read-only account's own MCP token (ADR 15).
async fn issue_own_token(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TokenRequest>,
) -> ApiResult<Json<NewToken>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    refuse_locked(&state, &account, &origin, "token_issue")?;
    issue_token(&state, &account, &account, req, &origin).await
}

async fn revoke_own_token(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    refuse_locked(&state, &account, &origin, "token_revoke")?;
    revoke_token(&state, &account, &account, &token, &origin).await
}

// ---- administration ----------------------------------------------------

async fn list_accounts(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<AccountView>>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "accounts").await?;
    Ok(Json(state.accounts.list()))
}

/// `null` and absent are different answers: absent leaves the rules as
/// they are, `null` clears them (every shared entry).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct UpdateRequest {
    #[serde(default)]
    role: Option<Role>,
    #[serde(default, deserialize_with = "present")]
    servers: Option<Option<Vec<String>>>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    locked: Option<bool>,
}

async fn update_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<UpdateRequest>,
) -> ApiResult<Json<AccountView>> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "account_update").await?;
    let changed = state
        .accounts
        .update(
            &name,
            AccountChange {
                role: req.role,
                servers: req.servers,
                disabled: req.disabled,
                locked: req.locked,
            },
        )
        .await?;
    if !changed.is_empty() {
        let changed: BTreeMap<String, Change> = changed
            .into_iter()
            .map(|(setting, from, to)| {
                (
                    setting.to_string(),
                    Change {
                        from: from.into(),
                        to: to.into(),
                    },
                )
            })
            .collect();
        state.audit.record(
            &admin,
            &origin,
            Event::AccountUpdated {
                target: name.clone(),
                changed,
            },
        );
    }
    state.accounts.view(&name).map(Json).ok_or(ApiError::NotFound(name))
}

/// What happens to what the account kept to itself (ADR 17).
#[derive(Deserialize)]
struct HandoverRequest {
    private: Disposition,
}

/// What `name` keeps to itself, for the question its deletion asks.
async fn owned_of(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<Owned>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "accounts").await?;
    if !state.accounts.exists(&name) {
        return Err(AccountError::NotFound.into());
    }
    Ok(Json(handover::owned_by(&state, &name).await?))
}

/// Delete `name`, and settle what it kept to itself as the request says —
/// handed to the administrator deleting it when it says nothing, so nothing
/// is left that nobody can see, and nothing a password was saved in is
/// shared without someone choosing that.
async fn delete_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: Option<Json<HandoverRequest>>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "account_delete").await?;
    // Refused before anything is handed over: a deletion that then fails
    // must not have moved the account's entries first.
    if !state.accounts.exists(&name) {
        return Err(AccountError::NotFound.into());
    }
    if state.accounts.is_admin(&name) && state.accounts.admin_count() <= 1 {
        return Err(AccountError::LastAdmin.into());
    }
    let private = body
        .map(|Json(request)| request.private)
        .unwrap_or_else(|| Disposition::HandTo { account: admin.clone() });
    let owned = handover::settle(&state, &admin, &origin, &name, &private).await?;
    state.accounts.delete(&name).await?;
    // What it kept for itself alone — its favorites, its settings — goes
    // with it; what it wrote for others, its notes on shared keys, stays.
    if let Err(e) = state.page_rows.forget(name.clone()).await {
        tracing::error!(error = %e, account = name, "the deleted account's own rows could not be removed");
    }
    state.audit.record(
        &admin,
        &origin,
        Event::AccountDeleted {
            target: name.clone(),
            private: (!owned.is_empty()).then(|| owned.handover(&private)),
        },
    );
    tracing::info!(account = name, by = admin, "account deleted");
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct Orphans {
    owner: String,
    #[serde(flatten)]
    owned: Owned,
}

/// What accounts deleted before handovers existed left behind: owned by a
/// name that is no account, so seen by nobody.
async fn list_orphans(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Orphans>>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "accounts").await?;
    let orphans = handover::orphans(&state).await?;
    Ok(Json(
        orphans
            .into_iter()
            .map(|(owner, owned)| Orphans { owner, owned })
            .collect(),
    ))
}

async fn settle_orphans(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(owner): Path<String>,
    Json(request): Json<HandoverRequest>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "orphans_settle").await?;
    // An account's own is settled by deleting the account, never from here.
    if state.accounts.exists(&owner) {
        return Err(ApiError::BadRequest(format!(
            "{owner} is an account; what it keeps is its own"
        )));
    }
    let owned = handover::settle(&state, &admin, &origin, &owner, &request.private).await?;
    if owned.is_empty() {
        return Err(ApiError::NotFound(format!("nothing is left by {owner}")));
    }
    state.audit.record(
        &admin,
        &origin,
        Event::OrphansSettled {
            owner,
            private: owned.handover(&request.private),
        },
    );
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DaysRequest {
    days: Option<u64>,
}

#[derive(Serialize)]
struct NewInvite {
    #[serde(flatten)]
    invite: InviteView,
    /// Shown once: what the link carries. The bridge keeps only its hash.
    code: String,
}

/// A link that sets a new password on `name` — an administrator never types
/// anyone's password.
async fn reset_link(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: Option<Json<DaysRequest>>,
) -> ApiResult<Json<NewInvite>> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "password_reset").await?;
    let days = body.and_then(|Json(b)| b.days).unwrap_or(INVITE_DAYS);
    let (invite, code) = state
        .accounts
        .invite_new(&admin, InviteKind::Reset { account: name.clone() }, days)
        .await?;
    state.audit.record(
        &admin,
        &origin,
        Event::InviteCreated {
            invite: invite.id.clone(),
            kind: "reset",
            role: None,
            target: Some(name),
            proxy: false,
            expires_at: invite.expires_at,
        },
    );
    Ok(Json(NewInvite { invite, code }))
}

async fn issue_token_for(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<TokenRequest>,
) -> ApiResult<Json<NewToken>> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "token_issue").await?;
    issue_token(&state, &admin, &name, req, &origin).await
}

async fn revoke_token_of(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((name, token)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "token_revoke").await?;
    revoke_token(&state, &admin, &name, &token, &origin).await
}

/// The audit, newest first, a page at a time, for an administrator
/// (ADR 18): narrowed by time, account, event and server. Reading it is not
/// itself a line — the log is of the door, and this is not a door.
async fn audit_lines(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<AuditPage>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "audit").await?;
    let Some(log) = state.audit.log() else {
        return Err(ApiError::NotFound(
            "this bridge keeps no audit: start it with --audit-log, or on a database".to_string(),
        ));
    };
    log.query(query)
        .await
        .map(Json)
        .map_err(|e| ApiError::Upstream(format!("the audit could not be read: {e}")))
}

async fn list_invites(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<InviteView>>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "invites").await?;
    Ok(Json(state.accounts.invites()))
}

#[derive(Deserialize)]
struct InviteRequest {
    role: Role,
    #[serde(default)]
    servers: Option<Vec<String>>,
    #[serde(default)]
    proxy: bool,
    #[serde(default)]
    days: Option<u64>,
}

async fn create_invite(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<InviteRequest>,
) -> ApiResult<Json<NewInvite>> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "invite_create").await?;
    if req.proxy && state.trusted.is_none() {
        return Err(ApiError::BadRequest(
            "no --trusted-header is configured: nothing would assert the name".to_string(),
        ));
    }
    let kind = InviteKind::Register {
        role: req.role,
        servers: req.servers,
        proxy: req.proxy,
    };
    let (invite, code) = state
        .accounts
        .invite_new(&admin, kind, req.days.unwrap_or(INVITE_DAYS))
        .await?;
    state.audit.record(
        &admin,
        &origin,
        Event::InviteCreated {
            invite: invite.id.clone(),
            kind: "register",
            role: Some(req.role.name()),
            target: None,
            proxy: req.proxy,
            expires_at: invite.expires_at,
        },
    );
    Ok(Json(NewInvite { invite, code }))
}

async fn revoke_invite(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "invite_revoke").await?;
    state.accounts.invite_revoke(&id).await?;
    state.audit.record(&admin, &origin, Event::InviteRevoked { invite: id });
    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn list_signups(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<SignupView>>> {
    authorize_admin(&state, &headers, &mut Origin::new(peer, &headers), "signups").await?;
    Ok(Json(state.accounts.signups()))
}

#[derive(Deserialize)]
struct ApproveRequest {
    role: Role,
    #[serde(default)]
    servers: Option<Vec<String>>,
}

/// Make the account a request asked for, with the role and rules the
/// administrator gives it and the password it was asked with.
async fn approve_signup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ApproveRequest>,
) -> ApiResult<Json<AccountView>> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "signup_approve").await?;
    let account = state.accounts.approve_signup(&id, req.role, req.servers).await?;
    state.audit.record(
        &admin,
        &origin,
        Event::SignupApproved {
            request: id,
            target: account.name.clone(),
            role: req.role.name(),
        },
    );
    tracing::info!(
        account = account.name,
        role = req.role.name(),
        by = admin,
        "a request for an account was approved"
    );
    Ok(Json(account))
}

async fn reject_signup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let admin = authorize_admin(&state, &headers, &mut origin, "signup_reject").await?;
    let name = state.accounts.reject_signup(&id).await?;
    state.audit.record(
        &admin,
        &origin,
        Event::SignupRejected {
            request: id,
            target: name,
        },
    );
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{Accounts, Signup};
    use crate::api::authorize_mcp;
    use crate::audit::Audit;
    use axum::http::StatusCode;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use std::sync::Arc;

    fn state(dir: &std::path::Path) -> AppState {
        AppState {
            accounts: Accounts::empty(),
            sessions: crate::session::Sessions::new(),
            logins: auth::Logins::new(),
            audit: Audit::off(),
            trusted: None,
            unlocks: crate::policy::Unlocks::new(),
            cookie: auth::CookiePolicy::new(false, ""),
            base_path: String::new(),
            web_root: None,
            mcp_calls: crate::mcp::Limiter::new(),
            presence: crate::presence::Presence::new(),
            watches: crate::watch::Watches::new(),
            servers: Arc::new(crate::store::LocalServers),
            trash: Arc::new(crate::trash::LocalTrash::new(dir.join("bridge-trash"))),
            metrics: Arc::new(crate::metrics::LocalMetrics::new(dir.join("bridge-metrics"))),
            page_rows: Arc::new(crate::page_data::LocalPageRows::new(dir.join("rows"))),
            key_notes: Arc::new(crate::page_data::LocalKeyNotes::new(dir.join("notes"))),
            scripts: Arc::new(crate::page_data::LocalScripts::new(dir.join("scripts.json"))),
            throttle: auth::Throttle::new(),
            setup: crate::accounts::Setup::in_dir(dir),
            signup: Signup::Off,
            store_probe: None,
        }
    }

    fn peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo("10.0.0.7:5000".parse().expect("address"))
    }

    fn basic(pair: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let value = format!("Basic {}", B64.encode(pair));
        headers.insert("authorization", value.parse().expect("header"));
        headers
    }

    /// The cookie a sign-in answered with, as the next request carries it.
    fn cookie_of(response: &Response) -> HeaderMap {
        let set = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .expect("a login cookie");
        let pair = set.split(';').next().expect("name=value");
        let mut headers = HeaderMap::new();
        headers.insert("cookie", pair.parse().expect("header"));
        headers
    }

    fn status(result: ApiResult<impl IntoResponse>) -> StatusCode {
        match result {
            Ok(answer) => answer.into_response().status(),
            Err(e) => e.into_response().status(),
        }
    }

    /// The whole of ADR 17's door, through the routes: the setup token seats
    /// the administrator once; an invitation registers an account once, with
    /// what it says; a non-administrator is refused administration; a
    /// read-only account issues itself a token the MCP entry point takes; the
    /// last administrator stays; a reset link replaces a password, the old one
    /// failing at once; and guessing a code is turned away.
    #[tokio::test]
    async fn setup_invitation_registration_and_administration() {
        let dir = std::env::temp_dir().join(format!("zedis-account-api-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("dir");
        let state = state(&dir);
        let token = state.setup.mint().await.expect("token");
        assert!(setup_state(State(state.clone())).await.open);

        let seat = |token: &str, name: &str| {
            setup(
                State(state.clone()),
                peer(),
                HeaderMap::new(),
                Json(SetupRequest {
                    token: token.to_string(),
                    name: name.to_string(),
                    password: "root-password".to_string(),
                    remember: false,
                }),
            )
        };
        assert_eq!(status(seat("not-the-token", "root").await), StatusCode::FORBIDDEN);
        let Ok(seated) = seat(&token, "root").await else {
            panic!("seated");
        };
        assert!(state.accounts.is_admin("root"));
        let root_cookie = cookie_of(&seated);
        assert!(!setup_state(State(state.clone())).await.open, "closed once used");
        assert_eq!(status(seat(&token, "mallory").await), StatusCode::NOT_FOUND);
        let Ok(Json(me)) = own_account(State(state.clone()), peer(), root_cookie.clone()).await else {
            panic!("the cookie signs in");
        };
        assert_eq!(me.account.role, Role::Admin);

        let root = basic("root:root-password");
        let Ok(Json(invite)) = create_invite(
            State(state.clone()),
            peer(),
            root.clone(),
            Json(InviteRequest {
                role: Role::ReadOnly,
                servers: None,
                proxy: false,
                days: None,
            }),
        )
        .await
        else {
            panic!("invited");
        };
        let Ok(Json(info)) = invitation(
            State(state.clone()),
            peer(),
            HeaderMap::new(),
            Json(CodeRequest {
                code: invite.code.clone(),
            }),
        )
        .await
        else {
            panic!("the invitation is open");
        };
        assert_eq!(info.invite.id, invite.invite.id);
        let join = |code: &str, name: &str, password: &str| {
            register(
                State(state.clone()),
                peer(),
                HeaderMap::new(),
                Json(RegisterRequest {
                    code: code.to_string(),
                    name: name.to_string(),
                    password: password.to_string(),
                    remember: true,
                }),
            )
        };
        assert_eq!(status(join(&invite.code, "ai", "short").await), StatusCode::BAD_REQUEST);
        let Ok(joined) = join(&invite.code, "ai", "ai-password").await else {
            panic!("registered");
        };
        assert_eq!(
            status(join(&invite.code, "eve", "eve-password").await),
            StatusCode::BAD_REQUEST,
            "used"
        );
        let ai = cookie_of(&joined);
        assert!(state.accounts.is_read_only("ai"));

        assert_eq!(
            status(list_accounts(State(state.clone()), peer(), ai.clone()).await),
            StatusCode::FORBIDDEN
        );
        let Ok(Json(accounts)) = list_accounts(State(state.clone()), peer(), root.clone()).await else {
            panic!("listed");
        };
        assert_eq!(
            accounts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["ai", "root"]
        );

        let Ok(Json(issued)) = issue_own_token(
            State(state.clone()),
            peer(),
            ai.clone(),
            Json(TokenRequest {
                name: "claude".to_string(),
                expires: None,
            }),
        )
        .await
        else {
            panic!("issued");
        };
        let mut bearer = HeaderMap::new();
        let value = format!("Bearer {}", issued.token);
        bearer.insert("authorization", value.parse().expect("header"));
        let mut origin = Origin::default();
        assert_eq!(
            authorize_mcp(&state, &bearer, &mut origin).await.ok().as_deref(),
            Some("ai")
        );

        let demote = update_account(
            State(state.clone()),
            peer(),
            root.clone(),
            Path("root".to_string()),
            Json(UpdateRequest {
                role: Some(Role::Full),
                servers: None,
                disabled: None,
                locked: None,
            }),
        )
        .await;
        assert_eq!(status(demote), StatusCode::CONFLICT, "the last administrator stays");

        let Ok(Json(reset)) =
            reset_link(State(state.clone()), peer(), root.clone(), Path("ai".to_string()), None).await
        else {
            panic!("a reset link");
        };
        assert!(join(&reset.code, "", "ai-password-2").await.is_ok());
        assert_eq!(
            status(own_account(State(state.clone()), peer(), ai.clone()).await),
            StatusCode::UNAUTHORIZED,
            "the old login ended"
        );
        assert_eq!(
            status(own_account(State(state.clone()), peer(), basic("ai:ai-password")).await),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            own_account(State(state.clone()), peer(), basic("ai:ai-password-2"))
                .await
                .is_ok()
        );

        for _ in 0..auth::THROTTLE_LIMIT {
            let wrong = invitation(
                State(state.clone()),
                peer(),
                HeaderMap::new(),
                Json(CodeRequest {
                    code: "guess".to_string(),
                }),
            )
            .await;
            assert_eq!(status(wrong), StatusCode::BAD_REQUEST);
        }
        let blocked = invitation(
            State(state.clone()),
            peer(),
            HeaderMap::new(),
            Json(CodeRequest {
                code: "guess".to_string(),
            }),
        )
        .await;
        assert_eq!(status(blocked), StatusCode::TOO_MANY_REQUESTS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The handover tests' accounts: alice administers, bob and carol write,
    /// ro reads. The server list is the process's, shared by every test, so
    /// each test's names are its own.
    struct Names {
        bob: String,
        carol: String,
        ro: String,
    }

    fn handover_state(dir: &std::path::Path) -> (AppState, Names) {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let names = Names {
            bob: format!("bob-{tag}"),
            carol: format!("carol-{tag}"),
            ro: format!("ro-{tag}"),
        };
        let inline = format!(
            "{}@bob-password,{}@carol-password,{}:ro@ro-password",
            names.bob, names.carol, names.ro
        );
        let state = AppState {
            accounts: Accounts::from_inline(&inline).expect("accounts"),
            audit: Audit::open(&dir.join("audit.log"), false).expect("audit"),
            ..state(dir)
        };
        (state, names)
    }

    /// An administrator reads the audit back, narrowed; any other account
    /// is refused, and a bridge that keeps none says so (ADR 18).
    #[tokio::test]
    async fn an_administrator_reads_the_audit() {
        let dir = std::env::temp_dir().join(format!("zedis-audit-route-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("dir");
        let (state, names) = handover_state(&dir);
        seat_alice(&state).await;
        let read = |who: &str, query: AuditQuery| audit_lines(State(state.clone()), peer(), basic(who), Query(query));
        let Ok(Json(page)) = read("alice:alice-password", AuditQuery::default()).await else {
            panic!("an administrator reads the audit");
        };
        let events: Vec<String> = page
            .lines
            .iter()
            .map(|line| serde_json::from_str::<serde_json::Value>(line.get()).expect("json")["event"].to_string())
            .collect();
        assert!(events.contains(&"\"setup_completed\"".to_string()), "{events:?}");
        let narrowed = read(
            "alice:alice-password",
            AuditQuery {
                event: Some("nothing_is_called_this".to_string()),
                ..AuditQuery::default()
            },
        )
        .await;
        assert!(matches!(narrowed, Ok(Json(page)) if page.lines.is_empty()));

        let bob = format!("{}:bob-password", names.bob);
        assert!(matches!(
            read(&bob, AuditQuery::default()).await,
            Err(ApiError::Forbidden(_))
        ));
        let silent = AppState {
            audit: crate::audit::Audit::off(),
            ..state.clone()
        };
        let none = audit_lines(
            State(silent),
            peer(),
            basic("alice:alice-password"),
            Query(AuditQuery::default()),
        )
        .await;
        assert!(matches!(none, Err(ApiError::NotFound(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn seat_alice(state: &AppState) {
        let token = state.setup.mint().await.expect("token");
        let seated = setup(
            State(state.clone()),
            peer(),
            HeaderMap::new(),
            Json(SetupRequest {
                token,
                name: "alice".to_string(),
                password: "alice-password".to_string(),
                remember: false,
            }),
        )
        .await;
        assert!(seated.is_ok(), "alice administers");
    }

    /// A server entry in the bridge's list, `owner`'s, or shared.
    async fn entry(owner: Option<&str>) -> zedis_connection::RedisServer {
        let server = zedis_connection::RedisServer {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: "handover".to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            owner: owner.map(str::to_string),
            ..Default::default()
        };
        crate::store::ServerStore::put(&crate::store::LocalServers, server.clone())
            .await
            .expect("server");
        server
    }

    async fn script(state: &AppState, id: &str, owner: Option<&str>) {
        let record = crate::page_data::ScriptRecord {
            id: id.to_string(),
            owner: owner.map(str::to_string),
            script: serde_json::value::RawValue::from_string(format!(r#"{{"name":"{id}"}}"#)).expect("json"),
            updated_by: owner.unwrap_or("alice").to_string(),
            updated_at: 1,
        };
        state.scripts.put(record).await.expect("script");
    }

    fn owner_of(id: &str) -> Option<Option<String>> {
        zedis_connection::get_server(id).ok().map(|server| server.owner)
    }

    async fn script_owner(state: &AppState, id: &str) -> Option<Option<String>> {
        let scripts = state.scripts.list().await.expect("scripts");
        scripts.into_iter().find(|s| s.id == id).map(|s| s.owner)
    }

    async fn delete(state: &AppState, name: &str, private: Option<Disposition>) -> StatusCode {
        status(
            delete_account(
                State(state.clone()),
                peer(),
                basic("alice:alice-password"),
                Path(name.to_string()),
                private.map(|private| Json(HandoverRequest { private })),
            )
            .await,
        )
    }

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zedis-handover-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    /// Deleting an account hands what it kept to itself to the administrator
    /// who deletes it, unless asked otherwise — and says so in the audit.
    #[tokio::test]
    async fn a_deleted_account_s_own_go_to_the_administrator_by_default() {
        let dir = scratch_dir();
        let (state, names) = handover_state(&dir);
        seat_alice(&state).await;
        let bobs = entry(Some(&names.bob)).await;
        let shared = entry(None).await;
        script(&state, "bob-private", Some(&names.bob)).await;
        script(&state, "everyone", None).await;

        let Ok(Json(owned)) = owned_of(
            State(state.clone()),
            peer(),
            basic("alice:alice-password"),
            Path(names.bob.clone()),
        )
        .await
        else {
            panic!("bob's own");
        };
        assert_eq!(
            owned.servers,
            vec![handover::OwnedItem {
                id: bobs.id.clone(),
                name: "handover".to_string(),
            }]
        );
        assert_eq!(owned.scripts.len(), 1);

        assert_eq!(delete(&state, &names.bob, None).await, StatusCode::NO_CONTENT);
        assert!(!state.accounts.exists(&names.bob));
        assert_eq!(owner_of(&bobs.id), Some(Some("alice".to_string())));
        assert_eq!(owner_of(&shared.id), Some(None), "what was everyone's stays so");
        assert_eq!(
            script_owner(&state, "bob-private").await,
            Some(Some("alice".to_string()))
        );
        assert_eq!(script_owner(&state, "everyone").await, Some(None));

        let log = std::fs::read_to_string(dir.join("audit.log")).expect("audit");
        let deleted = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json"))
            .find(|line| line["event"] == "account_deleted")
            .expect("an account_deleted line");
        assert_eq!(deleted["private"]["action"], "handed_to");
        assert_eq!(deleted["private"]["to"], "alice");
        assert_eq!(deleted["private"]["servers"], 1);
        assert!(
            log.contains(&format!(r#""owner":{{"from":"{}","to":"alice"}}"#, names.bob)),
            "the entry's owner change"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Shared on purpose, deleted on purpose — and a handover that cannot
    /// happen leaves the account and everything it had where they were.
    #[tokio::test]
    async fn sharing_deleting_and_a_refused_handover() {
        let dir = scratch_dir();
        let (state, names) = handover_state(&dir);
        seat_alice(&state).await;
        let bobs = entry(Some(&names.bob)).await;
        let carols = entry(Some(&names.carol)).await;
        script(&state, "carol-private", Some(&names.carol)).await;

        let to_ro = Disposition::HandTo {
            account: names.ro.clone(),
        };
        assert_eq!(delete(&state, &names.bob, Some(to_ro)).await, StatusCode::BAD_REQUEST);
        assert!(state.accounts.exists(&names.bob), "nothing happened");
        assert_eq!(owner_of(&bobs.id), Some(Some(names.bob.clone())));

        assert_eq!(
            delete(&state, &names.bob, Some(Disposition::Share)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(owner_of(&bobs.id), Some(None), "everyone's now");

        assert_eq!(
            delete(&state, &names.carol, Some(Disposition::Delete)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(owner_of(&carols.id), None, "gone with the account");
        assert_eq!(script_owner(&state, "carol-private").await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What an account deleted before handovers left is listed, and settled
    /// the same way; an account's own is not an orphan's.
    #[tokio::test]
    async fn orphans_are_listed_and_settled() {
        let dir = scratch_dir();
        let (state, names) = handover_state(&dir);
        seat_alice(&state).await;
        let ghost = format!("ghost-{}", uuid::Uuid::new_v4().simple());
        let left = entry(Some(&ghost)).await;
        let bobs = entry(Some(&names.bob)).await;

        let Ok(Json(orphans)) = list_orphans(State(state.clone()), peer(), basic("alice:alice-password")).await else {
            panic!("orphans");
        };
        let found = orphans.iter().find(|o| o.owner == ghost).expect("the ghost's entry");
        assert_eq!(found.owned.servers[0].id, left.id);
        assert!(orphans.iter().all(|o| o.owner != names.bob), "an account is no orphan");

        let settle = |owner: String, private: Disposition| {
            settle_orphans(
                State(state.clone()),
                peer(),
                basic("alice:alice-password"),
                Path(owner),
                Json(HandoverRequest { private }),
            )
        };
        assert_eq!(
            status(settle(names.bob.clone(), Disposition::Share).await),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(owner_of(&bobs.id), Some(Some(names.bob.clone())));
        let to_carol = Disposition::HandTo {
            account: names.carol.clone(),
        };
        assert_eq!(status(settle(ghost.clone(), to_carol).await), StatusCode::NO_CONTENT);
        assert_eq!(owner_of(&left.id), Some(Some(names.carol.clone())));
        let log = std::fs::read_to_string(dir.join("audit.log")).expect("audit");
        assert!(log.contains(r#""event":"orphans_settled""#));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The second door (ADR 17, amended 2026-10-18), through the routes: shut
    /// unless opened; a request answered the same whatever the name; listed
    /// and answered by an administrator only; approved into an account that
    /// signs in with what it asked; and every step an audit line.
    #[tokio::test]
    async fn a_request_for_an_account_waits_for_an_administrator() {
        let dir = scratch_dir();
        let mut state = AppState {
            audit: Audit::open(&dir.join("audit.log"), false).expect("audit"),
            ..state(&dir)
        };
        seat_alice(&state).await;
        let ask = |state: &AppState, name: &str| {
            signup(
                State(state.clone()),
                peer(),
                HeaderMap::new(),
                Json(SignupRequest {
                    name: name.to_string(),
                    password: format!("{name}-password"),
                    note: "for the release".to_string(),
                }),
            )
        };
        assert_eq!(
            status(ask(&state, "dana").await),
            StatusCode::NOT_FOUND,
            "shut by default"
        );
        assert!(!setup_state(State(state.clone())).await.signup);

        state.signup = Signup::Approval;
        assert!(setup_state(State(state.clone())).await.signup);
        assert_eq!(status(ask(&state, "dana").await), StatusCode::ACCEPTED);
        assert_eq!(
            status(ask(&state, "alice").await),
            StatusCode::ACCEPTED,
            "a taken name is not told to whoever asks"
        );
        assert_eq!(status(ask(&state, "a/b").await), StatusCode::BAD_REQUEST);
        assert!(!state.accounts.has("dana"));
        assert_eq!(
            status(own_account(State(state.clone()), peer(), basic("dana:dana-password")).await),
            StatusCode::UNAUTHORIZED,
            "a request signs nobody in"
        );

        let admin = || basic("alice:alice-password");
        let Ok(Json(me)) = own_account(State(state.clone()), peer(), admin()).await else {
            panic!("alice");
        };
        assert!(me.signup, "the page has requests to answer");
        let Ok(Json(waiting)) = list_signups(State(state.clone()), peer(), admin()).await else {
            panic!("listed");
        };
        assert_eq!(waiting.len(), 2);
        let dana = waiting.iter().find(|r| r.name == "dana").expect("dana's");
        let alices = waiting.iter().find(|r| r.name == "alice").expect("the one for alice");
        assert!(alices.name_taken && !dana.name_taken);
        assert_eq!(dana.note, "for the release");

        let approve = |id: &str, role: Role| {
            approve_signup(
                State(state.clone()),
                peer(),
                admin(),
                Path(id.to_string()),
                Json(ApproveRequest {
                    role,
                    servers: Some(vec!["staging".to_string()]),
                }),
            )
        };
        assert_eq!(
            status(approve(&alices.id, Role::Full).await),
            StatusCode::CONFLICT,
            "an account has that name"
        );
        let Ok(Json(made)) = approve(&dana.id, Role::ReadOnly).await else {
            panic!("approved");
        };
        assert_eq!((made.name.as_str(), made.role), ("dana", Role::ReadOnly));
        let Ok(Json(own)) = own_account(State(state.clone()), peer(), basic("dana:dana-password")).await else {
            panic!("dana signs in with the password she asked with");
        };
        assert_eq!(own.account.servers, Some(vec!["staging".to_string()]));
        assert_eq!(
            status(list_signups(State(state.clone()), peer(), basic("dana:dana-password")).await),
            StatusCode::FORBIDDEN,
            "administrators only"
        );
        assert_eq!(
            status(reject_signup(State(state.clone()), peer(), admin(), Path(alices.id.clone())).await),
            StatusCode::NO_CONTENT
        );
        let Ok(Json(waiting)) = list_signups(State(state.clone()), peer(), admin()).await else {
            panic!("listed");
        };
        assert!(waiting.is_empty());

        // Each request counts against its address: the throttle's ten.
        let mut refused = None;
        for n in 0..auth::THROTTLE_LIMIT {
            let answer = status(ask(&state, &format!("user{n}")).await);
            if answer == StatusCode::TOO_MANY_REQUESTS {
                refused = Some(n);
                break;
            }
        }
        assert!(refused.is_some(), "an address cannot fill the list");

        let log = std::fs::read_to_string(dir.join("audit.log")).expect("audit");
        for event in ["signup_requested", "signup_approved", "signup_rejected"] {
            assert!(log.contains(&format!(r#""event":"{event}""#)), "{event}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A locked account — a demonstration's, which many people sign in to —
    /// changes neither its password nor its tokens; an administrator still
    /// does, an administrator cannot be locked, and every refusal is an
    /// audit line.
    #[tokio::test]
    async fn a_locked_account_changes_nothing_of_its_own() {
        let dir = scratch_dir();
        let state = AppState {
            accounts: Accounts::from_inline("demo:ro@demo-password").expect("accounts"),
            audit: Audit::open(&dir.join("audit.log"), false).expect("audit"),
            ..state(&dir)
        };
        seat_alice(&state).await;
        let admin = || basic("alice:alice-password");
        let demo = || basic("demo:demo-password");
        let lock = |name: &str, locked: bool| {
            update_account(
                State(state.clone()),
                peer(),
                admin(),
                Path(name.to_string()),
                Json(UpdateRequest {
                    role: None,
                    servers: None,
                    disabled: None,
                    locked: Some(locked),
                }),
            )
        };
        let Ok(Json(view)) = lock("demo", true).await else {
            panic!("locked");
        };
        assert!(view.locked);
        let Ok(Json(me)) = own_account(State(state.clone()), peer(), demo()).await else {
            panic!("it still signs in");
        };
        assert!(me.account.locked, "the page knows");

        let changed = change_password(
            State(state.clone()),
            peer(),
            demo(),
            Json(PasswordRequest {
                current: "demo-password".to_string(),
                new: "mine-now-1".to_string(),
            }),
        )
        .await;
        assert_eq!(status(changed), StatusCode::FORBIDDEN);
        assert!(state.accounts.accepts_password("demo", "demo-password"), "unchanged");
        let token = |name: &str| TokenRequest {
            name: name.to_string(),
            expires: None,
        };
        assert_eq!(
            status(issue_own_token(State(state.clone()), peer(), demo(), Json(token("laptop"))).await),
            StatusCode::FORBIDDEN
        );
        let issued = issue_token_for(
            State(state.clone()),
            peer(),
            admin(),
            Path("demo".to_string()),
            Json(token("claude")),
        )
        .await;
        assert!(issued.is_ok(), "an administrator still issues its tokens");
        assert_eq!(
            status(revoke_own_token(State(state.clone()), peer(), demo(), Path("claude".to_string())).await),
            StatusCode::FORBIDDEN,
            "a visitor cannot take another's token away"
        );
        assert_eq!(status(lock("alice", true).await), StatusCode::BAD_REQUEST);

        let log = std::fs::read_to_string(dir.join("audit.log")).expect("audit");
        for action in ["password_change", "token_issue", "token_revoke"] {
            assert!(log.contains(&format!(r#""action":"{action}""#)), "{action}");
        }
        assert!(log.contains(r#""locked":{"from":"false","to":"true"}"#), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
