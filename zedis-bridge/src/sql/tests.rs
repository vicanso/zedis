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

//! The SQL store against a database: `ZEDIS_IT_STORE_URL=postgres://… cargo
//! test -p zedis-bridge -- --ignored sql::` (or `mysql://…`). Ignored
//! otherwise, like the live Redis tests: a green run without a database
//! has tested nothing here.

use super::*;
use crate::accounts::{AccountChange, AccountOp, AccountRecord, Accounts, Role};
use crate::auth::{Login, LoginChange, Logins, Throttle, attempt_key, hex};
use crate::store::{STORE_POLL, conformance};
use sha2::{Digest, Sha256};
use sqlx::AssertSqlSafe;
use zedis_connection::{RedisServer, disable_keychain, get_server};
use zedis_core::fs::override_config_dir;

/// The database named by `ZEDIS_IT_STORE_URL`, with this machine's test
/// master key: the same directory every run, since the database keeps the
/// check value the first run sealed.
async fn store() -> SqlStore {
    override_config_dir(std::env::temp_dir().join("zedis-it-store"));
    disable_keychain();
    let url = std::env::var("ZEDIS_IT_STORE_URL").expect("ZEDIS_IT_STORE_URL names the database to test against");
    SqlStore::connect(&url).await.expect("connect")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[test]
fn a_store_url_is_logged_without_its_password() {
    assert_eq!(
        redacted("postgres://bridge:s3cret@db.internal:5432/zedis?sslmode=require"),
        "postgres://db.internal:5432/zedis"
    );
    assert_eq!(redacted("mysql://db:3306/zedis"), "mysql://db:3306/zedis");
    assert_eq!(redacted("mysql://u:p@ss@db/zedis"), "mysql://db/zedis");
    assert_eq!(redacted("not a url"), "a store URL");
}

/// The storage guide (`docs/SQL_STORE.md` and its twin) shows the schema as
/// the migrations write it, file for file: a migration added or changed
/// without the guide fails here rather than in a reader's database.
#[test]
fn the_storage_guide_shows_every_migration_as_it_is() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for guide in ["SQL_STORE.md", "SQL_STORE_zh.md"] {
        let text = std::fs::read_to_string(root.join("../docs").join(guide)).expect("the guide");
        // A Windows checkout gives the guide CRLF line endings, while the
        // migrations stay LF (`.gitattributes`): what is compared is the
        // text, not how its lines end.
        let text = text.replace("\r\n", "\n");
        for dialect in ["postgres", "mysql"] {
            let mut files: Vec<_> = std::fs::read_dir(root.join("migrations").join(dialect))
                .expect("migrations")
                .map(|entry| entry.expect("entry").path())
                .collect();
            files.sort();
            assert!(!files.is_empty(), "no {dialect} migrations");
            for file in files {
                let sql = std::fs::read_to_string(&file).expect("migration");
                assert!(
                    text.contains(sql.trim_end()),
                    "{guide} does not show {} as it is",
                    file.display()
                );
            }
        }
    }
}

/// sqlx keeps each applied step's checksum and refuses a database whose
/// steps no longer match the files a bridge was built with — every bridge
/// on it, at once. So a released step never changes: what it should have
/// done differently is a new step, and a new one adds its line here.
#[test]
fn released_migrations_never_change() {
    const RELEASED: [(&str, &str); 6] = [
        (
            "mysql/0001_stores.sql",
            "07b3ac13673d11bc3e7ba1187ee18cc7028129a02405a0ed12392353eab12171",
        ),
        (
            "mysql/0002_audit.sql",
            "c0c856ccf6cc3337f9de2463e2cb69df462f12d63460ea955721a18ccfea6954",
        ),
        (
            "mysql/0003_signups.sql",
            "064824710ee350e183128e88299f62195ba17dadd6cae713a854c6b372ae808f",
        ),
        (
            "postgres/0001_stores.sql",
            "d7992d5194a074ea75c758f09d4aa3fdecd4507f974321ecf63b0b9466cdc3cd",
        ),
        (
            "postgres/0002_audit.sql",
            "f11807dac24545438376f4e2802186872a7ead3e2691220589778ea6ae08c639",
        ),
        (
            "postgres/0003_signups.sql",
            "e1786ced856858e0d1a274943c69d8eac39e6285628543078c899070a78fdc53",
        ),
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut found = Vec::new();
    for dialect in ["mysql", "postgres"] {
        for entry in std::fs::read_dir(root.join(dialect)).expect("migrations") {
            let name = entry.expect("entry").file_name().to_string_lossy().into_owned();
            found.push(format!("{dialect}/{name}"));
        }
    }
    found.sort();
    let pinned: Vec<String> = RELEASED.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(found, pinned, "a new step adds its line to RELEASED");
    for (name, sha256) in RELEASED {
        let bytes = std::fs::read(root.join(name)).expect("migration");
        assert_eq!(
            hex(&Sha256::digest(&bytes)),
            sha256,
            "{name} was released and never changes: write a new step instead"
        );
    }
}

/// A schema step cut short — MySQL cannot take back the half of one that
/// ran, and sqlx marks it unfinished — is run again at the next start,
/// rather than refusing every bridge until someone edits sqlx's table.
#[tokio::test]
#[ignore]
async fn a_schema_step_cut_short_is_run_again_at_the_next_start() {
    let first = store().await;
    let latest: i64 = sqlx::query("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&first.pool)
        .await
        .expect("steps")
        .try_get(0)
        .expect("the latest step");
    // The step's number is the database's own, not anything typed in.
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE _sqlx_migrations SET success = FALSE WHERE version = {latest}"
    )))
    .execute(&first.pool)
    .await
    .expect("cut short");

    let again = store().await;
    let finished: i64 = sqlx::query(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = {latest} AND success = TRUE"
    )))
    .fetch_one(&again.pool)
    .await
    .expect("steps")
    .try_get(0)
    .expect("count");
    assert_eq!(finished, 1, "the step ran again and is marked finished");
}

