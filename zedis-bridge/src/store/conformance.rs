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

//! What every implementation of the stores answers alike (ADR 18): run here
//! on the local directory, and by `sql::tests` on a database. Every name is
//! the run's own, so a database other runs used is no obstacle.
//!
//! The server list is left to each implementation's own tests: the local
//! one writes the configuration directory every test in the process shares.

use super::Stores;
use crate::accounts::{AccountOp, AccountRecord, InviteKind, InviteRecord, Role, SignupRecord};
use crate::auth::{Login, LoginChange};
use crate::page_data::{KeyNote, MAX_ACCOUNT_BYTES, RowError, ScriptRecord};
use crate::presence::{ClientKey, ProcessPresence, SharedClient};
use crate::trash::{MAX_PAYLOAD, NewEntry};
use serde_json::value::RawValue;

/// A name no other run uses.
fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Every check, in turn.
pub(crate) async fn keeps_the_contract(stores: &Stores) {
    accounts(stores).await;
    logins(stores).await;
    setup_token(stores).await;
    recycle_bin(stores).await;
    metrics(stores).await;
    page_rows(stores).await;
    key_notes(stores).await;
    scripts(stores).await;
    presence(stores).await;
}

/// A change applies against the version it was decided on, and two names
/// that differ only in case are two accounts.
async fn accounts(stores: &Stores) {
    let store = &stores.accounts;
    let at = store.load().await.expect("load");
    let lower = unique("alice");
    let upper = lower.to_uppercase();
    let record = |name: &str| AccountRecord {
        name: name.to_string(),
        role: Role::Full,
        version: 1,
        ..AccountRecord::default()
    };
    let invite = InviteRecord {
        id: unique("invite"),
        code_sha256: "00".repeat(32),
        kind: InviteKind::Reset { account: lower.clone() },
        expires_at: 4_000_000_000,
        created_by: "conformance".to_string(),
        created_at: 1,
    };
    let request = SignupRecord {
        id: unique("signup"),
        name: lower.clone(),
        password: "$argon2id$v=19$m=8,t=1,p=1$c2FsdA$aGFzaA".to_string(),
        note: "for the on-call rota — ünïcode kept".to_string(),
        peer: Some("10.0.0.7".to_string()),
        requested_at: 1,
        expires_at: 4_000_000_000,
    };
    let applied = store
        .apply(
            at.version,
            vec![
                AccountOp::PutAccount(record(&lower)),
                AccountOp::PutAccount(record(&upper)),
                AccountOp::PutInvite(invite.clone()),
                AccountOp::PutSignup(request.clone()),
            ],
        )
        .await
        .expect("apply");
    let Some(version) = applied else {
        panic!("a change on the version it was decided on applies");
    };
    assert!(version > at.version);
    assert_eq!(
        store
            .apply(at.version, vec![AccountOp::RemoveAccount(lower.clone())])
            .await
            .expect("apply"),
        None,
        "a change decided on an older version is refused"
    );

    let now = store.load().await.expect("load");
    assert_eq!(now.version, version);
    let names: Vec<&str> = now.accounts.iter().map(|a| a.name.as_str()).collect();
    assert!(
        names.contains(&lower.as_str()) && names.contains(&upper.as_str()),
        "case is kept apart"
    );
    assert!(now.invites.contains(&invite));
    assert!(now.signups.contains(&request), "a request is kept as it was asked");

    let removed = store
        .apply(
            version,
            vec![
                AccountOp::RemoveAccount(lower.clone()),
                AccountOp::RemoveAccount(upper.clone()),
                AccountOp::RemoveInvite(invite.id.clone()),
                AccountOp::RemoveSignup(request.id.clone()),
            ],
        )
        .await
        .expect("apply");
    assert!(removed.is_some());
    let now = store.load().await.expect("load");
    assert!(!now.accounts.iter().any(|a| a.name == lower || a.name == upper));
    assert!(!now.invites.iter().any(|i| i.id == invite.id));
    assert!(!now.signups.iter().any(|r| r.id == request.id));
}

async fn logins(stores: &Stores) {
    let store = &stores.logins;
    let key = unique("login");
    let login = Login {
        account: "alice".to_string(),
        version: 3,
        last_used: 1_700_000_000,
        remember: true,
    };
    let used = |last_used| {
        LoginChange::Used(Login {
            last_used,
            ..login.clone()
        })
    };
    store
        .save(vec![(key.clone(), LoginChange::Opened(login.clone()))])
        .await
        .expect("save");
    assert_eq!(store.load().await.expect("load").get(&key), Some(&login));
    store
        .save(vec![(key.clone(), used(login.last_used + 60))])
        .await
        .expect("save");
    store
        .save(vec![(key.clone(), used(login.last_used + 30))])
        .await
        .expect("save");
    let kept = store.load().await.expect("load")[&key].last_used;
    assert_eq!(kept, login.last_used + 60, "a use never moves the time back");

    store
        .save(vec![(key.clone(), LoginChange::Closed)])
        .await
        .expect("save");
    store
        .save(vec![(key.clone(), used(login.last_used + 90))])
        .await
        .expect("save");
    assert!(
        !store.load().await.expect("load").contains_key(&key),
        "a use does not put back a login that was signed out"
    );
}

