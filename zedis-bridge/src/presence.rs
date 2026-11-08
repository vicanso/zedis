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

//! Who is using the bridge now: every authorized request marks its caller
//! seen, and a caller with no request for [`ONLINE_WINDOW`] is offline.
//!
//! A *client* is one browser sign-in — the login cookie, which every tab of
//! that browser shares — one script per account and address (HTTP Basic),
//! one MCP token, or one account behind the trusted proxy, which hands this
//! bridge no cookie of its own and no address of the browser's.
//!
//! A page with a server open is never quiet that long: it beats every 10s,
//! every 60s in a background tab (`pacing.rs`). A page on the home screen,
//! with nothing open, sends nothing, and goes offline after the window like
//! anything else that stopped asking.
//!
//! Each client also remembers the server entries its requests named in the
//! same window — a page beats on the one it has open — which is what lets
//! the list say who is on which server.
//!
//! Kept in memory in each process, and — when several bridges share a
//! `--store` database (ADR 18) — published and merged every [`crate::store::STORE_POLL`]
//! through [`PresenceStore`] so `GET /v1/clients` lists active clients across
//! every bridge process.

use crate::audit::{Origin, rfc3339};
use crate::store::PresenceStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a client may go without a request and still count as online.
pub const ONLINE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Refresh a process's snapshot in the shared store at least once a minute
/// while clients are online, even if no new request arrived.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// One client, as told apart from the others.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ClientKey {
    /// A browser sign-in, by its login's key — the SHA-256 of the cookie id,
    /// as the logins file keys it, never the id.
    Browser(String),
    /// A script signing in with HTTP Basic, per account and address.
    Basic { account: String, peer: Option<IpAddr> },
    /// An account's MCP token, by its name (ADR 15).
    Token { account: String, token: String },
    /// An identity the trusted proxy asserted (ADR 12).
    Proxy(String),
}

impl ClientKey {
    /// What the list and the log call this kind of client.
    pub fn kind(&self) -> &'static str {
        match self {
            ClientKey::Browser(_) => "browser",
            ClientKey::Basic { .. } => "basic",
            ClientKey::Token { .. } => "token",
            ClientKey::Proxy(_) => "proxy",
        }
    }
}

struct Seen {
    account: String,
    peer: Option<String>,
    forwarded_for: Option<String>,
    /// For the window: monotonic, so a clock change neither revives nor
    /// drops anyone.
    at: Instant,
    /// For people: wall-clock seconds.
    at_unix: u64,
    /// The server entries its requests named, by id, and when last.
    servers: HashMap<String, Instant>,
}

/// A client that is online, as `GET /v1/clients` lists it.
#[derive(Debug, PartialEq, Serialize)]
pub struct Client {
    pub account: String,
    pub kind: &'static str,
    /// The MCP token's name, for a token client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The socket peer of its last request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// What a proxy wrote in `X-Forwarded-For` on that request — whatever
    /// the previous hop said, as in the audit log.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded_for: Option<String>,
    pub last_seen: String,
    /// `last_seen` as Unix seconds, for a page to show in its own format.
    pub seen_at: u64,
    pub idle_secs: u64,
    /// The server entries it used within the window, the latest first —
    /// by id here; the route that lists them names the ones the asker may
    /// see and drops the rest.
    pub servers: Vec<ClientServer>,
}

/// A server entry a client used.
#[derive(Debug, PartialEq, Serialize)]
pub struct ClientServer {
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// One client in a process's shared presence snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedClient {
    pub key: ClientKey,
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_for: Option<String>,
    pub seen_at: u64,
    /// `(server_id, used_at_unix)` within [`ONLINE_WINDOW`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<(String, u64)>,
}

/// One bridge process's online clients, as stored in [`PresenceStore`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPresence {
    pub updated_at: u64,
    #[serde(default)]
    pub clients: Vec<SharedClient>,
}

#[derive(Default)]
struct HeldPresence {
    own: HashMap<ClientKey, Seen>,
    others: Vec<ProcessPresence>,
    dirty: bool,
    last_published: Option<Instant>,
}

#[derive(Clone)]
struct Shared {
    store: Arc<dyn PresenceStore>,
    process: String,
}

