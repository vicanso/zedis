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

//! Where the bridge keeps what it keeps (ADR 17).
//!
//! Each kind of data is reached through a trait here, and the routes hold an
//! `Arc<dyn …>` of it: the accounts, invites and requests ([`AccountStore`],
//! records), the server list ([`ServerStore`], records), browser logins
//! ([`LoginStore`], leases), the recycle bin ([`TrashStore`], blobs beside
//! their metadata), the Metrics history ([`MetricsStore`], a log), the
//! page's own data — an account's rows ([`PageRowStore`]), key tags and
//! notes ([`KeyNoteStore`]) and saved scripts ([`ScriptStore`]), records —
//! and the audit log ([`AuditLog`]), with the setup token ([`SetupStore`]).
//! The implementations are the local directory's — [`LocalServers`] below,
//! `accounts::{LocalAccounts, LocalSetup}`, `auth::LocalLogins`,
//! `trash::LocalTrash`, `metrics::LocalMetrics`,
//! `page_data::{LocalPageRows, LocalKeyNotes, LocalScripts}`,
//! `audit::LocalAuditLog` — files beside the server list. A SQL store
//! is another `impl` of the same traits, chosen at start-up; nothing that
//! calls them changes.
//!
//! What only a store shared by several processes needs (ADR 18) is here
//! too, and the local directory — one process — has none of it: the
//! counters that tell a process another one changed what it keeps in
//! memory ([`StoreVersions`]), and the failed sign-ins the others saw
//! ([`ThrottleStore`]).
//!
//! Async, because a database is: the local implementations do their file
//! work on the blocking pool ([`blocking`]), where the routes used to put it
//! themselves. Arguments are owned and the futures `'static`, the shape
//! `zedis_connection::bridge` gives the page's side of the same data.

use crate::accounts::Accounts;
use crate::accounts::{AccountOp, AccountRecord, InviteRecord, LocalAccounts, LocalSetup, SignupRecord};
use crate::auth::{LocalLogins, Login, LoginChange, Logins, Throttle};
use crate::metrics::{self, LocalMetrics};
use crate::page_data::{AccountRows, KeyNote, LocalKeyNotes, LocalPageRows, LocalScripts, RowError, ScriptRecord};
use crate::presence::{Presence, ProcessPresence};
use crate::trash::{self, LocalTrash, Meta, NewEntry};
use futures::future::BoxFuture;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use zedis_connection::{RedisServer, get_servers, save_servers};

/// What a store answers, when it is done.
pub type StoreFuture<T> = BoxFuture<'static, io::Result<T>>;

/// `work`, which touches the disk, on the blocking pool.
pub fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> StoreFuture<T> {
    Box::pin(async move { tokio::task::spawn_blocking(work).await.map_err(io::Error::other) })
}

/// The server list: records, changed one entry at a time.
///
/// An implementation keeps `zedis_connection`'s cache — what `get_server`
/// reads and the pool dials from — in step with what it stores.
pub trait ServerStore: Send + Sync {
    /// Read the list into the cache again: another process changed it
    /// ([`StoreVersions`]).
    fn refresh(&self) -> StoreFuture<()>;
    /// Every entry, secrets decrypted.
    fn list(&self) -> StoreFuture<Vec<RedisServer>>;
    /// Add `server`, or put it in place of the entry with its id.
    fn put(&self, server: RedisServer) -> StoreFuture<()>;
    /// Put `replacement` in place of the entry `expected.id` — or remove the
    /// entry, for `None` — if that entry is still exactly `expected`. Answers
    /// whether it was: an entry someone changed in the meantime is theirs.
    fn swap(&self, expected: RedisServer, replacement: Option<RedisServer>) -> StoreFuture<bool>;
}

/// The accounts at one moment, and the count of changes that names it —
/// what a change is applied against (ADR 18).
#[derive(Clone, Debug, Default)]
pub struct AccountsAt {
    pub version: u64,
    pub accounts: Vec<AccountRecord>,
    pub invites: Vec<InviteRecord>,
    /// The requests for an account waiting for an administrator.
    pub signups: Vec<SignupRecord>,
}

