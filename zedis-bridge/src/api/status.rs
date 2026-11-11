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

//! What a load balancer and an administrator ask about the bridge itself:
//! is it alive, is its store answering, who is using it.

use super::*;

/// Unauthenticated, and says only that the bridge is up: liveness. It does
/// not ask the database — restarting a bridge whose database is away mends
/// nothing — which is `/v1/ready`'s question.
pub(super) async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// How long `/v1/ready` waits for the database to answer.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether the last `/v1/ready` found the database answering, so the log
/// says when that changes rather than once a probe.
static STORE_ANSWERED: AtomicBool = AtomicBool::new(true);

/// Readiness, for a load balancer in front of several bridges (ADR 18):
/// `200` while this bridge can serve — with a database, while it answers
/// within [`READY_TIMEOUT`] — and `503` otherwise, so traffic goes to the
/// others. Unauthenticated, and says nothing of the database but whether it
/// answered: the reason is in the process log.
pub(super) async fn ready(State(state): State<AppState>) -> Response {
    let Some(store) = state.store_probe.clone() else {
        return Json(serde_json::json!({ "status": "ok" })).into_response();
    };
    let failure = match tokio::time::timeout(READY_TIMEOUT, store.read()).await {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some(format!("no answer within {}s", READY_TIMEOUT.as_secs())),
    };
    let answered = failure.is_none();
    if STORE_ANSWERED.swap(answered, Ordering::Relaxed) != answered {
        match &failure {
            Some(error) => tracing::warn!(error, "not ready: the database does not answer"),
            None => tracing::info!("ready again: the database answers"),
        }
    }
    if answered {
        Json(serde_json::json!({ "status": "ok", "store": "ok" })).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "unavailable", "store": "unreachable" })),
        )
            .into_response()
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
/// What `GET /v1/clients` answers.
#[derive(Serialize)]
pub(super) struct ClientsReply {
    /// The bridge process that answers: each counts its own clients, and
    /// several share a database store (ADR 18).
    process: &'static str,
    /// How many clients made a request within the window.
    online: usize,
    window_secs: u64,
    clients: Vec<Client>,
}