async fn setup_token(stores: &Stores) {
    let store = &stores.setup;
    store.take().await.expect("take");
    assert!(!store.waiting().await.expect("waiting"));
    assert!(store.put_if_none("first".to_string()).await.expect("put"));
    assert!(
        !store.put_if_none("second".to_string()).await.expect("put"),
        "one at a time"
    );
    assert!(store.waiting().await.expect("waiting"));
    assert!(store.accepts("first".to_string()).await.expect("accepts"));
    assert!(!store.accepts("second".to_string()).await.expect("accepts"));
    store.put("third".to_string()).await.expect("put");
    assert!(store.accepts("third".to_string()).await.expect("accepts"));
    store.take().await.expect("take");
    assert!(!store.waiting().await.expect("waiting"));
}

/// A payload as large as the bin takes comes back byte for byte.
async fn recycle_bin(stores: &Stores) {
    let store = &stores.trash;
    let server = unique("server");
    let payload: Vec<u8> = (0..MAX_PAYLOAD).map(|n| (n % 251) as u8).collect();
    let meta = store
        .put(
            server.clone(),
            NewEntry {
                key: "user:1".to_string(),
                db: 2,
                pttl_ms: -1,
                deleted_by: "alice".to_string(),
                payload: payload.clone(),
                size: payload.len() as u64,
                sealed: false,
            },
        )
        .await
        .expect("put");
    let second = store
        .put(
            server.clone(),
            NewEntry {
                key: "user:2".to_string(),
                db: 0,
                pttl_ms: 5_000,
                deleted_by: "bob".to_string(),
                payload: b"small".to_vec(),
                size: 5,
                sealed: true,
            },
        )
        .await
        .expect("put");
    let listed = store.list(server.clone()).await.expect("list");
    assert_eq!(listed, vec![second.clone(), meta.clone()], "newest first");
    let (got, bytes) = store
        .get(server.clone(), meta.id.clone())
        .await
        .expect("get")
        .expect("there");
    assert_eq!(got, meta);
    assert!(bytes == payload, "the payload comes back whole");
    assert_eq!(
        store.remove(server.clone(), meta.id.clone()).await.expect("remove"),
        Some(meta.clone())
    );
    assert_eq!(store.get(server.clone(), meta.id).await.expect("get"), None);
    store.remove_server(server.clone()).await.expect("remove server");
    assert!(store.list(server).await.expect("list").is_empty());
}