/// The clients seen within the window, and the ones just past it until the
/// next sweep.
#[derive(Clone, Default)]
pub struct Presence {
    held: Arc<Mutex<HeldPresence>>,
    shared: Option<Shared>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// A presence tracker that publishes its clients to `store` as `process`
    /// and merges the other bridge processes' snapshots.
    pub fn shared(store: Arc<dyn PresenceStore>, process: String) -> Self {
        Self {
            held: Arc::default(),
            shared: Some(Shared { store, process }),
        }
    }

    /// Mark `key` seen by a request from `origin`. Answers how many clients
    /// are online when this one was not until now, so the caller can say it
    /// came online.
    pub fn seen(&self, key: ClientKey, account: &str, origin: &Origin) -> Option<usize> {
        self.seen_at(key, account, origin, Instant::now(), unix_now())
    }

    fn seen_at(&self, key: ClientKey, account: &str, origin: &Origin, now: Instant, now_unix: u64) -> Option<usize> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let arrived = held
            .own
            .get(&key)
            .is_none_or(|seen| now.saturating_duration_since(seen.at) >= ONLINE_WINDOW);
        let mut servers = held.own.remove(&key).map(|seen| seen.servers).unwrap_or_default();
        servers.retain(|_, at| now.saturating_duration_since(*at) < ONLINE_WINDOW);
        held.own.insert(
            key,
            Seen {
                account: account.to_string(),
                peer: origin.peer_ip.map(|ip| ip.to_string()),
                forwarded_for: origin.forwarded_for.clone(),
                at: now,
                at_unix: now_unix,
                servers,
            },
        );
        held.dirty = true;
        arrived.then(|| online_count(&held.own, now))
    }

    /// Note that `key`'s request named the server entry `server_id`. A
    /// client not on the list — it was never authorized — is not added.
    pub fn used(&self, key: &ClientKey, server_id: &str) {
        self.used_at(key, server_id, Instant::now());
    }

    fn used_at(&self, key: &ClientKey, server_id: &str, now: Instant) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(seen) = held.own.get_mut(key) {
            seen.servers.insert(server_id.to_string(), now);
            held.dirty = true;
        }
    }

    /// Take a client off at once — a browser that signed out. Answers whose
    /// it was and how many are left online, when it was on the list.
    pub fn forget(&self, key: &ClientKey) -> Option<(String, usize)> {
        let now = Instant::now();
        let now_unix = unix_now();
        let (account, remaining, snapshot) = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let seen = held.own.remove(key)?;
            held.dirty = true;
            let count = online_count(&held.own, now);
            let snap = self.shared.is_some().then(|| snapshot_of(&held.own, now, now_unix));
            (seen.account, count, snap)
        };
        if let (Some(shared), Some(snapshot)) = (&self.shared, snapshot)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let store = shared.store.clone();
            let process = shared.process.clone();
            runtime.spawn(async move {
                if let Err(e) = store.put(process, snapshot).await {
                    tracing::warn!(error = %e, "could not publish presence after sign-out");
                }
            });
        }
        Some((account, remaining))
    }

    /// Drop every client quiet past the window: each one's account and
    /// kind, and how many are still online.
    pub fn sweep(&self) -> (Vec<(String, &'static str)>, usize) {
        self.sweep_at(Instant::now())
    }

    fn sweep_at(&self, now: Instant) -> (Vec<(String, &'static str)>, usize) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut gone = Vec::new();
        held.own.retain(|key, seen| {
            let online = now.saturating_duration_since(seen.at) < ONLINE_WINDOW;
            if !online {
                gone.push((seen.account.clone(), key.kind()));
            }
            online
        });
        if !gone.is_empty() {
            held.dirty = true;
        }
        gone.sort();
        let online = held.own.len();
        (gone, online)
    }

    /// Publish this process's online clients to [`PresenceStore`] when changed
    /// (or once a minute as a heartbeat) and read the other processes' snapshots.
    pub async fn sync(&self) {
        let Some(shared) = &self.shared else {
            return;
        };
        let now = Instant::now();
        let now_unix = unix_now();
        let to_publish = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let due_heartbeat = !held.own.is_empty()
                && held
                    .last_published
                    .is_none_or(|last| now.saturating_duration_since(last) >= HEARTBEAT_INTERVAL);
            if held.dirty || due_heartbeat {
                held.dirty = false;
                held.last_published = Some(now);
                Some(snapshot_of(&held.own, now, now_unix))
            } else {
                None
            }
        };
        if let Some(snapshot) = to_publish
            && let Err(e) = shared.store.put(shared.process.clone(), snapshot).await
        {
            tracing::warn!(error = %e, "this process's online clients could not be written to the store");
        }
        let cutoff = now_unix.saturating_sub(ONLINE_WINDOW.as_secs());
        match shared.store.others(shared.process.clone(), cutoff).await {
            Ok(others) => {
                self.held.lock().unwrap_or_else(PoisonError::into_inner).others = others;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the other processes' online clients could not be read");
            }
        }
    }

    /// Prune expired process snapshots from [`PresenceStore`].
    pub async fn prune_shared(&self) {
        let Some(shared) = &self.shared else {
            return;
        };
        let cutoff = unix_now().saturating_sub(ONLINE_WINDOW.as_secs());
        if let Err(e) = shared.store.prune(cutoff).await {
            tracing::warn!(error = %e, "stale online client snapshots could not be pruned");
        }
    }

    /// Who is online, the most recently seen first.
    pub fn online(&self) -> Vec<Client> {
        self.online_at(Instant::now())
    }

    fn online_at(&self, now: Instant) -> Vec<Client> {
        let now_unix = unix_now();
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        struct Entry {
            account: String,
            peer: Option<String>,
            forwarded_for: Option<String>,
            at_unix: u64,
            idle: Duration,
            servers: HashMap<String, Duration>,
        }
        let mut merged: HashMap<ClientKey, Entry> = HashMap::new();
        for (key, seen) in &held.own {
            let idle = now.saturating_duration_since(seen.at);
            if idle >= ONLINE_WINDOW {
                continue;
            }
            let servers: HashMap<String, Duration> = seen
                .servers
                .iter()
                .map(|(id, at)| (id.clone(), now.saturating_duration_since(*at)))
                .filter(|(_, since)| *since < ONLINE_WINDOW)
                .collect();
            merged.insert(
                key.clone(),
                Entry {
                    account: seen.account.clone(),
                    peer: seen.peer.clone(),
                    forwarded_for: seen.forwarded_for.clone(),
                    at_unix: seen.at_unix,
                    idle,
                    servers,
                },
            );
        }
        for snapshot in &held.others {
            if now_unix.saturating_sub(snapshot.updated_at) >= ONLINE_WINDOW.as_secs() {
                continue;
            }
            for client in &snapshot.clients {
                let idle = Duration::from_secs(now_unix.saturating_sub(client.seen_at));
                if idle >= ONLINE_WINDOW {
                    continue;
                }
                let other_servers: Vec<(String, Duration)> = client
                    .servers
                    .iter()
                    .map(|(id, ts)| (id.clone(), Duration::from_secs(now_unix.saturating_sub(*ts))))
                    .filter(|(_, since)| *since < ONLINE_WINDOW)
                    .collect();
                match merged.get_mut(&client.key) {
                    Some(existing) => {
                        if idle < existing.idle {
                            existing.account = client.account.clone();
                            existing.peer = client.peer.clone();
                            existing.forwarded_for = client.forwarded_for.clone();
                            existing.at_unix = client.seen_at;
                            existing.idle = idle;
                        }
                        for (id, since) in other_servers {
                            existing
                                .servers
                                .entry(id)
                                .and_modify(|prev| *prev = (*prev).min(since))
                                .or_insert(since);
                        }
                    }
                    None => {
                        merged.insert(
                            client.key.clone(),
                            Entry {
                                account: client.account.clone(),
                                peer: client.peer.clone(),
                                forwarded_for: client.forwarded_for.clone(),
                                at_unix: client.seen_at,
                                idle,
                                servers: other_servers.into_iter().collect(),
                            },
                        );
                    }
                }
            }
        }
        let mut online: Vec<Client> = merged
            .into_iter()
            .map(|(key, entry)| {
                let mut servers: Vec<(String, Duration)> = entry.servers.into_iter().collect();
                servers.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                Client {
                    account: entry.account,
                    kind: key.kind(),
                    token: match key {
                        ClientKey::Token { token, .. } => Some(token),
                        _ => None,
                    },
                    peer: entry.peer,
                    forwarded_for: entry.forwarded_for,
                    last_seen: rfc3339(entry.at_unix, 0),
                    seen_at: entry.at_unix,
                    idle_secs: entry.idle.as_secs(),
                    servers: servers
                        .into_iter()
                        .map(|(id, _)| ClientServer {
                            id,
                            name: String::new(),
                        })
                        .collect(),
                }
            })
            .collect();
        online.sort_by(|a, b| a.idle_secs.cmp(&b.idle_secs).then_with(|| a.account.cmp(&b.account)));
        online
    }
}