/// Who is using the bridge now: every client that made a request within
/// `presence::ONLINE_WINDOW`, the most recently seen first. An
/// administrator's question, like the account list: the answer names other
/// accounts, where they call from and which servers they are on, which is
/// not every colleague's to read. Anyone else is refused, and the refusal
/// is an audit line.
pub(super) async fn clients(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<ClientsReply>> {
    let mut origin = Origin::new(peer, &headers);
    let account = account_api::authorize_admin(&state, &headers, &mut origin, "list clients").await?;
    let mut clients = state.presence.online();
    // Each client's servers by name — and only those the asker may see: an
    // entry of someone else's is not confirmed to exist by who uses it.
    for client in &mut clients {
        client.servers.retain_mut(|used| match get_server(&used.id) {
            Ok(server) if visible_to(&state.accounts, &server, &account) => {
                used.name = server.name;
                true
            }
            _ => false,
        });
    }
    Ok(Json(ClientsReply {
        process: crate::store::process_id(),
        online: clients.len(),
        window_secs: ONLINE_WINDOW.as_secs(),
        clients,
    }))
}

#[cfg(test)]
mod ready_tests {
    use super::*;
    use crate::store::{StoreFuture, Versions};

    /// A database that answers, or one that does not.
    struct Probe(bool);

    impl StoreVersions for Probe {
        fn read(&self) -> StoreFuture<Versions> {
            let answers = self.0;
            Box::pin(async move {
                if answers {
                    Ok(Versions::default())
                } else {
                    Err(std::io::Error::other("connection refused"))
                }
            })
        }
    }

    /// Ready while the database answers and not while it does not — and
    /// always on the local directory, which is there while the process is;
    /// the liveness check never asks the database at all.
    #[tokio::test]
    async fn ready_follows_the_database_and_health_does_not() {
        let state = |probe: Option<bool>| AppState {
            store_probe: probe.map(|answers| Arc::new(Probe(answers)) as Arc<dyn StoreVersions>),
            ..crate::api::mcp_auth_tests::state("[[users]]\nname = \"alice\"\npassword = \"alice-password\"\n")
        };
        assert_eq!(ready(State(state(None))).await.status(), StatusCode::OK);
        assert_eq!(ready(State(state(Some(true)))).await.status(), StatusCode::OK);
        assert_eq!(
            ready(State(state(Some(false)))).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(health().await.into_response().status(), StatusCode::OK);
    }
}

#[cfg(test)]
mod online_servers_tests {
    use super::*;
    use zedis_core::fs::override_config_dir;

    /// Who is online says which servers each client used, by name — and
    /// only those the asker may see: someone else's private entry is not
    /// confirmed to exist by who is on it, not even to an administrator.
    #[tokio::test]
    async fn the_online_list_names_the_servers_each_client_used_that_the_asker_sees() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-bridge-store-{}", std::process::id())));
        let entry = |id: &str, name: &str, owner: Option<&str>| RedisServer {
            id: id.to_string(),
            name: name.to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            owner: owner.map(str::to_string),
            ..Default::default()
        };
        for server in [
            entry("online-prod", "prod", None),
            entry("online-bobs", "bob's own", Some("bob")),
            entry("online-opss", "ops's own", Some("ops")),
        ] {
            crate::store::LocalServers.put(server).await.expect("server");
        }
        let state = crate::api::mcp_auth_tests::state(
            "[[users]]\nname = \"ops\"\npassword = \"secret\"\n\n\
             [[users]]\nname = \"bob\"\npassword = \"bobs-password\"",
        );
        state.accounts.make_admin("ops", None).await.expect("ops administers");
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("address");
        let basic = |who: &str| {
            let mut headers = HeaderMap::new();
            let value = format!("Basic {}", B64.encode(who));
            headers.insert("authorization", value.parse().expect("header"));
            headers
        };
        let bob = basic("bob:bobs-password");
        let mut origin = Origin::new(peer, &bob);
        assert!(authorize(&state, &bob, &mut origin).await.is_ok());
        note_server(&state, &origin, "online-bobs");
        note_server(&state, &origin, "online-prod");
        let ops = basic("ops:secret");
        let mut origin = Origin::new(peer, &ops);
        assert!(authorize(&state, &ops, &mut origin).await.is_ok());
        note_server(&state, &origin, "online-opss");

        let Ok(Json(reply)) = clients(State(state.clone()), ConnectInfo(peer), ops.clone()).await else {
            panic!("the administrator reads the list");
        };
        let named = |account: &str| -> Vec<(String, String)> {
            reply
                .clients
                .iter()
                .find(|client| client.account == account)
                .expect("online")
                .servers
                .iter()
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect()
        };
        assert_eq!(named("bob"), [("online-prod".to_string(), "prod".to_string())]);
        assert_eq!(
            named("ops"),
            [("online-opss".to_string(), "ops's own".to_string())],
            "its own private entry, by name"
        );

        assert!(
            matches!(
                clients(State(state.clone()), ConnectInfo(peer), bob.clone()).await,
                Err(ApiError::Forbidden(_))
            ),
            "a full account does not read it"
        );
    }

    /// Whoever signs in is online — a browser by its login, a script by its
    /// account and address — and the list of who is online is an
    /// administrator's to read: it names the others and where they call
    /// from, so a full account is refused like a read-only one.
    #[test]
    fn whoever_signs_in_is_online_and_only_an_administrator_reads_the_list() {
        let state = crate::api::mcp_auth_tests::state(
            "[[users]]\nname = \"ops\"\npassword = \"secret\"\n\n\
             [[users]]\nname = \"viewer\"\npassword = \"look\"\nread_only = true\n\n\
             [[users]]\nname = \"root\"\npassword = \"rootpass\"",
        );
        let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        runtime
            .block_on(state.accounts.make_admin("root", None))
            .expect("root is the administrator");
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("address");
        let browser_of = |name: &str| {
            let id = state.logins.open(name, &state.accounts, false).expect("login");
            let mut browser = HeaderMap::new();
            let cookie = format!("{}={id}", auth::COOKIE_NAME);
            browser.insert("cookie", cookie.parse().expect("cookie"));
            browser
        };
        let ops = browser_of("ops");
        let root = browser_of("root");
        let script = crate::api::mcp_auth_tests::headers(&format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("viewer:look")
        ));
        for headers in [&ops, &script, &root] {
            let mut origin = Origin::new(peer, headers);
            assert!(runtime.block_on(authorize(&state, headers, &mut origin)).is_ok());
        }

        let list = |headers: HeaderMap| runtime.block_on(clients(State(state.clone()), ConnectInfo(peer), headers));
        assert!(
            matches!(list(script.clone()), Err(ApiError::Forbidden(_))),
            "not a read-only account"
        );
        assert!(
            matches!(list(ops.clone()), Err(ApiError::Forbidden(_))),
            "nor a full one"
        );
        let Ok(Json(reply)) = list(root.clone()) else {
            panic!("an administrator reads the list");
        };
        assert_eq!(reply.online, 3);
        let mut seen: Vec<(&str, &str)> = reply.clients.iter().map(|c| (c.account.as_str(), c.kind)).collect();
        seen.sort();
        assert_eq!(seen, [("ops", "browser"), ("root", "browser"), ("viewer", "basic")]);
        assert!(reply.clients.iter().all(|c| c.peer.as_deref() == Some("10.0.0.7")));
    }
}
