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

//! The server list: what an account sees of it, adding, editing and deleting
//! entries, the write lock's window, and diagnosing a saved entry.

use super::*;

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

/// One entry as a caller sees it: every setting, no secret.
#[derive(Serialize)]
pub(super) struct ServerEntry {
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

/// Only the entries the caller may see: its own, and the shared ones its
/// rules admit. `owner` comes back as stored — the caller's own name or
/// nothing — which is how the page knows whether its form's *shared* box
/// starts ticked.
pub(super) async fn servers(
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
pub(super) struct AddServerRequest {
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
pub(super) struct AddServerResponse {
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
pub(super) async fn add_server(
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
    // Adding never overwrites. An id that is already taken gets a fresh one,
    // whoever's entry holds it: an edit is a PUT, which asks whether this
    // account may write that entry, and a POST that kept a visible id went
    // around the check — and around the dial-or-roll-back, whose roll-back
    // for a *new* entry deleted the one it had replaced. Taken by an entry
    // the caller cannot see looks the same, so the reply never confirms that
    // one exists. An id that could not travel as one path segment is
    // replaced too: the page builds `/v1/servers/{id}` from it, and a
    // `../servers/<other>` there would aim another account's edit elsewhere.
    if !is_plain_id(&server.id) || get_server(&server.id).is_ok() {
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
    let result = save_and_dial(&state.servers, server, None).await;
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

/// Whether `id` can name an entry as it is: non-empty, short, and made of
/// the characters a UUID or a hand-written slug uses, so it is one URL path
/// segment as written.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Put `server` into the shared list and prove it reachable, or put the list
/// back the way it was: `previous` is the entry it replaces (an edit), `None`
/// when it is new.
///
/// Saving without connecting would let a typo sit in the list looking
/// healthy, and for an edit it would do worse — break an entry that worked.
/// So the dial is part of the request either way, and a failure leaves no
/// trace in a list everyone shares.
async fn save_and_dial(
    servers: &Arc<dyn ServerStore>,
    server: RedisServer,
    previous: Option<RedisServer>,
) -> ApiResult<AddServerResponse> {
    let id = server.id.clone();
    let name = server.name.clone();
    // The pool is keyed by the entry's whole content, so the edited entry
    // gets a client of its own; this drops the one the old content had.
    if previous.is_some() {
        get_connection_manager().remove_client(&id, 0);
    }

    let saved = server.clone();
    servers
        .put(server)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let client = match get_connection_manager().get_client(&id, 0).await {
        Ok(client) => client,
        Err(e) => {
            // Take back this request's entry and nothing else: the dial took
            // seconds, and an edit someone made to the same entry meanwhile
            // is theirs to keep.
            let _ = servers.swap(saved, previous).await;
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
pub(super) struct UpdateServerRequest {
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
    // A kept secret goes only where it went before. The bridge dials an edit
    // before it keeps it, with the stored secrets filled in, so an edit that
    // pointed the host — or the SSH hop in front of it — somewhere new
    // handed the stored password to whoever answers there: anyone who could
    // write a shared entry could read its credentials. Moving an entry is
    // fine; its secrets are typed again for the new address.
    let ssh_route = |s: &RedisServer| (s.is_ssh_tunnel(), s.ssh_addr.clone(), s.ssh_username.clone());
    let ssh_moved = ssh_route(&incoming) != ssh_route(stored);
    let redis_moved = ssh_moved || incoming.host != stored.host || incoming.port != stored.port;
    for name in keep {
        let moved = if name.starts_with("ssh_") {
            ssh_moved
        } else {
            redis_moved
        };
        if moved && RedisServer::SECRET_FIELDS.contains(&name.as_str()) {
            return Err(format!(
                "`{name}` was stored for another address; enter it again to use it with the new one"
            ));
        }
    }
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
pub(super) async fn update_server(
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
    let result = save_and_dial(&state.servers, server, Some(stored)).await;
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
pub(super) async fn delete_server(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "server_delete")?;
    // Someone else's entry is left alone and answered like one already gone;
    // one the account may see but not write to is refused — and nothing is
    // saved, since nothing changed.
    let found = get_server(&id)
        .ok()
        .filter(|s| visible_to(&state.accounts, s, &account));
    if let Some(server) = &found {
        refuse_read_only_on(&state, &account, server, &origin, "server_delete")?;
    }
    // Removed only as it was checked: an entry changed since — made someone
    // else's, say — is left for the next try to judge again.
    let removed = match found {
        Some(server) => {
            let reference = ServerRef::from(&server);
            let gone = state
                .servers
                .swap(server, None)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            if !gone {
                return Err(ApiError::Conflict(
                    "the entry changed while it was being deleted; try again".to_string(),
                ));
            }
            Some(reference)
        }
        None => None,
    };
    // Only a deletion that deleted something: the second of two asks for
    // the same id changed nothing.
    if let Some(server) = removed {
        // What was deleted from it goes with it: nobody can restore into an
        // entry that is not there, and the values were someone's.
        let _ = state.trash.remove_server(server.id.clone()).await;
        let _ = state.metrics.remove_server(server.id.clone()).await;
        let _ = state.key_notes.remove_server(server.id.clone()).await;
        state.audit.record(&account, &origin, Event::ServerDeleted { server });
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct UnlockRequest {
    /// The answer to the lock's question: anything for a plain entry, the
    /// entry's name for production — the same rule `policy::check` applies
    /// to a script that confirms one command.
    confirm: String,
}

#[derive(Serialize)]
pub(super) struct UnlockResponse {
    /// How long the window is, so the page's countdown says what the
    /// bridge will do.
    seconds: u64,
}

/// Open the caller's write window on a locked entry (ADR 14). Refused for
/// an account that may not write there at all, and for an entry that is
/// not locked — there is nothing to open — and asked again, with the
/// strictness, when the confirmation does not satisfy it.
pub(super) async fn unlock_writes(
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
pub(super) async fn lock_writes(
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

#[derive(Serialize)]
pub(super) struct DiagnoseReply {
    stages: Vec<DiagReportRow>,
}

/// Diagnose a saved entry from here (DNS → TCP → SSH → TLS → AUTH → PING):
/// a page cannot, having no socket, and this is the process that dials it —
/// so which stage fails *from the bridge* is the answer the page needs.
///
/// Anyone who sees the entry may ask, read-only accounts too: it connects,
/// as opening the entry does, and changes nothing — which is also why it is
/// not an audit line. It is no reach a full account lacks either, since
/// adding an entry dials whatever it names. Each run dials afresh (a new
/// SSH session, a new connection), as the desktop's does.
pub(super) async fn diagnose_server(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<DiagnoseReply>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    let server = visible_server(&state.accounts, &id, &account)?;
    let mut stages = Vec::new();
    run_diagnostics(&server, |stage, outcome| {
        if let Some(outcome) = outcome {
            stages.push(DiagReportRow::new(stage, &outcome));
        }
        true
    })
    .await;
    for row in &mut stages {
        // What `~/.ssh/config` contributed is this host's file — its users,
        // its keys' paths — and not the entry's to show whoever sees it.
        if row.stage == DiagStage::SshAuth {
            row.detail = None;
        }
    }
    let failed = stages
        .iter()
        .find(|row| row.status == DiagStatus::Failed)
        .map(|row| row.stage);
    tracing::info!(account, server = %server.id, ?failed, "diagnosed");
    Ok(Json(DiagnoseReply { stages }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct KillScriptRequest {
    target: KillTarget,
}

#[derive(Serialize)]
pub(super) struct KillScriptReply {
    replies: Vec<KillReply>,
}

/// Send `SCRIPT KILL` / `FUNCTION KILL` to every data node of `server` over
/// fresh connections that bypass a `BUSY` pool. Refused for an account that
/// may not write on the server, and recorded as an administrative command in
/// the audit log.
pub(super) async fn kill_script(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<KillScriptRequest>,
) -> ApiResult<Json<KillScriptReply>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin)?;
    let server = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &server, &origin, "kill_script")?;
    note_server(&state, &origin, &server.id);
    let command = match req.target {
        KillTarget::Script => "SCRIPT",
        KillTarget::Function => "FUNCTION",
    };
    let result = kill_running(&server, req.target).await;
    state.audit.record(
        &account,
        &origin,
        Event::Command(audit::CommandLine {
            server: ServerRef::from(&server),
            db: 0,
            command: command.to_string(),
            args: vec!["KILL".to_string()],
            count: 1,
            outcome: audit::Outcome::Allowed,
            kind: None,
            confirm: None,
            error: result.as_ref().err().map(ToString::to_string),
        }),
    );
    let replies = result.map_err(|e| ApiError::Upstream(e.to_string()))?;
    tracing::info!(account, server = %server.id, command = req.target.command(), "kill script");
    Ok(Json(KillScriptReply { replies }))
}

#[cfg(test)]
mod id_tests {
    use super::is_plain_id;

    #[test]
    fn only_an_id_that_is_one_path_segment_is_kept() {
        assert!(is_plain_id("0192f3c4-7b1e-7d2a-9c3e-1a2b3c4d5e6f"));
        assert!(is_plain_id("staging_eu-1"));
        assert!(!is_plain_id(""));
        assert!(!is_plain_id("../servers/prod"));
        assert!(!is_plain_id("a/b"));
        assert!(!is_plain_id("a%2Fb"));
        assert!(!is_plain_id(&"x".repeat(129)));
    }
}

#[cfg(test)]
mod server_tests {
    use super::{Accounts, RedisServer, ServerEntry, assign_owner, merge_update, visible_to};

    fn owned_by(owner: Option<&str>) -> RedisServer {
        RedisServer {
            name: "prod-eu".to_string(),
            owner: owner.map(str::to_string),
            ..Default::default()
        }
    }

    fn accounts(toml: &str) -> Accounts {
        Accounts::from_toml(toml).expect("accounts")
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
            host: "10.0.0.5".to_string(),
            port: 6379,
            ..Default::default()
        };
        let merged = merge_update(&stored(), edit, &["password".to_string()]).expect("merge");
        assert_eq!(merged.id, "srv-1", "the path names the entry, not the body");
        assert_eq!(
            (merged.name.as_str(), merged.host.as_str(), merged.port),
            ("prod-renamed", "10.0.0.5", 6379)
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
    fn a_stored_secret_is_never_sent_to_a_new_address() {
        let moved = |edit: RedisServer, keep: &str| merge_update(&stored(), edit, &[keep.to_string()]);
        // Another host or port: the stored password stays behind.
        let elsewhere = RedisServer {
            host: "attacker.example".to_string(),
            port: 6379,
            ..Default::default()
        };
        let err = moved(elsewhere, "password").expect_err("a new host");
        assert!(err.contains("`password`"), "{err}");
        let other_port = RedisServer {
            host: "10.0.0.5".to_string(),
            port: 6380,
            ..Default::default()
        };
        assert!(moved(other_port, "password").is_err(), "a new port");
        // The same host behind a new SSH hop is a new address too.
        let tunnelled = RedisServer {
            host: "10.0.0.5".to_string(),
            port: 6379,
            ssh_tunnel: Some(true),
            ssh_addr: Some("attacker.example:22".to_string()),
            ..Default::default()
        };
        assert!(moved(tunnelled.clone(), "password").is_err(), "a new SSH route");
        assert!(
            moved(tunnelled, "ssh_key").is_err(),
            "the SSH key follows the SSH route"
        );
        // A secret typed again is simply the new one.
        let retyped = RedisServer {
            host: "10.0.0.9".to_string(),
            port: 6379,
            password: Some("typed-again".to_string()),
            ..Default::default()
        };
        let merged = merge_update(&stored(), retyped, &[]).expect("merge");
        assert_eq!(merged.password.as_deref(), Some("typed-again"));
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
mod diagnose_tests {
    use super::*;
    use zedis_core::fs::override_config_dir;

    /// A page asks the bridge to diagnose an entry it sees, and gets every
    /// stage back — here a port nobody listens on: no name to resolve, the
    /// connect refused, and nothing after it tried. Someone else's entry
    /// answers like one that does not exist.
    #[tokio::test]
    async fn the_bridge_diagnoses_an_entry_the_caller_sees_and_no_other() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-bridge-store-{}", std::process::id())));
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = closed.local_addr().expect("address").port();
        drop(closed);
        let entry = |id: &str, owner: Option<&str>| RedisServer {
            id: id.to_string(),
            name: id.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            owner: owner.map(str::to_string),
            ..Default::default()
        };
        crate::store::LocalServers
            .put(entry("diag-shared", None))
            .await
            .expect("server");
        crate::store::LocalServers
            .put(entry("diag-bobs", Some("bob")))
            .await
            .expect("server");
        let state = crate::api::mcp_auth_tests::state(
            "[[users]]\nname = \"viewer\"\npassword = \"look\"\nread_only = true\n\n\
             [[users]]\nname = \"bob\"\npassword = \"bobs-password\"",
        );
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("address");
        let mut headers = HeaderMap::new();
        let basic = format!("Basic {}", B64.encode("viewer:look"));
        headers.insert("authorization", basic.parse().expect("header"));
        let diagnose = |id: &str| {
            diagnose_server(
                State(state.clone()),
                ConnectInfo(peer),
                headers.clone(),
                Path(id.to_string()),
            )
        };

        let Ok(Json(reply)) = diagnose("diag-shared").await else {
            panic!("a read-only account may diagnose what it sees");
        };
        let seen: Vec<(DiagStage, DiagStatus)> = reply.stages.iter().map(|row| (row.stage, row.status)).collect();
        assert_eq!(
            seen,
            [
                (DiagStage::Dns, DiagStatus::Skipped),
                (DiagStage::Tcp, DiagStatus::Failed),
                (DiagStage::Auth, DiagStatus::Skipped),
                (DiagStage::Ping, DiagStatus::Skipped),
            ]
        );
        assert!(reply.stages[1].error.is_some(), "the refusal is said");

        assert!(matches!(diagnose("diag-bobs").await, Err(ApiError::UnknownServer(_))));
    }

    #[tokio::test]
    async fn kill_script_requires_write_access_and_dials_visible_entry() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-bridge-kill-{}", std::process::id())));
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = closed.local_addr().expect("address").port();
        drop(closed);
        let entry = |id: &str, owner: Option<&str>| RedisServer {
            id: id.to_string(),
            name: id.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            owner: owner.map(str::to_string),
            ..Default::default()
        };
        crate::store::LocalServers
            .put(entry("kill-shared", None))
            .await
            .expect("server");
        crate::store::LocalServers
            .put(entry("kill-bobs", Some("bob")))
            .await
            .expect("server");
        let state = crate::api::mcp_auth_tests::state(
            "[[users]]\nname = \"viewer\"\npassword = \"look\"\nread_only = true\n\n\
             [[users]]\nname = \"bob\"\npassword = \"bobs-password\"",
        );
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("address");
        let call = |auth_pair: &str, id: &str| {
            let mut headers = HeaderMap::new();
            let basic = format!("Basic {}", B64.encode(auth_pair));
            headers.insert("authorization", basic.parse().expect("header"));
            kill_script(
                State(state.clone()),
                ConnectInfo(peer),
                headers,
                Path(id.to_string()),
                Json(KillScriptRequest {
                    target: KillTarget::Script,
                }),
            )
        };

        assert!(matches!(
            call("viewer:look", "kill-shared").await,
            Err(ApiError::Forbidden(_))
        ));
        assert!(matches!(
            call("viewer:look", "kill-bobs").await,
            Err(ApiError::UnknownServer(_))
        ));

        let Ok(Json(reply)) = call("bob:bobs-password", "kill-shared").await else {
            panic!("a write-capable account may send SCRIPT KILL to an entry it sees");
        };
        assert_eq!(reply.replies.len(), 1);
        assert!(matches!(
            reply.replies[0].outcome,
            zedis_connection::KillOutcome::Failed(_)
        ));
    }
}