#[tokio::test]
#[ignore]
async fn the_database_keeps_the_contract() {
    conformance::keeps_the_contract(&store().await.stores()).await;
}

/// A login one bridge saved is the other's, and its last use never moves
/// back; a sign-out moves the counter every bridge polls.
#[tokio::test]
#[ignore]
async fn a_login_is_every_bridge_s() {
    let store = store().await;
    let stores = store.clone().stores();
    let versions = stores.versions.clone().expect("versions");
    let before = versions.read().await.expect("versions");
    let key = unique("login");
    let login = |last_used| Login {
        account: "alice".to_string(),
        version: 1,
        last_used,
        remember: false,
    };
    stores
        .logins
        .save(vec![(key.clone(), LoginChange::Opened(login(2_000)))])
        .await
        .expect("save");
    stores
        .logins
        .save(vec![(key.clone(), LoginChange::Used(login(1_000)))])
        .await
        .expect("save");
    assert_eq!(stores.logins.get(key.clone()).await.expect("get"), Some(login(2_000)));
    stores
        .logins
        .save(vec![(key.clone(), LoginChange::Closed)])
        .await
        .expect("save");
    assert_eq!(stores.logins.get(key).await.expect("get"), None);
    let after = versions.read().await.expect("versions");
    assert!(after.logins_revoked > before.logins_revoked);
}

/// Two bridges each demote one of two administrators: the second decides
/// again on what the first did and is refused.
#[tokio::test]
#[ignore]
async fn the_last_administrator_holds_across_bridges() {
    let one_store = store().await;
    let two_store = store().await;
    let (alice, bob) = (unique("alice"), unique("bob"));
    let admin = |name: &str| AccountRecord {
        name: name.to_string(),
        role: Role::Admin,
        version: 1,
        ..AccountRecord::default()
    };
    // Every other administrator a previous run left is demoted first, so
    // these two are the last.
    let seeded = one_store.clone().stores();
    let at = seeded.accounts.load().await.expect("load");
    let mut ops: Vec<AccountOp> = at
        .accounts
        .iter()
        .filter(|a| a.role == Role::Admin)
        .map(|a| {
            AccountOp::PutAccount(AccountRecord {
                role: Role::Full,
                ..a.clone()
            })
        })
        .collect();
    ops.extend([AccountOp::PutAccount(admin(&alice)), AccountOp::PutAccount(admin(&bob))]);
    seeded
        .accounts
        .apply(at.version, ops)
        .await
        .expect("apply")
        .expect("applied");

    let one = Accounts::load(one_store.stores().accounts).await.expect("load");
    let two = Accounts::load(two_store.stores().accounts).await.expect("load");
    one.update(
        &alice,
        AccountChange {
            role: Some(Role::Full),
            ..AccountChange::default()
        },
    )
    .await
    .expect("demoted");
    assert!(matches!(
        two.update(
            &bob,
            AccountChange {
                role: Some(Role::Full),
                ..AccountChange::default()
            }
        )
        .await,
        Err(crate::accounts::AccountError::LastAdmin)
    ));
}