/// The accounts (ADR 17): registered, each with its role, server rules and
/// MCP tokens, the invitations that register the next ones, and the requests
/// for one an administrator has yet to answer. Records;
/// the bridge keeps them in memory in front (`accounts::Accounts`), decides
/// a change there and writes it through.
pub trait AccountStore: Send + Sync {
    /// Every account, invitation and request, and the version they are at.
    fn load(&self) -> StoreFuture<AccountsAt>;
    /// The version alone: whether what a process holds is still current.
    fn version(&self) -> StoreFuture<u64>;
    /// Apply `ops` together, if the accounts are still at version `seen`.
    /// Answers the version they are at now — or `None` when another process
    /// changed them first, and the change was decided on what is no longer
    /// there.
    fn apply(&self, seen: u64, ops: Vec<AccountOp>) -> StoreFuture<Option<u64>>;
}

/// Browser logins: leases, each keyed by the hash of its cookie id and
/// carrying the credential version it was opened under. The bridge holds
/// them in memory (`auth::Logins`) and hands the store what changed.
pub trait LoginStore: Send + Sync {
    /// Every saved login.
    fn load(&self) -> StoreFuture<HashMap<String, Login>>;
    /// The saved login `key`, for a cookie this process has not seen —
    /// another process opened it (ADR 18).
    fn get(&self, key: String) -> StoreFuture<Option<Login>>;
    /// Keep each sign-in, move each use's time forward on the login if it
    /// is still there — never put it back — and forget each sign-out.
    fn save(&self, changes: Vec<(String, LoginChange)>) -> StoreFuture<()>;
}

/// The recycle bin of the browser build: per server, a deleted key's `DUMP`
/// beside what lists it, for [`trash::RETENTION`].
pub trait TrashStore: Send + Sync {
    /// Keep a deleted key, making room by dropping the oldest.
    fn put(&self, server_id: String, entry: NewEntry) -> BoxFuture<'static, Result<Meta, trash::PutError>>;
    /// What `server_id`'s bin holds, newest first.
    fn list(&self, server_id: String) -> StoreFuture<Vec<Meta>>;
    /// One deleted key, with its payload.
    fn get(&self, server_id: String, id: String) -> StoreFuture<Option<(Meta, Vec<u8>)>>;
    /// Forget one deleted key — restored, or not wanted — answering what it
    /// was, or `None` if the bin did not hold it.
    fn remove(&self, server_id: String, id: String) -> StoreFuture<Option<Meta>>;
    /// Forget a server's whole bin: the entry itself is gone.
    fn remove_server(&self, server_id: String) -> StoreFuture<()>;
    /// Drop every entry past its retention, answering how many went.
    fn purge_expired(&self) -> StoreFuture<usize>;
}

/// The Metrics history of the browser build: one sample a minute per
/// server, for [`metrics::RETENTION_DAYS`].
pub trait MetricsStore: Send + Sync {
    /// Keep `sample` for `server_id` at `ts_ms`, unless the server already
    /// has this minute's. Answers whether it was kept.
    fn put(&self, server_id: String, ts_ms: i64, sample: String)
    -> BoxFuture<'static, Result<bool, metrics::PutError>>;
    /// The samples since `from_ms`, oldest first, thinned to `max_points`.
    fn since(&self, server_id: String, from_ms: i64, max_points: usize) -> StoreFuture<Vec<Box<RawValue>>>;
    /// Forget a server's whole history: the entry itself is gone.
    fn remove_server(&self, server_id: String) -> StoreFuture<()>;
    /// Delete what is past the retention, answering how much went.
    fn prune(&self) -> StoreFuture<usize>;
}

/// An account's own rows of the page's data (ADR 17): favorites, recent
/// keys, search history, preferences — private, keyed by table and key.
pub trait PageRowStore: Send + Sync {
    /// Every row `account` keeps.
    fn rows(&self, account: String) -> StoreFuture<AccountRows>;
    /// Keep `value` under `table` / `key`, or forget it for `None`.
    fn put(
        &self,
        account: String,
        table: String,
        key: String,
        value: Option<String>,
    ) -> BoxFuture<'static, Result<(), RowError>>;
    /// Forget every row of `account`: it is gone.
    fn forget(&self, account: String) -> StoreFuture<()>;
}

