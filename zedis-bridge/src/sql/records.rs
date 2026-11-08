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

//! What each process also holds in memory — the accounts, the server list,
//! the logins — and the leases beside them: the setup token and the failed
//! sign-ins.

use super::statements::{self as st};
use super::{SqlStore, counter, db, stored};
use crate::accounts::{AccountOp, AccountRecord, InviteRecord, SignupRecord};
use crate::auth::{Login, LoginChange, hex};
use crate::presence::ProcessPresence;
use crate::store::{
    AccountStore, AccountsAt, LoginStore, PresenceStore, ServerStore, SetupStore, StoreFuture, ThrottleStore,
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashMap;
use std::io;
use zedis_connection::{RedisServer, get_servers, open_server_secrets, seal_server_secrets, set_servers_cache};

/// A record as its document.
fn doc<T: serde::Serialize>(record: &T) -> io::Result<String> {
    serde_json::to_string(record).map_err(io::Error::other)
}

/// A document as its record.
fn record<T: serde::de::DeserializeOwned>(doc: &str) -> io::Result<T> {
    serde_json::from_str(doc).map_err(io::Error::other)
}

fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

fn now_secs() -> i64 {
    stored(crate::auth::now_secs())
}

// ---- accounts ---------------------------------------------------------------------

impl AccountStore for SqlStore {
    /// The version first, then the records: a change landing between the
    /// two makes the records newer than the version says, so the next
    /// change made on them is refused and read again — never the other way
    /// round, which would let a change pass on records it never saw.
    fn load(&self) -> StoreFuture<AccountsAt> {
        let store = self.clone();
        Box::pin(async move {
            let version: i64 = sqlx::query(store.sql(&st::VERSION_OF))
                .bind("accounts")
                .fetch_one(&store.pool)
                .await
                .map_err(db)?
                .try_get(0)
                .map_err(db)?;
            let accounts = sqlx::query(store.sql(&st::ACCOUNTS))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?
                .iter()
                .map(|row| record::<AccountRecord>(&row.try_get::<String, _>(0).map_err(db)?))
                .collect::<io::Result<Vec<_>>>()?;
            let invites = sqlx::query(store.sql(&st::INVITES))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?
                .iter()
                .map(|row| record::<InviteRecord>(&row.try_get::<String, _>(0).map_err(db)?))
                .collect::<io::Result<Vec<_>>>()?;
            let signups = sqlx::query(store.sql(&st::SIGNUPS))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?
                .iter()
                .map(|row| record::<SignupRecord>(&row.try_get::<String, _>(0).map_err(db)?))
                .collect::<io::Result<Vec<_>>>()?;
            Ok(AccountsAt {
                version: counter(version),
                accounts,
                invites,
                signups,
            })
        })
    }

    fn version(&self) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let version: i64 = sqlx::query(store.sql(&st::VERSION_OF))
                .bind("accounts")
                .fetch_one(&store.pool)
                .await
                .map_err(db)?
                .try_get(0)
                .map_err(db)?;
            Ok(counter(version))
        })
    }

    /// The counter moves first, and only from `seen`: the row stays locked
    /// to the end of the transaction, so a second process changing the
    /// accounts waits, then finds the counter moved and is refused.
    fn apply(&self, seen: u64, ops: Vec<AccountOp>) -> StoreFuture<Option<u64>> {
        let store = self.clone();
        Box::pin(async move {
            let mut tx = store.pool.begin().await.map_err(db)?;
            let moved = sqlx::query(store.sql(&st::VERSION_BUMP_FROM))
                .bind("accounts")
                .bind(stored(seen))
                .execute(&mut *tx)
                .await
                .map_err(db)?
                .rows_affected();
            if moved == 0 {
                tx.rollback().await.map_err(db)?;
                return Ok(None);
            }
            for op in ops {
                let query = match &op {
                    AccountOp::PutAccount(account) => sqlx::query(store.sql(&st::ACCOUNT_PUT))
                        .bind(account.name.clone())
                        .bind(doc(account)?),
                    AccountOp::RemoveAccount(name) => sqlx::query(store.sql(&st::ACCOUNT_DELETE)).bind(name.clone()),
                    AccountOp::PutInvite(invite) => sqlx::query(store.sql(&st::INVITE_PUT))
                        .bind(invite.id.clone())
                        .bind(stored(invite.expires_at))
                        .bind(doc(invite)?),
                    AccountOp::RemoveInvite(id) => sqlx::query(store.sql(&st::INVITE_DELETE)).bind(id.clone()),
                    AccountOp::PutSignup(request) => sqlx::query(store.sql(&st::SIGNUP_PUT))
                        .bind(request.id.clone())
                        .bind(stored(request.expires_at))
                        .bind(doc(request)?),
                    AccountOp::RemoveSignup(id) => sqlx::query(store.sql(&st::SIGNUP_DELETE)).bind(id.clone()),
                };
                query.execute(&mut *tx).await.map_err(db)?;
            }
            tx.commit().await.map_err(db)?;
            Ok(Some(seen + 1))
        })
    }
}

