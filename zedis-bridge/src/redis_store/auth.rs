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

use super::*;
use crate::accounts::{AccountOp, AccountRecord, InviteRecord, SignupRecord};
use crate::auth::{Login, LoginChange};
use crate::presence::ProcessPresence;
use crate::store::{
    AccountStore, AccountsAt, LoginStore, PresenceStore, SetupStore, StoreFuture, StoreVersions, ThrottleStore,
    Versions,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

impl StoreVersions for RedisStore {
    fn read(&self) -> StoreFuture<Versions> {
        let store = self.clone();
        Box::pin(async move {
            let key = store.key("versions");
            let pairs: Vec<(String, u64)> = store
                .run(|mut conn| {
                    let key = key.clone();
                    async move { redis::cmd("HGETALL").arg(&key).query_async(&mut conn).await }
                })
                .await?;
            let mut versions = Versions::default();
            for (name, ver) in pairs {
                match name.as_str() {
                    "servers" => versions.servers = ver,
                    "accounts" => versions.accounts = ver,
                    "logins_revoked" => versions.logins_revoked = ver,
                    _ => {}
                }
            }
            Ok(versions)
        })
    }
}

// ---- accounts, invitations and signup requests ---------------------------------

impl AccountStore for RedisStore {
    fn load(&self) -> StoreFuture<AccountsAt> {
        let store = self.clone();
        Box::pin(async move {
            let versions_key = store.key("versions");
            let accounts_key = store.key("accounts");
            let invites_key = store.key("invites");
            let signups_key = store.key("signups");
            type RawLoaded = (
                Option<u64>,
                Vec<(String, String)>,
                Vec<(String, String)>,
                Vec<(String, String)>,
            );
            let (version, raw_accounts, raw_invites, raw_signups): RawLoaded = store
                .run(|mut conn| {
                    let versions_key = versions_key.clone();
                    let accounts_key = accounts_key.clone();
                    let invites_key = invites_key.clone();
                    let signups_key = signups_key.clone();
                    async move {
                        redis::pipe()
                            .cmd("HGET")
                            .arg(&versions_key)
                            .arg("accounts")
                            .cmd("HGETALL")
                            .arg(&accounts_key)
                            .cmd("HGETALL")
                            .arg(&invites_key)
                            .cmd("HGETALL")
                            .arg(&signups_key)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;

            let mut accounts = Vec::with_capacity(raw_accounts.len());
            for (_, doc) in raw_accounts {
                accounts.push(from_json::<AccountRecord>(&doc)?);
            }
            accounts.sort_by(|a, b| a.name.cmp(&b.name));

            let mut invites = Vec::with_capacity(raw_invites.len());
            for (_, doc) in raw_invites {
                invites.push(from_json::<InviteRecord>(&doc)?);
            }
            invites.sort_by(|a, b| a.id.cmp(&b.id));

            let mut signups = Vec::with_capacity(raw_signups.len());
            for (_, doc) in raw_signups {
                signups.push(from_json::<SignupRecord>(&doc)?);
            }
            signups.sort_by(|a, b| a.id.cmp(&b.id));

            Ok(AccountsAt {
                version: version.unwrap_or(0),
                accounts,
                invites,
                signups,
            })
        })
    }

    fn version(&self) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let versions_key = store.key("versions");
            let ver: Option<u64> = store
                .run(|mut conn| {
                    let versions_key = versions_key.clone();
                    async move {
                        redis::cmd("HGET")
                            .arg(&versions_key)
                            .arg("accounts")
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(ver.unwrap_or(0))
        })
    }

    fn apply(&self, seen: u64, ops: Vec<AccountOp>) -> StoreFuture<Option<u64>> {
        let store = self.clone();
        Box::pin(async move {
            let mut encoded_ops: Vec<(String, String, String)> = Vec::with_capacity(ops.len());
            for op in ops {
                match op {
                    AccountOp::PutAccount(record) => {
                        let doc = to_json(&record)?;
                        encoded_ops.push(("put_account".to_string(), record.name, doc));
                    }
                    AccountOp::RemoveAccount(name) => {
                        encoded_ops.push(("rem_account".to_string(), name, String::new()));
                    }
                    AccountOp::PutInvite(invite) => {
                        let doc = to_json(&invite)?;
                        encoded_ops.push(("put_invite".to_string(), invite.id, doc));
                    }
                    AccountOp::RemoveInvite(id) => {
                        encoded_ops.push(("rem_invite".to_string(), id, String::new()));
                    }
                    AccountOp::PutSignup(signup) => {
                        let doc = to_json(&signup)?;
                        encoded_ops.push(("put_signup".to_string(), signup.id, doc));
                    }
                    AccountOp::RemoveSignup(id) => {
                        encoded_ops.push(("rem_signup".to_string(), id, String::new()));
                    }
                }
            }

            let versions_key = store.key("versions");
            let accounts_key = store.key("accounts");
            let invites_key = store.key("invites");
            let signups_key = store.key("signups");
            let script = redis::Script::new(ACCOUNT_APPLY_LUA);
            let res: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let versions_key = versions_key.clone();
                    let accounts_key = accounts_key.clone();
                    let invites_key = invites_key.clone();
                    let signups_key = signups_key.clone();
                    let encoded_ops = encoded_ops.clone();
                    async move {
                        let mut inv = script.key(&versions_key);
                        inv.key(&accounts_key)
                            .key(&invites_key)
                            .key(&signups_key)
                            .arg(seen)
                            .arg(INVITES_SIGNUPS_TTL_SECS);
                        for (kind, k, v) in &encoded_ops {
                            inv.arg(kind).arg(k).arg(v);
                        }
                        inv.invoke_async(&mut conn).await
                    }
                })
                .await?;
            Ok(u64::try_from(res).ok())
        })
    }
}

// ---- logins, setup token and throttle ------------------------------------------

impl LoginStore for RedisStore {
    fn load(&self) -> StoreFuture<HashMap<String, Login>> {
        let store = self.clone();
        Box::pin(async move {
            let logins_key = store.key("logins");
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let logins_key = logins_key.clone();
                    async move { redis::cmd("HGETALL").arg(&logins_key).query_async(&mut conn).await }
                })
                .await?;
            let mut logins = HashMap::with_capacity(pairs.len());
            for (key, doc) in pairs {
                let login = from_json::<Login>(&doc)?;
                logins.insert(key, login);
            }
            Ok(logins)
        })
    }

    fn get(&self, key: String) -> StoreFuture<Option<Login>> {
        let store = self.clone();
        Box::pin(async move {
            let logins_key = store.key("logins");
            let doc: Option<String> = store
                .run(|mut conn| {
                    let logins_key = logins_key.clone();
                    let key = key.clone();
                    async move {
                        redis::cmd("HGET")
                            .arg(&logins_key)
                            .arg(&key)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            doc.map(|d| from_json::<Login>(&d)).transpose()
        })
    }

    fn save(&self, changes: Vec<(String, LoginChange)>) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            if changes.is_empty() {
                return Ok(());
            }
            let mut args: Vec<(String, String, u64, String)> = Vec::with_capacity(changes.len());
            for (key, change) in changes {
                match change {
                    LoginChange::Opened(login) => {
                        let doc = to_json(&login)?;
                        args.push(("open".to_string(), key, login.last_used, doc));
                    }
                    LoginChange::Used(login) => {
                        args.push(("use".to_string(), key, login.last_used, String::new()));
                    }
                    LoginChange::Closed => {
                        args.push(("close".to_string(), key, 0, String::new()));
                    }
                }
            }
            let logins_key = store.key("logins");
            let versions_key = store.key("versions");
            let script = redis::Script::new(LOGIN_SAVE_LUA);
            let _: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let logins_key = logins_key.clone();
                    let versions_key = versions_key.clone();
                    let args = args.clone();
                    async move {
                        let mut inv = script.key(&logins_key);
                        inv.key(&versions_key).arg(LOGINS_TTL_SECS);
                        for (op, k, ts, doc) in &args {
                            inv.arg(op).arg(k).arg(*ts).arg(doc);
                        }
                        inv.invoke_async(&mut conn).await
                    }
                })
                .await?;
            Ok(())
        })
    }
}