/// Key tags and notes (ADR 17): per server, one key at a time, with who left
/// each and when.
pub trait KeyNoteStore: Send + Sync {
    /// `server_id`'s notes, by key.
    fn list(&self, server_id: String) -> StoreFuture<BTreeMap<String, KeyNote>>;
    /// Keep `note` on `key`, or forget the key's for `None`.
    fn put(&self, server_id: String, key: String, note: Option<KeyNote>) -> StoreFuture<()>;
    /// Forget a server's notes: the entry itself is gone.
    fn remove_server(&self, server_id: String) -> StoreFuture<()>;
}

/// Saved Lua scripts (ADR 17): records, private or shared, one at a time.
pub trait ScriptStore: Send + Sync {
    fn list(&self) -> StoreFuture<Vec<ScriptRecord>>;
    /// Add `record`, or put it in place of the one with its id.
    fn put(&self, record: ScriptRecord) -> StoreFuture<()>;
    /// Forget the script `id`, answering what it was.
    fn remove(&self, id: String) -> StoreFuture<Option<ScriptRecord>>;
}

/// One line of the audit, with what it is looked up by.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    /// When, in unix milliseconds — the line's `ts`.
    pub at_ms: i64,
    pub account: String,
    pub event: String,
    /// The server entry the line is about, by id.
    pub server: Option<String>,
    /// The JSON object, without its newline.
    pub line: Vec<u8>,
}

/// What an administrator asks the audit (`GET /v1/admin/audit`, ADR 18):
/// every field narrows, none is every line.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct AuditQuery {
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub account: Option<String>,
    pub event: Option<String>,
    pub server: Option<String>,
    /// Where the previous page stopped: its `next`.
    pub before: Option<String>,
    pub limit: Option<usize>,
}

/// The most lines one page holds, and how many when not asked.
pub const AUDIT_PAGE_MAX: usize = 500;
pub const AUDIT_PAGE_DEFAULT: usize = 100;

impl AuditQuery {
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(AUDIT_PAGE_DEFAULT).clamp(1, AUDIT_PAGE_MAX)
    }
}

/// A page of the audit, newest first.
#[derive(Debug, Default, serde::Serialize)]
pub struct AuditPage {
    pub lines: Vec<Box<RawValue>>,
    /// Where the next, older page starts; `None` when there is none.
    pub next: Option<String>,
    /// The local file was not read to its start for this page — it reads a
    /// bounded part per page — so `next` may find older lines that match.
    pub stopped_early: bool,
}

/// The audit log (ADR 11): appended to, one JSON line at a time, and read
/// back by an administrator (ADR 18).
pub trait AuditLog: Send + Sync {
    /// Keep `entry`. Never fails the caller: an implementation that cannot
    /// keep a line says so in the process log (ADR 11).
    fn append(&self, entry: AuditEntry);
    fn query(&self, query: AuditQuery) -> StoreFuture<AuditPage>;
    /// Forget the lines before `before_ms`, answering how many went. A file
    /// keeps everything: rotating it is the host's.
    fn prune(&self, before_ms: i64) -> StoreFuture<u64>;
}

/// The one-time token that seats an administrator (ADR 17), where every
/// process reads it.
pub trait SetupStore: Send + Sync {
    /// Whether a token is waiting.
    fn waiting(&self) -> StoreFuture<bool>;
    /// Whether `token` is the waiting one.
    fn accepts(&self, token: String) -> StoreFuture<bool>;
    /// Keep `token` in place of any waiting one: `zedis-bridge setup-token`.
    fn put(&self, token: String) -> StoreFuture<()>;
    /// Keep `token` unless one is waiting, answering whether it was kept —
    /// so processes that start together mint one between them.
    fn put_if_none(&self, token: String) -> StoreFuture<bool>;
    /// The waiting token itself, where the store can say it: the local
    /// file can, a database keeps only its hash.
    fn shown(&self) -> StoreFuture<Option<String>>;
    /// The token did its work.
    fn take(&self) -> StoreFuture<()>;
    /// Where it is kept, for the log: a path, or "the database".
    fn location(&self) -> String;
}