/// One sample a minute: a second for the same minute, or a late one for an
/// earlier minute, is not kept.
async fn metrics(stores: &Stores) {
    let store = &stores.metrics;
    let server = unique("server");
    let now = crate::metrics::now_ms();
    let minute = crate::metrics::INTERVAL_MS;
    let at = now - now.rem_euclid(minute);
    assert!(
        store
            .put(server.clone(), at, r#"{"n":1}"#.to_string())
            .await
            .expect("put")
    );
    assert!(
        !store
            .put(server.clone(), at + 1, r#"{"n":2}"#.to_string())
            .await
            .expect("put")
    );
    assert!(
        !store
            .put(server.clone(), at - minute, r#"{"n":0}"#.to_string())
            .await
            .expect("put")
    );
    assert!(
        store.put(server.clone(), at - 1, "[]".to_string()).await.is_err(),
        "an object only"
    );
    let samples = store.since(server.clone(), at - 1, 100).await.expect("since");
    let texts: Vec<&str> = samples.iter().map(|s| RawValue::get(s)).collect();
    assert_eq!(texts, vec![r#"{"n":1}"#]);
    store.remove_server(server.clone()).await.expect("remove server");
    assert!(store.since(server, 0, 100).await.expect("since").is_empty());
}

/// An account's rows, and the bound on all of them together.
async fn page_rows(stores: &Stores) {
    let store = &stores.page_rows;
    let account = unique("account");
    store
        .put(
            account.clone(),
            "favorites".to_string(),
            "srv".to_string(),
            Some("[\"a\"]".to_string()),
        )
        .await
        .expect("put");
    store
        .put(
            account.clone(),
            "preferences".to_string(),
            "zedis.toml".to_string(),
            Some("x = 1".to_string()),
        )
        .await
        .expect("put");
    let rows = store.rows(account.clone()).await.expect("rows");
    assert_eq!(rows["favorites"]["srv"], "[\"a\"]");
    assert_eq!(rows["preferences"]["zedis.toml"], "x = 1");

    let big = "x".repeat(256 * 1024);
    let mut refused = false;
    for n in 0..=(MAX_ACCOUNT_BYTES / big.len()) {
        match store
            .put(
                account.clone(),
                "recent_keys".to_string(),
                format!("db/{n}"),
                Some(big.clone()),
            )
            .await
        {
            Ok(()) => {}
            Err(RowError::TooLarge) => {
                refused = true;
                break;
            }
            Err(RowError::Store(e)) => panic!("put: {e}"),
        }
    }
    assert!(refused, "an account keeps at most {MAX_ACCOUNT_BYTES} bytes");
    let rows = store.rows(account.clone()).await.expect("rows");
    let total: usize = rows.values().flatten().map(|(k, v)| k.len() + v.len()).sum();
    assert!(total <= MAX_ACCOUNT_BYTES, "the refused row is not kept");

    store
        .put(account.clone(), "favorites".to_string(), "srv".to_string(), None)
        .await
        .expect("delete");
    assert!(
        !store
            .rows(account.clone())
            .await
            .expect("rows")
            .contains_key("favorites")
    );
    store.forget(account.clone()).await.expect("forget");
    assert!(store.rows(account).await.expect("rows").is_empty());
}

/// A key name past any index's limit, and two keys that differ in case.
async fn key_notes(stores: &Stores) {
    let store = &stores.key_notes;
    let server = unique("server");
    let note = |text: &str| KeyNote {
        tag: Some("red".to_string()),
        note: text.to_string(),
        by: "alice".to_string(),
        at: 1,
    };
    let long = "k".repeat(4096);
    store
        .put(server.clone(), long.clone(), Some(note("long")))
        .await
        .expect("put");
    store
        .put(server.clone(), "A".to_string(), Some(note("upper")))
        .await
        .expect("put");
    store
        .put(server.clone(), "a".to_string(), Some(note("lower")))
        .await
        .expect("put");
    store
        .put(server.clone(), "a".to_string(), Some(note("lower, again")))
        .await
        .expect("put");
    let notes = store.list(server.clone()).await.expect("list");
    assert_eq!(notes.len(), 3);
    assert_eq!(notes[&long].note, "long");
    assert_eq!(notes["A"].note, "upper");
    assert_eq!(notes["a"].note, "lower, again");
    store.put(server.clone(), "A".to_string(), None).await.expect("delete");
    assert!(!store.list(server.clone()).await.expect("list").contains_key("A"));
    store.remove_server(server.clone()).await.expect("remove server");
    assert!(store.list(server).await.expect("list").is_empty());
}

async fn scripts(stores: &Stores) {
    let store = &stores.scripts;
    let id = unique("script");
    let script = |owner: Option<&str>| ScriptRecord {
        id: id.clone(),
        owner: owner.map(str::to_string),
        script: RawValue::from_string(r#"{"name":"top","code":"return 1"}"#.to_string()).expect("json"),
        updated_by: "alice".to_string(),
        updated_at: 2,
    };
    store.put(script(Some("alice"))).await.expect("put");
    store.put(script(None)).await.expect("put");
    let listed = store.list().await.expect("list");
    let kept = listed.iter().find(|s| s.id == id).expect("listed");
    assert_eq!(kept.owner, None, "put in place");
    assert_eq!(kept.name(), "top");
    let removed = store.remove(id.clone()).await.expect("remove").expect("there");
    assert_eq!(removed.id, id);
    assert!(store.remove(id).await.expect("remove").is_none());
}

async fn presence(stores: &Stores) {
    let Some(store) = &stores.presence else {
        return;
    };
    let proc_a = unique("proc-a");
    let proc_b = unique("proc-b");
    let snap_a = ProcessPresence {
        updated_at: 1_000,
        clients: vec![SharedClient {
            key: ClientKey::Browser("login-a".to_string()),
            account: "alice".to_string(),
            peer: Some("10.0.0.1".to_string()),
            forwarded_for: None,
            seen_at: 995,
            servers: vec![("prod".to_string(), 995)],
        }],
    };
    let snap_b = ProcessPresence {
        updated_at: 2_000,
        clients: vec![SharedClient {
            key: ClientKey::Browser("login-b".to_string()),
            account: "bob".to_string(),
            peer: Some("10.0.0.2".to_string()),
            forwarded_for: None,
            seen_at: 1_990,
            servers: vec![],
        }],
    };
    store.put(proc_a.clone(), snap_a.clone()).await.expect("put a");
    store.put(proc_b.clone(), snap_b.clone()).await.expect("put b");

    let seen_by_a = store.others(proc_a.clone(), 1_500).await.expect("others for a");
    assert!(seen_by_a.contains(&snap_b), "process A sees process B's fresh snapshot");
    assert!(!seen_by_a.contains(&snap_a), "process A does not read its own snapshot");

    let pruned = store.prune(1_500).await.expect("prune");
    assert!(pruned >= 1, "stale snapshot before 1500 is pruned");
    let after = store.others(proc_b.clone(), 0).await.expect("others for b");
    assert!(!after.contains(&snap_a), "pruned snapshot is gone");
    let _ = store.prune(u64::MAX).await;
}

/// The local directory, which `make test` always has.
#[tokio::test]
async fn the_local_directory_keeps_the_contract() {
    let dir = std::env::temp_dir().join(unique("zedis-store-conformance"));
    std::fs::create_dir_all(&dir).expect("dir");
    keeps_the_contract(&Stores::local(&dir)).await;
    let _ = std::fs::remove_dir_all(&dir);
}