impl SetupStore for RedisStore {
    fn waiting(&self) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let exists: i64 = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("EXISTS").arg(&setup_key).query_async(&mut conn).await }
                })
                .await?;
            Ok(exists > 0)
        })
    }

    fn accepts(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let stored: Option<String> = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("GET").arg(&setup_key).query_async(&mut conn).await }
                })
                .await?;
            let hash = sha256_hex(&token);
            Ok(stored.is_some_and(|s| crate::auth::same(s.as_bytes(), hash.as_bytes())))
        })
    }

    fn put(&self, token: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let hash = sha256_hex(&token);
            store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    let hash = hash.clone();
                    async move {
                        redis::cmd("SET")
                            .arg(&setup_key)
                            .arg(&hash)
                            .arg("EX")
                            .arg(SETUP_TTL_SECS)
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn put_if_none(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let hash = sha256_hex(&token);
            let res: Option<String> = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    let hash = hash.clone();
                    async move {
                        redis::cmd("SET")
                            .arg(&setup_key)
                            .arg(&hash)
                            .arg("NX")
                            .arg("EX")
                            .arg(SETUP_TTL_SECS)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(res.is_some())
        })
    }

    fn shown(&self) -> StoreFuture<Option<String>> {
        Box::pin(async { Ok(None) })
    }

    fn take(&self) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("DEL").arg(&setup_key).query_async::<()>(&mut conn).await }
                })
                .await
        })
    }

    fn location(&self) -> String {
        "the database".to_string()
    }
}