/// Failed sign-ins other processes saw (ADR 18). The throttle decides in
/// each process's memory (`auth::Throttle`) and shares through this, so ten
/// failed tries are ten across every process.
pub trait ThrottleStore: Send + Sync {
    /// Keep one failure of `key`, seen by `process` at `at_ms`.
    fn failed(&self, process: String, key: String, at_ms: i64) -> StoreFuture<()>;
    /// Forget `key`'s failures: it got in.
    fn succeeded(&self, key: String) -> StoreFuture<()>;
    /// The failures since `since_ms` seen by processes other than `process`.
    fn others(&self, process: String, since_ms: i64) -> StoreFuture<Vec<(String, i64)>>;
    /// Forget the failures before `before_ms`, answering how many went.
    fn prune(&self, before_ms: i64) -> StoreFuture<u64>;
}

/// Online clients other processes saw (`GET /v1/clients`). Each bridge
/// publishes its own active client snapshot and reads the others' every
/// [`STORE_POLL`] so `GET /v1/clients` lists active clients across all
/// bridges sharing a store.
pub trait PresenceStore: Send + Sync {
    /// Keep `process`'s current online client snapshot.
    fn put(&self, process: String, snapshot: ProcessPresence) -> StoreFuture<()>;
    /// The snapshots updated since `since_secs` by processes other than `process`.
    fn others(&self, process: String, since_secs: u64) -> StoreFuture<Vec<ProcessPresence>>;
    /// Forget snapshots not updated since `before_secs`, answering how many went.
    fn prune(&self, before_secs: u64) -> StoreFuture<u64>;
}

/// The count of changes to each set a process keeps in memory, read every
/// [`STORE_POLL`] by every process (ADR 18): a counter that moved is a set
/// another process changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Versions {
    pub servers: u64,
    pub accounts: u64,
    /// Moved by a sign-out or an expired login anywhere.
    pub logins_revoked: u64,
}

/// How often a process reads the [`Versions`]: the longest it may act on an
/// account, a server list or a login another process just changed.
pub const STORE_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// This process, among the others on a shared store: a failed sign-in is
/// filed under it so it is not counted twice, and `GET /v1/clients`, which
/// answers for one process, says which.
pub fn process_id() -> &'static str {
    static PROCESS: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());
    &PROCESS
}

pub trait StoreVersions: Send + Sync {
    fn read(&self) -> StoreFuture<Versions>;
}

/// What a process holds in memory of a store other processes change too.
pub struct Held {
    pub servers: Arc<dyn ServerStore>,
    pub accounts: Accounts,
    pub logins: Logins,
    pub throttle: Throttle,
    pub presence: Presence,
}

/// Keep `held` in step with `versions`, every [`STORE_POLL`], for the life
/// of the process: the set whose counter moved is read again — the server
/// list into `zedis_connection`'s cache, the accounts, the logins dropped
/// to be taken in again one by one — and the other processes' failed
/// sign-ins and online clients merged.
pub async fn keep_in_step(versions: Arc<dyn StoreVersions>, held: Held) {
    let mut seen = versions.read().await.unwrap_or_default();
    let mut tick = tokio::time::interval(STORE_POLL);
    loop {
        tick.tick().await;
        held.throttle.merge().await;
        held.presence.sync().await;
        let now = match versions.read().await {
            Ok(now) => now,
            Err(e) => {
                tracing::warn!(error = %e, "the store's versions could not be read");
                continue;
            }
        };
        if now.servers != seen.servers
            && let Err(e) = held.servers.refresh().await
        {
            tracing::warn!(error = %e, "the server list could not be read again");
            continue;
        }
        // Compared with what is held, not with the last poll: this
        // process's own changes move the counter too, and are held already.
        if now.accounts != held.accounts.version_held()
            && let Err(e) = held.accounts.refresh().await
        {
            tracing::warn!(error = %e, "the accounts could not be read again");
            continue;
        }
        if now.logins_revoked != seen.logins_revoked {
            held.logins.drop_cached().await;
        }
        seen = now;
    }
}