// ---- logins --------------------------------------------------------------------------

/// A login as its row keeps it: the last use is the column's, which only
/// moves forward, not the document's.
fn login_of(row: &sqlx::any::AnyRow) -> io::Result<(String, Login)> {
    let key: String = row.try_get(0).map_err(db)?;
    let last_used: i64 = row.try_get(1).map_err(db)?;
    let mut login: Login = record(&row.try_get::<String, _>(2).map_err(db)?)?;
    login.last_used = counter(last_used);
    Ok((key, login))
}

impl LoginStore for SqlStore {
    fn load(&self) -> StoreFuture<HashMap<String, Login>> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::LOGINS))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?
                .iter()
                .map(login_of)
                .collect()
        })
    }

    fn get(&self, key: String) -> StoreFuture<Option<Login>> {
        let store = self.clone();
        Box::pin(async move {
            let row = sqlx::query(store.sql(&st::LOGIN_GET))
                .bind(key)
                .fetch_optional(&store.pool)
                .await
                .map_err(db)?;
            row.as_ref()
                .map(login_of)
                .transpose()
                .map(|found| found.map(|(_, login)| login))
        })
    }

    /// A login that went — a sign-out, an expiry — moves `logins_revoked`,
    /// on which every process drops the logins it holds.
    fn save(&self, changes: Vec<(String, LoginChange)>) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let mut tx = store.pool.begin().await.map_err(db)?;
            let mut revoked = false;
            for (key, change) in changes {
                match change {
                    LoginChange::Opened(login) => {
                        sqlx::query(store.sql(&st::LOGIN_PUT))
                            .bind(key)
                            .bind(login.account.clone())
                            .bind(stored(login.last_used))
                            .bind(doc(&login)?)
                            .execute(&mut *tx)
                            .await
                            .map_err(db)?;
                    }
                    LoginChange::Used(login) => {
                        sqlx::query(store.sql(&st::LOGIN_USED))
                            .bind(stored(login.last_used))
                            .bind(key)
                            .execute(&mut *tx)
                            .await
                            .map_err(db)?;
                    }
                    LoginChange::Closed => {
                        let gone = sqlx::query(store.sql(&st::LOGIN_DELETE))
                            .bind(key)
                            .execute(&mut *tx)
                            .await
                            .map_err(db)?
                            .rows_affected();
                        revoked |= gone > 0;
                    }
                }
            }
            if revoked {
                store.bump(&mut tx, "logins_revoked").await?;
            }
            tx.commit().await.map_err(db)
        })
    }
}

// ---- the setup token ------------------------------------------------------------------