fn snapshot_of(own: &HashMap<ClientKey, Seen>, now: Instant, now_unix: u64) -> ProcessPresence {
    let clients = own
        .iter()
        .filter_map(|(key, seen)| {
            let idle = now.saturating_duration_since(seen.at);
            if idle >= ONLINE_WINDOW {
                return None;
            }
            let servers: Vec<(String, u64)> = seen
                .servers
                .iter()
                .filter_map(|(id, at)| {
                    let since = now.saturating_duration_since(*at);
                    (since < ONLINE_WINDOW).then(|| (id.clone(), now_unix.saturating_sub(since.as_secs())))
                })
                .collect();
            Some(SharedClient {
                key: key.clone(),
                account: seen.account.clone(),
                peer: seen.peer.clone(),
                forwarded_for: seen.forwarded_for.clone(),
                seen_at: seen.at_unix,
                servers,
            })
        })
        .collect();
    ProcessPresence {
        updated_at: now_unix,
        clients,
    }
}

fn online_count(clients: &HashMap<ClientKey, Seen>, now: Instant) -> usize {
    clients
        .values()
        .filter(|seen| now.saturating_duration_since(seen.at) < ONLINE_WINDOW)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(ip: &str) -> Origin {
        Origin {
            peer_ip: Some(ip.parse().expect("ip")),
            ..Origin::default()
        }
    }

    fn browser(n: u8) -> ClientKey {
        ClientKey::Browser(format!("login-{n}"))
    }

    /// A client is online from its first request until five minutes pass
    /// without another, and only its arrival is news.
    #[test]
    fn a_client_is_online_until_five_quiet_minutes_pass() {
        let presence = Presence::new();
        let t0 = Instant::now();
        assert_eq!(presence.seen_at(browser(1), "alice", &from("10.0.0.1"), t0, 1), Some(1));
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            presence.seen_at(browser(1), "alice", &from("10.0.0.1"), later, 61),
            None,
            "already online"
        );

        let almost = later + ONLINE_WINDOW - Duration::from_secs(1);
        let online = presence.online_at(almost);
        assert_eq!(online.len(), 1);
        assert_eq!((online[0].account.as_str(), online[0].kind), ("alice", "browser"));
        assert_eq!(online[0].peer.as_deref(), Some("10.0.0.1"));
        assert_eq!(online[0].idle_secs, ONLINE_WINDOW.as_secs() - 1);

        let past = later + ONLINE_WINDOW;
        assert!(presence.online_at(past).is_empty(), "five quiet minutes");
        assert_eq!(presence.sweep_at(past), (vec![("alice".to_string(), "browser")], 0));
        assert_eq!(
            presence.seen_at(browser(1), "alice", &from("10.0.0.1"), past, 400),
            Some(1),
            "back online"
        );
    }

    /// One browser sign-in is one client however many tabs share its cookie;
    /// a script is one per account and address; a token one per name.
    #[test]
    fn clients_are_sign_ins_scripts_by_address_and_tokens_by_name() {
        let presence = Presence::new();
        let now = Instant::now();
        let seen = |key: ClientKey, account: &str, ip: &str| presence.seen_at(key, account, &from(ip), now, 1);
        seen(browser(1), "alice", "10.0.0.1");
        seen(browser(1), "alice", "10.0.0.1");
        seen(browser(2), "alice", "10.0.0.2");
        let script = |ip: &str| ClientKey::Basic {
            account: "ops".to_string(),
            peer: Some(ip.parse().expect("ip")),
        };
        seen(script("10.0.0.3"), "ops", "10.0.0.3");
        seen(script("10.0.0.3"), "ops", "10.0.0.3");
        seen(script("10.0.0.4"), "ops", "10.0.0.4");
        let token = ClientKey::Token {
            account: "ai".to_string(),
            token: "claude-laptop".to_string(),
        };
        assert_eq!(seen(token, "ai", "10.0.0.5"), Some(5));

        let online = presence.online_at(now);
        assert_eq!(online.len(), 5, "two browsers, two script addresses, a token");
        let token = online.iter().find(|c| c.kind == "token").expect("the token client");
        assert_eq!(token.token.as_deref(), Some("claude-laptop"));
    }

    /// A client remembers the servers its requests named, the latest first,
    /// for as long as the window — and a request with no server keeps them.
    #[test]
    fn a_client_remembers_the_servers_it_used_within_the_window() {
        let presence = Presence::new();
        let t0 = Instant::now();
        presence.seen_at(browser(1), "alice", &from("10.0.0.1"), t0, 1);
        presence.used_at(&browser(1), "prod", t0);
        presence.used_at(&browser(1), "staging", t0 + Duration::from_secs(10));
        presence.used_at(&browser(9), "prod", t0);
        let later = t0 + Duration::from_secs(20);
        presence.seen_at(browser(1), "alice", &from("10.0.0.1"), later, 21);
        let ids = |now: Instant| -> Vec<String> {
            presence.online_at(now)[0]
                .servers
                .iter()
                .map(|s| s.id.clone())
                .collect()
        };
        assert_eq!(ids(later), ["staging", "prod"]);
        assert_eq!(presence.online_at(later).len(), 1, "a client never seen is not added");

        // Five minutes after `prod` alone, it has gone from the list.
        let past_prod = t0 + ONLINE_WINDOW + Duration::from_secs(5);
        presence.seen_at(browser(1), "alice", &from("10.0.0.1"), past_prod, 306);
        assert_eq!(ids(past_prod), ["staging"]);
    }

    /// Signing out takes the browser off at once rather than five minutes on.
    #[test]
    fn signing_out_takes_the_browser_off_at_once() {
        let presence = Presence::new();
        presence.seen(browser(1), "alice", &from("10.0.0.1"));
        presence.seen(browser(2), "bob", &from("10.0.0.2"));
        assert_eq!(presence.forget(&browser(1)), Some(("alice".to_string(), 1)));
        assert_eq!(presence.forget(&browser(1)), None);
        assert_eq!(presence.online().len(), 1);
    }

    struct MemPresenceStore(Mutex<HashMap<String, ProcessPresence>>);

    impl PresenceStore for MemPresenceStore {
        fn put(&self, process: String, snapshot: ProcessPresence) -> crate::store::StoreFuture<()> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(process, snapshot);
            Box::pin(async { Ok(()) })
        }

        fn others(&self, process: String, since_secs: u64) -> crate::store::StoreFuture<Vec<ProcessPresence>> {
            let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let out = map
                .iter()
                .filter(|(k, v)| *k != &process && v.updated_at >= since_secs)
                .map(|(_, v)| v.clone())
                .collect();
            Box::pin(async move { Ok(out) })
        }

        fn prune(&self, before_secs: u64) -> crate::store::StoreFuture<u64> {
            let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let before = map.len();
            map.retain(|_, v| v.updated_at >= before_secs);
            let removed = (before - map.len()) as u64;
            Box::pin(async move { Ok(removed) })
        }
    }

    /// Two bridges sharing a [`PresenceStore`] see each other's online clients
    /// and union the servers a client used across both processes.
    #[tokio::test]
    async fn shared_presence_merges_clients_across_processes() {
        let store: Arc<dyn PresenceStore> = Arc::new(MemPresenceStore(Mutex::default()));
        let bridge_a = Presence::shared(store.clone(), "proc-a".to_string());
        let bridge_b = Presence::shared(store.clone(), "proc-b".to_string());

        bridge_a.seen(browser(1), "alice", &from("10.0.0.1"));
        bridge_a.used(&browser(1), "prod");
        bridge_b.seen(browser(2), "bob", &from("10.0.0.2"));
        bridge_b.used(&browser(2), "staging");

        bridge_a.sync().await;
        bridge_b.sync().await;
        bridge_a.sync().await;

        let on_a = bridge_a.online();
        assert_eq!(on_a.len(), 2, "bridge A sees both its own and bridge B's clients");
        let names: Vec<&str> = on_a.iter().map(|c| c.account.as_str()).collect();
        assert!(names.contains(&"alice") && names.contains(&"bob"));

        let on_b = bridge_b.online();
        assert_eq!(on_b.len(), 2, "bridge B sees both its own and bridge A's clients");
    }
}