/// The stores a bridge starts with.
pub struct Stores {
    pub accounts: Arc<dyn AccountStore>,
    pub logins: Arc<dyn LoginStore>,
    pub servers: Arc<dyn ServerStore>,
    pub trash: Arc<dyn TrashStore>,
    pub metrics: Arc<dyn MetricsStore>,
    pub page_rows: Arc<dyn PageRowStore>,
    pub key_notes: Arc<dyn KeyNoteStore>,
    pub scripts: Arc<dyn ScriptStore>,
    pub setup: Arc<dyn SetupStore>,
    /// The audit where the store keeps it — the database's — or `None`: the
    /// local directory's audit is the file `--audit-log` names.
    pub audit: Option<Arc<dyn AuditLog>>,
    /// What only several processes need — `None` for one.
    pub throttle: Option<Arc<dyn ThrottleStore>>,
    pub presence: Option<Arc<dyn PresenceStore>>,
    pub versions: Option<Arc<dyn StoreVersions>>,
}

/// Where the local directory keeps each store, under the configuration
/// directory — what `Stores::local` opens and `import-local` reads.
pub mod local_paths {
    pub const ACCOUNTS: &str = "bridge-accounts.json";
    pub const LOGINS: &str = "bridge-logins.json";
    pub const SERVERS: &str = "redis-servers.toml";
    pub const TRASH: &str = "bridge-trash";
    pub const METRICS: &str = "bridge-metrics";
    pub const PAGE_ROWS: &str = "bridge-page-data/accounts";
    pub const KEY_NOTES: &str = "bridge-page-data/key-notes";
    pub const SCRIPTS: &str = "bridge-scripts.json";
    pub const SETUP_TOKEN: &str = "setup-token";
}

impl Stores {
    /// The local directory's, under `dir` — the configuration directory,
    /// `/data` in the image.
    pub fn local(dir: &Path) -> Self {
        use local_paths as at;
        Self {
            accounts: Arc::new(LocalAccounts::new(dir.join(at::ACCOUNTS))),
            logins: Arc::new(LocalLogins::new(dir.join(at::LOGINS))),
            servers: Arc::new(LocalServers),
            trash: Arc::new(LocalTrash::new(dir.join(at::TRASH))),
            metrics: Arc::new(LocalMetrics::new(dir.join(at::METRICS))),
            page_rows: Arc::new(LocalPageRows::new(dir.join(at::PAGE_ROWS))),
            key_notes: Arc::new(LocalKeyNotes::new(dir.join(at::KEY_NOTES))),
            scripts: Arc::new(LocalScripts::new(dir.join(at::SCRIPTS))),
            setup: Arc::new(LocalSetup::new(dir.join(at::SETUP_TOKEN))),
            audit: None,
            throttle: None,
            presence: None,
            versions: None,
        }
    }
}

/// The server list in `redis-servers.toml`, through `zedis_connection`'s own
/// `get_servers` / `save_servers` — the desktop's functions, unchanged — so
/// the file and the cache are what they always were.
pub struct LocalServers;

/// One change to `redis-servers.toml` at a time.
///
/// The file is read, changed and written whole — and saving it updates the
/// in-memory copy first and writes the file afterwards, off the request — so
/// two changes that overlapped kept only one of them: an entry added while
/// another was edited could vanish from the list, from the file, or from
/// one of the two. [`LocalServers::edit`] holds this from the read to the
/// end of the write.
static LIST_EDIT: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

impl LocalServers {
    /// Read the list, let `change` edit it, and save it if `change` says it
    /// changed anything — under [`LIST_EDIT`]. Answers what `change` did.
    async fn edit(change: impl FnOnce(&mut Vec<RedisServer>) -> bool) -> io::Result<bool> {
        let _edit = LIST_EDIT.lock().await;
        let mut servers = get_servers().map_err(|e| io::Error::other(e.to_string()))?;
        if !change(&mut servers) {
            return Ok(false);
        }
        save_servers(servers)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(true)
    }
}