/// Ten failed tries are ten across bridges.
#[tokio::test]
#[ignore]
async fn failed_sign_ins_are_counted_across_bridges() {
    let shared = store().await.stores().throttle.expect("throttle");
    let one = Throttle::shared(shared.clone(), unique("one"));
    let two = Throttle::shared(shared, unique("two"));
    let key = attempt_key(None, &unique("guess"));
    for _ in 0..crate::auth::THROTTLE_LIMIT / 2 {
        one.failed(&key);
        two.failed(&key);
    }
    assert!(merged_until(&one, &key, true).await);
    two.succeeded(&key);
    one.succeeded(&key);
    assert!(merged_until(&one, &key, false).await);
}

/// Merge until `throttle` says `blocked` about `key`, for a few polls'
/// worth at most. A failure is written by a task of its own, and on a CI
/// runner the writes have been seen to land later than a quarter of a poll
/// (`postgres:18`, 2026-10-26).
async fn merged_until(throttle: &Throttle, key: &str, blocked: bool) -> bool {
    for _ in 0..100 {
        throttle.merge().await;
        if throttle.blocked(key) == blocked {
            return true;
        }
        tokio::time::sleep(STORE_POLL / 20).await;
    }
    false
}

/// The server list: a change lands in the cache the pool dials from, a
/// swap of an entry someone changed is refused, and the secrets reach the
/// database sealed.
#[tokio::test]
#[ignore]
async fn the_server_list_is_kept_and_swapped() {
    let store = store().await;
    let stores = store.clone().stores();
    let before = stores
        .versions
        .clone()
        .expect("versions")
        .read()
        .await
        .expect("versions");
    let id = unique("server");
    let first = RedisServer {
        id: id.clone(),
        name: "first".to_string(),
        host: "127.0.0.1".to_string(),
        port: 6379,
        password: Some("hunter2".to_string()),
        ..Default::default()
    };
    stores.servers.put(first.clone()).await.expect("put");
    assert_eq!(get_server(&id).expect("cached"), first);
    let raw: String = sqlx::query(store.sql(&statements::SERVER_FOR_UPDATE))
        .bind(id.clone())
        .fetch_one(&store.pool)
        .await
        .expect("row")
        .try_get(0)
        .expect("doc");
    assert!(!raw.contains("hunter2"), "the secret is sealed");

    let edited = RedisServer {
        name: "edited".to_string(),
        ..first.clone()
    };
    stores.servers.put(edited.clone()).await.expect("edit");
    assert!(
        !stores.servers.swap(first.clone(), None).await.expect("swap"),
        "not what it saw"
    );
    assert!(stores.servers.swap(edited, None).await.expect("swap"));
    assert!(get_server(&id).is_err(), "gone from the cache");
    let after = stores.versions.expect("versions").read().await.expect("versions");
    assert!(after.servers >= before.servers + 3);
}

/// A login saved by one bridge's `Logins` is taken in by another's.
#[tokio::test]
#[ignore]
async fn a_sign_in_on_one_bridge_is_taken_in_by_another() {
    let accounts = Accounts::from_inline("carol@secret").expect("accounts");
    let one = Logins::load(store().await.stores().logins, &accounts).await;
    let two = Logins::load(store().await.stores().logins, &accounts).await;
    let id = one.open_saved("carol", &accounts, false).await.expect("login");
    two.fetch(&id, &accounts).await;
    assert_eq!(two.account(&id, &accounts).as_deref(), Some("carol"));
}