impl SetupStore for SqlStore {
    fn waiting(&self) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let count: i64 = sqlx::query(store.sql(&st::SETUP_COUNT))
                .fetch_one(&store.pool)
                .await
                .map_err(db)?
                .try_get(0)
                .map_err(db)?;
            Ok(count > 0)
        })
    }

    fn accepts(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let row = sqlx::query(store.sql(&st::SETUP_GET))
                .fetch_optional(&store.pool)
                .await
                .map_err(db)?;
            let Some(row) = row else {
                return Ok(false);
            };
            let kept: String = row.try_get(0).map_err(db)?;
            Ok(crate::auth::same(kept.as_bytes(), sha256_hex(&token).as_bytes()))
        })
    }

    fn put(&self, token: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::SETUP_PUT))
                .bind(sha256_hex(&token))
                .bind(now_secs())
                .execute(&store.pool)
                .await
                .map_err(db)?;
            Ok(())
        })
    }

    fn put_if_none(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let kept = sqlx::query(store.sql(&st::SETUP_PUT_IF_NONE))
                .bind(sha256_hex(&token))
                .bind(now_secs())
                .execute(&store.pool)
                .await
                .map_err(db)?
                .rows_affected();
            Ok(kept > 0)
        })
    }

    /// Never: only the token's hash is kept.
    fn shown(&self) -> StoreFuture<Option<String>> {
        Box::pin(async { Ok(None) })
    }

    fn take(&self) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::SETUP_TAKE))
                .execute(&store.pool)
                .await
                .map_err(db)?;
            Ok(())
        })
    }

    fn location(&self) -> String {
        "the database".to_string()
    }
}

// ---- failed sign-ins ------------------------------------------------------------------------

impl ThrottleStore for SqlStore {
    fn failed(&self, process: String, key: String, at_ms: i64) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::THROTTLE_ADD))
                .bind(process)
                .bind(sha256_hex(&key))
                .bind(key)
                .bind(at_ms)
                .execute(&store.pool)
                .await
                .map_err(db)?;
            Ok(())
        })
    }

    fn succeeded(&self, key: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::THROTTLE_CLEAR))
                .bind(sha256_hex(&key))
                .execute(&store.pool)
                .await
                .map_err(db)?;
            Ok(())
        })
    }

    fn others(&self, process: String, since_ms: i64) -> StoreFuture<Vec<(String, i64)>> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::THROTTLE_OTHERS))
                .bind(process)
                .bind(since_ms)
                .fetch_all(&store.pool)
                .await
                .map_err(db)?
                .iter()
                .map(|row| Ok((row.try_get(0).map_err(db)?, row.try_get(1).map_err(db)?)))
                .collect()
        })
    }

    fn prune(&self, before_ms: i64) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            Ok(sqlx::query(store.sql(&st::THROTTLE_PRUNE))
                .bind(before_ms)
                .execute(&store.pool)
                .await
                .map_err(db)?
                .rows_affected())
        })
    }
}

// ---- online client presence ---------------------------------------------------------------

const PRESENCE_META_PREFIX: &str = "presence:";

impl PresenceStore for SqlStore {
    fn put(&self, process: String, snapshot: ProcessPresence) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            sqlx::query(store.sql(&st::META_PUT))
                .bind(format!("{PRESENCE_META_PREFIX}{process}"))
                .bind(doc(&snapshot)?)
                .execute(&store.pool)
                .await
                .map_err(db)?;
            Ok(())
        })
    }

    fn others(&self, process: String, since_secs: u64) -> StoreFuture<Vec<ProcessPresence>> {
        let store = self.clone();
        Box::pin(async move {
            let own_name = format!("{PRESENCE_META_PREFIX}{process}");
            let rows = sqlx::query(store.sql(&st::PRESENCE_LIST))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?;
            let mut out = Vec::new();
            for row in rows {
                let name: String = row.try_get(0).map_err(db)?;
                if name == own_name {
                    continue;
                }
                let raw: String = row.try_get(1).map_err(db)?;
                if let Ok(snapshot) = record::<ProcessPresence>(&raw)
                    && snapshot.updated_at >= since_secs
                {
                    out.push(snapshot);
                }
            }
            Ok(out)
        })
    }

    fn prune(&self, before_secs: u64) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let rows = sqlx::query(store.sql(&st::PRESENCE_LIST))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?;
            let mut pruned = 0u64;
            for row in rows {
                let name: String = row.try_get(0).map_err(db)?;
                let raw: String = row.try_get(1).map_err(db)?;
                let expired = match record::<ProcessPresence>(&raw) {
                    Ok(snap) => snap.updated_at < before_secs,
                    Err(_) => true,
                };
                if expired {
                    pruned += sqlx::query(store.sql(&st::META_DELETE))
                        .bind(name)
                        .execute(&store.pool)
                        .await
                        .map_err(db)?
                        .rows_affected();
                }
            }
            Ok(pruned)
        })
    }
}