impl ServerStore for LocalServers {
    /// Nothing to read again: the file is this process's own.
    fn refresh(&self) -> StoreFuture<()> {
        Box::pin(async { Ok(()) })
    }

    fn list(&self) -> StoreFuture<Vec<RedisServer>> {
        Box::pin(async { get_servers().map_err(|e| io::Error::other(e.to_string())) })
    }

    fn put(&self, server: RedisServer) -> StoreFuture<()> {
        Box::pin(async move {
            Self::edit(move |servers| {
                // In place, so an edit does not move the entry to the end.
                match servers.iter_mut().find(|s| s.id == server.id) {
                    Some(slot) => *slot = server,
                    None => servers.push(server),
                }
                true
            })
            .await
            .map(|_| ())
        })
    }

    fn swap(&self, expected: RedisServer, replacement: Option<RedisServer>) -> StoreFuture<bool> {
        Box::pin(Self::edit(move |servers| {
            let Some(at) = servers.iter().position(|s| s.id == expected.id) else {
                return false;
            };
            if servers[at] != expected {
                return false;
            }
            match replacement {
                Some(replacement) => servers[at] = replacement,
                None => {
                    servers.remove(at);
                }
            }
            true
        }))
    }
}

#[cfg(test)]
pub(crate) mod conformance;

#[cfg(test)]
mod tests {
    use super::*;
    use zedis_core::fs::{get_or_create_config_dir, override_config_dir};

    fn isolate() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-bridge-store-{}", std::process::id())));
    }

    fn entry(id: &str) -> RedisServer {
        RedisServer {
            id: id.to_string(),
            name: id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            ..Default::default()
        }
    }

    /// Overlapping changes to the server list all land — in the list the
    /// bridge serves and in the file it starts from next time. Without
    /// [`LIST_EDIT`], two of them read the same list and the one saved last
    /// dropped the other's entry, and the file writes, done off the request,
    /// could land in either order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn overlapping_edits_all_land_in_the_list_and_the_file() {
        isolate();
        let ids: Vec<String> = (0..64).map(|n| format!("list-edit-{n}")).collect();
        let puts: Vec<_> = ids.iter().map(|id| tokio::spawn(LocalServers.put(entry(id)))).collect();
        for put in puts {
            put.await.expect("task").expect("put");
        }

        let listed = LocalServers.list().await.expect("list");
        let file = std::fs::read_to_string(get_or_create_config_dir().expect("dir").join("redis-servers.toml"))
            .expect("server file");
        for id in &ids {
            assert!(listed.iter().any(|s| &s.id == id), "{id} is missing from the list");
            assert!(
                file.contains(&format!("id = \"{id}\"")),
                "{id} is missing from the file"
            );
        }
    }

    /// A swap changes an entry only while it is the one the caller saw: a
    /// roll-back or a delete never takes away an edit made since.
    #[tokio::test]
    async fn a_swap_leaves_an_entry_someone_changed_alone() {
        isolate();
        let store = LocalServers;
        let first = entry("swap-target");
        store.put(first.clone()).await.expect("put");
        let edited = RedisServer {
            name: "edited meanwhile".to_string(),
            ..first.clone()
        };
        store.put(edited.clone()).await.expect("edit");

        assert!(
            !store.swap(first.clone(), None).await.expect("swap"),
            "not the entry it saw"
        );
        let still = |list: &[RedisServer]| list.iter().find(|s| s.id == "swap-target").cloned();
        assert_eq!(still(&store.list().await.expect("list")), Some(edited.clone()));

        assert!(store.swap(edited, Some(first.clone())).await.expect("swap"));
        assert_eq!(still(&store.list().await.expect("list")), Some(first.clone()));
        assert!(store.swap(first, None).await.expect("swap"));
        assert_eq!(still(&store.list().await.expect("list")), None);
    }
}