/// A local directory with one of everything goes into the database as it
/// was — a deleted key's id and time, a sample's minute, a login's last use
/// — and a database that is not empty is refused.
#[tokio::test]
#[ignore]
async fn a_local_directory_is_copied_into_the_database() {
    use crate::accounts::{InviteKind, InviteRecord, LocalAccounts, LocalSetup, SignupRecord};
    use crate::auth::LocalLogins;
    use crate::import::{copy, import_local};
    use crate::metrics::LocalMetrics;
    use crate::page_data::{KeyNote, LocalKeyNotes, LocalPageRows, LocalScripts, ScriptRecord};
    use crate::store::{
        AccountStore, KeyNoteStore, LoginStore, PageRowStore, ScriptStore, SetupStore, local_paths as at,
    };
    use crate::trash::{LocalTrash, NewEntry};
    use serde_json::value::RawValue;

    let store = store().await;
    let dir = std::env::temp_dir().join(unique("zedis-import"));
    std::fs::create_dir_all(&dir).expect("dir");
    let (account, server, script, login_key) = (unique("erin"), unique("srv"), unique("script"), unique("login"));

    let record = AccountRecord {
        name: account.clone(),
        role: Role::Full,
        version: 2,
        ..AccountRecord::default()
    };
    let invite = InviteRecord {
        id: unique("invite"),
        code_sha256: "11".repeat(32),
        kind: InviteKind::Reset {
            account: account.clone(),
        },
        expires_at: 4_000_000_000,
        created_by: "root".to_string(),
        created_at: 1,
    };
    let request = SignupRecord {
        id: unique("signup"),
        name: unique("frank"),
        password: "hash".to_string(),
        note: "waiting".to_string(),
        peer: None,
        requested_at: 1,
        expires_at: 4_000_000_000,
    };
    LocalAccounts::new(dir.join(at::ACCOUNTS))
        .apply(
            0,
            vec![
                AccountOp::PutAccount(record.clone()),
                AccountOp::PutInvite(invite.clone()),
                AccountOp::PutSignup(request.clone()),
            ],
        )
        .await
        .expect("accounts")
        .expect("applied");
    let login = Login {
        account: account.clone(),
        version: 2,
        last_used: 1_800_000_000,
        remember: true,
    };
    LocalLogins::new(dir.join(at::LOGINS))
        .save(vec![(login_key.clone(), LoginChange::Opened(login.clone()))])
        .await
        .expect("logins");
    std::fs::write(
        dir.join(at::SERVERS),
        format!("[[servers]]\nid = \"{server}\"\nname = \"imported\"\nhost = \"127.0.0.1\"\nport = 6379\n"),
    )
    .expect("servers");
    let saved = ScriptRecord {
        id: script.clone(),
        owner: Some(account.clone()),
        script: RawValue::from_string(r#"{"name":"moved"}"#.to_string()).expect("json"),
        updated_by: account.clone(),
        updated_at: 3,
    };
    ScriptStore::put(&LocalScripts::new(dir.join(at::SCRIPTS)), saved)
        .await
        .expect("script");
    PageRowStore::put(
        &LocalPageRows::new(dir.join(at::PAGE_ROWS)),
        account.clone(),
        "favorites".to_string(),
        server.clone(),
        Some("[\"k\"]".to_string()),
    )
    .await
    .expect("row");
    let note = KeyNote {
        tag: None,
        note: "moved".to_string(),
        by: account.clone(),
        at: 4,
    };
    KeyNoteStore::put(
        &LocalKeyNotes::new(dir.join(at::KEY_NOTES)),
        server.clone(),
        "user:1".to_string(),
        Some(note.clone()),
    )
    .await
    .expect("note");
    let bin = LocalTrash::new(dir.join(at::TRASH));
    let deleted = bin
        .put(
            &server,
            NewEntry {
                key: "user:2".to_string(),
                db: 0,
                pttl_ms: -1,
                deleted_by: account.clone(),
                payload: b"dump".to_vec(),
                size: 4,
                sealed: false,
            },
        )
        .expect("bin");
    let sampled_at = crate::metrics::now_ms();
    assert!(
        LocalMetrics::new(dir.join(at::METRICS))
            .put(&server, sampled_at, r#"{"ops":7}"#)
            .expect("sample")
    );
    let token = unique("token");
    LocalSetup::new(dir.join(at::SETUP_TOKEN))
        .put(token.clone())
        .await
        .expect("token");

    let done = copy(&dir, &store).await.expect("copied");
    assert_eq!(
        (
            done.accounts,
            done.invites,
            done.signups,
            done.logins,
            done.servers,
            done.scripts
        ),
        (1, 1, 1, 1, 1, 1)
    );
    assert_eq!((done.page_rows, done.key_notes, done.trash, done.metrics), (1, 1, 1, 1));
    assert!(done.setup_token);

    let target = store.clone().stores();
    let at_now = target.accounts.load().await.expect("accounts");
    assert!(at_now.accounts.contains(&record) && at_now.invites.contains(&invite));
    assert!(at_now.signups.contains(&request), "a waiting request moves too");
    assert_eq!(target.logins.get(login_key).await.expect("login"), Some(login));
    assert!(
        target
            .servers
            .list()
            .await
            .expect("servers")
            .iter()
            .any(|s| s.id == server)
    );
    assert!(
        target
            .scripts
            .list()
            .await
            .expect("scripts")
            .iter()
            .any(|s| s.id == script)
    );
    assert_eq!(
        target.page_rows.rows(account).await.expect("rows")["favorites"][&server],
        "[\"k\"]"
    );
    assert_eq!(
        target.key_notes.list(server.clone()).await.expect("notes")["user:1"],
        note
    );
    assert_eq!(target.trash.list(server.clone()).await.expect("bin"), vec![deleted]);
    let samples = target.metrics.since(server, sampled_at - 1, 10).await.expect("samples");
    assert_eq!(
        samples.iter().map(|s| s.get()).collect::<Vec<_>>(),
        vec![r#"{"ops":7}"#]
    );
    assert!(target.setup.accepts(token).await.expect("token"));
    target.setup.take().await.expect("take");

    assert!(
        import_local(&dir, None, &store).await.is_err(),
        "a database that holds accounts is refused"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The audit in the database: every line kept, read back newest first,
/// narrowed and in pages, pruned by age — and an audit file copied in.
#[tokio::test]
#[ignore]
async fn the_audit_is_kept_read_back_and_pruned() {
    use crate::store::{AuditEntry, AuditQuery};
    let store = store().await;
    let log = store.clone().stores().audit.expect("audit");
    let account = unique("auditor");
    for (n, event) in ["login", "logout", "server_deleted"].into_iter().enumerate() {
        log.append(AuditEntry {
            at_ms: 1_000 + n as i64,
            account: account.clone(),
            event: event.to_string(),
            server: (event == "server_deleted").then(|| "srv-a".to_string()),
            line: format!(r#"{{"event":"{event}","account":"{account}","n":{n}}}"#).into_bytes(),
        });
    }
    let mine = || AuditQuery {
        account: Some(account.clone()),
        ..AuditQuery::default()
    };
    // The writer is a task of its own.
    let mut kept = 0;
    for _ in 0..100 {
        kept = log.query(mine()).await.expect("query").lines.len();
        if kept == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(kept, 3);
    let n = |page: &crate::store::AuditPage| -> Vec<i64> {
        page.lines
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line.get()).expect("json")["n"]
                    .as_i64()
                    .expect("n")
            })
            .collect()
    };
    let first = log
        .query(AuditQuery {
            limit: Some(2),
            ..mine()
        })
        .await
        .expect("query");
    assert_eq!(n(&first), [2, 1], "newest first");
    let rest = log
        .query(AuditQuery {
            limit: Some(2),
            before: first.next.clone(),
            ..mine()
        })
        .await
        .expect("query");
    assert_eq!(n(&rest), [0]);
    assert!(rest.next.is_none());
    let about = log
        .query(AuditQuery {
            server: Some("srv-a".to_string()),
            ..mine()
        })
        .await
        .expect("query");
    assert_eq!(n(&about), [2]);
    let since = log
        .query(AuditQuery {
            since_ms: Some(1_001),
            until_ms: Some(1_001),
            ..mine()
        })
        .await
        .expect("query");
    assert_eq!(n(&since), [1]);
    assert!(log.prune(1_001).await.expect("prune") >= 1);
    assert_eq!(n(&log.query(mine()).await.expect("query")), [2, 1]);

    let file = std::env::temp_dir().join(unique("audit")).with_extension("log");
    let filed = unique("filed");
    std::fs::write(
        &file,
        format!(
            "{{\"ts\":\"2026-10-18T07:14:58.219Z\",\"account\":\"{filed}\",\"event\":\"logout\"}}\n\
             {{\"ts\":\"2026-10-18T07:15:00.000Z\",\"account\":\"{filed}\",\"event\":\"lo"
        ),
    )
    .expect("file");
    assert_eq!(
        crate::import::copy_audit(&file, &store).await.expect("copy"),
        1,
        "a cut line is left out"
    );
    let copied = log
        .query(AuditQuery {
            account: Some(filed),
            ..AuditQuery::default()
        })
        .await
        .expect("query");
    assert_eq!(copied.lines.len(), 1);
    let _ = std::fs::remove_file(&file);
}

/// A directory with a file that cannot be read writes nothing: a copy
/// refused halfway would leave a database the next run refuses as not
/// empty.
#[tokio::test]
#[ignore]
async fn a_directory_that_cannot_be_read_is_not_half_copied() {
    use crate::accounts::LocalAccounts;
    use crate::store::{AccountStore, local_paths as at};
    let store = store().await;
    let dir = std::env::temp_dir().join(unique("zedis-import-bad"));
    std::fs::create_dir_all(&dir).expect("dir");
    let account = unique("frank");
    LocalAccounts::new(dir.join(at::ACCOUNTS))
        .apply(
            0,
            vec![AccountOp::PutAccount(AccountRecord {
                name: account.clone(),
                role: Role::Full,
                version: 1,
                ..AccountRecord::default()
            })],
        )
        .await
        .expect("accounts")
        .expect("applied");
    std::fs::write(dir.join(at::SERVERS), "not toml [").expect("servers");
    assert!(crate::import::copy(&dir, &store).await.is_err());
    let at_now = store.clone().stores().accounts.load().await.expect("accounts");
    assert!(
        !at_now.accounts.iter().any(|a| a.name == account),
        "nothing was written"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