// ---- the server list ------------------------------------------------------------------------

/// An entry as the database keeps it: its secrets sealed.
fn sealed_doc(server: &RedisServer) -> io::Result<String> {
    let mut sealed = server.clone();
    seal_server_secrets(&mut sealed).map_err(|e| io::Error::other(e.to_string()))?;
    doc(&sealed)
}

/// An entry as the cache holds it: its secrets opened.
fn opened(doc: &str) -> io::Result<RedisServer> {
    let mut server: RedisServer = record(doc)?;
    open_server_secrets(&mut server);
    Ok(server)
}

/// The cache, with `id`'s entry replaced by `entry` (or gone, for `None`).
fn cache_with(id: &str, entry: Option<RedisServer>) {
    let mut servers = get_servers().unwrap_or_default();
    servers.retain(|server| server.id != id);
    servers.extend(entry);
    set_servers_cache(servers);
}

impl SqlStore {
    async fn read_servers(&self) -> io::Result<Vec<RedisServer>> {
        sqlx::query(self.sql(&st::SERVERS))
            .fetch_all(&self.pool)
            .await
            .map_err(db)?
            .iter()
            .map(|row| opened(&row.try_get::<String, _>(0).map_err(db)?))
            .collect()
    }
}

impl ServerStore for SqlStore {
    fn refresh(&self) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            set_servers_cache(store.read_servers().await?);
            Ok(())
        })
    }

    /// Read from the database, and into the cache with it — the first call,
    /// at start-up, is what fills the cache.
    fn list(&self) -> StoreFuture<Vec<RedisServer>> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            let servers = store.read_servers().await?;
            set_servers_cache(servers.clone());
            Ok(servers)
        })
    }

    fn put(&self, server: RedisServer) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            let mut tx = store.pool.begin().await.map_err(db)?;
            sqlx::query(store.sql(&st::SERVER_PUT))
                .bind(server.id.clone())
                .bind(server.owner.clone())
                .bind(sealed_doc(&server)?)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            store.bump(&mut tx, "servers").await?;
            tx.commit().await.map_err(db)?;
            cache_with(&server.id.clone(), Some(server));
            Ok(())
        })
    }

    /// The entry is read `FOR UPDATE` and compared opened — a seal has its
    /// own nonce, so two seals of one entry never match as text.
    fn swap(&self, expected: RedisServer, replacement: Option<RedisServer>) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            let mut tx = store.pool.begin().await.map_err(db)?;
            let row = sqlx::query(store.sql(&st::SERVER_FOR_UPDATE))
                .bind(expected.id.clone())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            let current = row
                .map(|row| row.try_get::<String, _>(0).map_err(db).and_then(|doc| opened(&doc)))
                .transpose()?;
            if current.as_ref() != Some(&expected) {
                tx.rollback().await.map_err(db)?;
                return Ok(false);
            }
            match &replacement {
                Some(server) => sqlx::query(store.sql(&st::SERVER_REPLACE))
                    .bind(server.owner.clone())
                    .bind(sealed_doc(server)?)
                    .bind(expected.id.clone()),
                None => sqlx::query(store.sql(&st::SERVER_DELETE)).bind(expected.id.clone()),
            }
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            store.bump(&mut tx, "servers").await?;
            tx.commit().await.map_err(db)?;
            cache_with(&expected.id, replacement);
            Ok(true)
        })
    }
}
