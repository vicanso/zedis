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

//! What becomes of the server entries and Lua scripts an account kept to
//! itself when the account goes (ADR 17): handed to another account, shared
//! with every account, or deleted with it — the administrator's choice, made
//! in the deletion, so nothing is left behind that nobody can see.
//!
//! Sharing is never the default. A private entry carries its owner's Redis
//! password, and sharing it lets every account use that connection — a
//! choice to make on purpose. The default is the administrator who deletes.
//!
//! An owner that is no account any more — deleted before this existed — is
//! an orphan, and is settled the same way.

use crate::api::{ApiError, ApiResult, AppState};
use crate::audit::{Change, Event, Handover, Origin, ServerRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use zedis_connection::get_servers;

/// What becomes of what an account kept to itself.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum Disposition {
    /// It becomes `account`'s.
    HandTo { account: String },
    /// It becomes every account's.
    Share,
    /// It goes with the account.
    Delete,
}

impl Disposition {
    fn action(&self) -> &'static str {
        match self {
            Self::HandTo { .. } => "handed_to",
            Self::Share => "shared",
            Self::Delete => "deleted",
        }
    }

    /// The owner what is kept passes to: an account, or nobody (shared).
    fn new_owner(&self) -> Option<String> {
        match self {
            Self::HandTo { account } => Some(account.clone()),
            Self::Share | Self::Delete => None,
        }
    }
}

/// One thing an account kept to itself, by id and by the name people know.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnedItem {
    pub id: String,
    pub name: String,
}

/// What an account keeps to itself on the bridge.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Owned {
    pub servers: Vec<OwnedItem>,
    pub scripts: Vec<OwnedItem>,
}

impl Owned {
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.scripts.is_empty()
    }

    /// The audit's account of where it went.
    pub fn handover(&self, disposition: &Disposition) -> Handover {
        Handover {
            action: disposition.action(),
            to: disposition.new_owner(),
            servers: self.servers.len(),
            scripts: self.scripts.len(),
        }
    }
}

fn store_error(e: std::io::Error) -> ApiError {
    tracing::error!(error = %e, "a handover could not be kept");
    ApiError::Upstream(format!("the bridge could not keep it: {e}"))
}

/// Everything kept to itself, by owner.
async fn owned_by_owner(state: &AppState) -> ApiResult<BTreeMap<String, Owned>> {
    let mut owners: BTreeMap<String, Owned> = BTreeMap::new();
    for server in get_servers().unwrap_or_default() {
        if let Some(owner) = server.owner.filter(|owner| !owner.is_empty()) {
            owners.entry(owner).or_default().servers.push(OwnedItem {
                id: server.id,
                name: server.name,
            });
        }
    }
    for script in state.scripts.list().await.map_err(store_error)? {
        if let Some(owner) = script.owner.clone() {
            let name = script.name();
            owners
                .entry(owner)
                .or_default()
                .scripts
                .push(OwnedItem { id: script.id, name });
        }
    }
    Ok(owners)
}

/// What `owner` keeps to itself.
pub(crate) async fn owned_by(state: &AppState, owner: &str) -> ApiResult<Owned> {
    Ok(owned_by_owner(state).await?.remove(owner).unwrap_or_default())
}

/// Every owner that is no account any more, and what it left.
pub(crate) async fn orphans(state: &AppState) -> ApiResult<BTreeMap<String, Owned>> {
    let mut owners = owned_by_owner(state).await?;
    let accounts: BTreeSet<String> = owners
        .keys()
        .filter(|owner| state.accounts.exists(owner))
        .cloned()
        .collect();
    owners.retain(|owner, _| !accounts.contains(owner));
    Ok(owners)
}

/// Settle what `owner` kept to itself as `disposition` says, each change an
/// audit line under `admin` — the entries' as `server_updated` (the owner
/// from → to) or `server_deleted`, the scripts' as `script_saved` or
/// `script_deleted`. Answers what there was.
pub(crate) async fn settle(
    state: &AppState,
    admin: &str,
    origin: &Origin,
    owner: &str,
    disposition: &Disposition,
) -> ApiResult<Owned> {
    if let Disposition::HandTo { account } = disposition {
        if account == owner {
            return Err(ApiError::BadRequest("hand it to another account".to_string()));
        }
        if !state.accounts.exists(account) {
            return Err(ApiError::BadRequest(format!("no account {account:?}")));
        }
        // A read-only account could not edit what it was given, nor a
        // disabled one sign in to see it.
        if state.accounts.is_read_only(account) {
            return Err(ApiError::BadRequest(format!(
                "{account} is read-only or disabled and cannot own entries"
            )));
        }
    }
    let owned = owned_by(state, owner).await?;
    let new_owner = disposition.new_owner();

    for server in get_servers()
        .unwrap_or_default()
        .into_iter()
        .filter(|server| server.owner.as_deref() == Some(owner))
    {
        let reference = ServerRef::from(&server);
        if *disposition == Disposition::Delete {
            if state.servers.swap(server.clone(), None).await.map_err(store_error)? {
                zedis_connection::get_connection_manager().remove_server(&server);
                state.sessions.close_server(&server.id).await;
                state.watches.close_server(&server.id);
                // What was kept for it goes with it, as when an entry is deleted.
                let _ = state.trash.remove_server(server.id.clone()).await;
                let _ = state.metrics.remove_server(server.id.clone()).await;
                let _ = state.key_notes.remove_server(server.id.clone()).await;
                state
                    .audit
                    .record(admin, origin, Event::ServerDeleted { server: reference });
            }
            continue;
        }
        let mut handed = server.clone();
        handed.owner = new_owner.clone();
        if !state
            .servers
            .swap(server.clone(), Some(handed))
            .await
            .map_err(store_error)?
        {
            return Err(ApiError::Conflict(format!(
                "{} changed while it was being handed over; try again",
                server.name
            )));
        }
        zedis_connection::get_connection_manager().remove_server(&server);
        state.sessions.close_server(&server.id).await;
        state.watches.close_server(&server.id);
        let to = new_owner.clone().map_or(serde_json::Value::Null, Into::into);
        state.audit.record(
            admin,
            origin,
            Event::ServerUpdated {
                server: reference,
                changed: BTreeMap::from([("owner".to_string(), Change { from: owner.into(), to })]),
                secrets_changed: Vec::new(),
                shared: (*disposition == Disposition::Share).then(|| Change {
                    from: false.into(),
                    to: true.into(),
                }),
                error: None,
            },
        );
    }

    for mut script in state
        .scripts
        .list()
        .await
        .map_err(store_error)?
        .into_iter()
        .filter(|script| script.owner.as_deref() == Some(owner))
    {
        let (id, name) = (script.id.clone(), script.name());
        if *disposition == Disposition::Delete {
            if state.scripts.remove(id.clone()).await.map_err(store_error)?.is_some() {
                state.audit.record(admin, origin, Event::ScriptDeleted { id, name });
            }
            continue;
        }
        script.owner = new_owner.clone();
        state.scripts.put(script).await.map_err(store_error)?;
        state.audit.record(
            admin,
            origin,
            Event::ScriptSaved {
                id,
                name,
                shared: new_owner.is_none(),
                created: false,
            },
        );
    }
    Ok(owned)
}