#[derive(Serialize, Deserialize)]
struct ThrottleEntry {
    p: String,
    h: String,
    k: String,
    a: i64,
    n: String,
}

impl ThrottleStore for RedisStore {
    fn failed(&self, process: String, key: String, at_ms: i64) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let entry = ThrottleEntry {
                p: process,
                h: sha256_hex(&key),
                k: key,
                a: at_ms,
                n: Uuid::new_v4().simple().to_string(),
            };
            let doc = to_json(&entry)?;
            let throttle_key = store.key("throttle");
            store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    let doc = doc.clone();
                    async move {
                        redis::pipe()
                            .cmd("ZADD")
                            .arg(&throttle_key)
                            .arg(at_ms as f64)
                            .arg(&doc)
                            .ignore()
                            .cmd("EXPIRE")
                            .arg(&throttle_key)
                            .arg(THROTTLE_TTL_SECS)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn succeeded(&self, key: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let hash = sha256_hex(&key);
            let script = redis::Script::new(THROTTLE_CLEAR_LUA);
            let _: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let throttle_key = throttle_key.clone();
                    let hash = hash.clone();
                    async move { script.key(&throttle_key).arg(&hash).invoke_async(&mut conn).await }
                })
                .await?;
            Ok(())
        })
    }

    fn others(&self, process: String, since_ms: i64) -> StoreFuture<Vec<(String, i64)>> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let raw: Vec<String> = store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    async move {
                        redis::cmd("ZRANGEBYSCORE")
                            .arg(&throttle_key)
                            .arg(since_ms as f64)
                            .arg("+inf")
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            let mut out = Vec::new();
            for doc in raw {
                if let Ok(entry) = from_json::<ThrottleEntry>(&doc)
                    && entry.p != process
                {
                    out.push((entry.k, entry.a));
                }
            }
            Ok(out)
        })
    }

    fn prune(&self, before_ms: i64) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let max_score = format!("({before_ms}");
            let removed: u64 = store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    let max_score = max_score.clone();
                    async move {
                        redis::cmd("ZREMRANGEBYSCORE")
                            .arg(&throttle_key)
                            .arg("-inf")
                            .arg(&max_score)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(removed)
        })
    }
}

impl PresenceStore for RedisStore {
    fn put(&self, process: String, snapshot: ProcessPresence) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let doc = to_json(&snapshot)?;
            let presence_key = store.key("presence");
            store
                .run(|mut conn| {
                    let presence_key = presence_key.clone();
                    let process = process.clone();
                    let doc = doc.clone();
                    async move {
                        redis::pipe()
                            .cmd("HSET")
                            .arg(&presence_key)
                            .arg(&process)
                            .arg(&doc)
                            .ignore()
                            .cmd("EXPIRE")
                            .arg(&presence_key)
                            .arg(PRESENCE_TTL_SECS)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn others(&self, process: String, since_secs: u64) -> StoreFuture<Vec<ProcessPresence>> {
        let store = self.clone();
        Box::pin(async move {
            let presence_key = store.key("presence");
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let presence_key = presence_key.clone();
                    async move { redis::cmd("HGETALL").arg(&presence_key).query_async(&mut conn).await }
                })
                .await?;
            let mut out = Vec::new();
            for (field, doc) in pairs {
                if field != process
                    && let Ok(snapshot) = from_json::<ProcessPresence>(&doc)
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
            let presence_key = store.key("presence");
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let presence_key = presence_key.clone();
                    async move { redis::cmd("HGETALL").arg(&presence_key).query_async(&mut conn).await }
                })
                .await?;
            let mut stale = Vec::new();
            for (field, doc) in pairs {
                let expired = match from_json::<ProcessPresence>(&doc) {
                    Ok(snapshot) => snapshot.updated_at < before_secs,
                    Err(_) => true,
                };
                if expired {
                    stale.push(field);
                }
            }
            if stale.is_empty() {
                return Ok(0);
            }
            let removed: u64 = store
                .run(|mut conn| {
                    let presence_key = presence_key.clone();
                    let stale = stale.clone();
                    async move {
                        let mut cmd = redis::cmd("HDEL");
                        cmd.arg(&presence_key);
                        for field in &stale {
                            cmd.arg(field);
                        }
                        cmd.query_async(&mut conn).await
                    }
                })
                .await?;
            Ok(removed)
        })
    }
}
