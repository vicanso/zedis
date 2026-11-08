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

//! The bridge's stores in PostgreSQL or MySQL (ADR 18): every trait of
//! `store.rs` on one database, chosen at start-up by `--store <url>`, so
//! several bridge processes can share what they keep.
//!
//! One implementation on sqlx's `AnyPool`, with each statement in both
//! spellings (`statements.rs`) and the dialect read from the connection.
//! Records are documents — the record's JSON beside the columns a statement
//! needs — so a new field is `#[serde(default)]`, not a migration. The
//! migrations are the schema, embedded, one directory per dialect, run at
//! start under the database's own lock.
//!
//! What processes hold in memory — the server list, the accounts, the
//! logins — is kept in step by counters (`store_versions`) that every
//! change moves and every process polls (`store::keep_in_step`).

pub(crate) mod audit;
mod data;
mod records;
mod statements;
#[cfg(test)]
mod tests;

use crate::store::{StoreFuture, StoreVersions, Stores, Versions};
use sqlx::any::{AnyPoolOptions, install_default_drivers};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{AnyPool, Row};
use statements::Stmt;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use zedis_connection::string::{decrypt, encrypt};

/// The URL of the database the bridge keeps its stores in, when not the
/// local directory. Preferred to `--store`: a flag is visible to anyone who
/// can list processes, and the URL carries a password.
pub const STORE_ENV: &str = "ZEDIS_BRIDGE_STORE";

/// Connections a process keeps to the database.
const POOL_SIZE: u32 = 10;
/// How long a request waits for one.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the master-key check seals: a process whose key cannot open it
/// would write secrets no other process can read, and refuses to start.
const MASTER_KEY_CHECK: &str = "master_key_check";
const MASTER_KEY_CHECK_TEXT: &str = "zedis-bridge master key check";

/// Which database the store is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    MySql,
}

impl Dialect {
    /// The dialect of a connection, by the name sqlx gives its backend. A
    /// MariaDB answers as MySQL; it is not promised (ADR 18).
    fn of(backend: &str) -> io::Result<Self> {
        match backend {
            "PostgreSQL" => Ok(Self::Postgres),
            "MySQL" => Ok(Self::MySql),
            other => Err(io::Error::other(format!(
                "the bridge keeps its stores in PostgreSQL or MySQL, not {other}"
            ))),
        }
    }

    /// `stmt` as this database spells it.
    fn sql(self, stmt: &Stmt) -> &'static str {
        match self {
            Self::Postgres => stmt.pg,
            Self::MySql => stmt.my,
        }
    }
}

/// Bring the schema up to this bridge's. A step that was cut short — MySQL
/// cannot take back the half of one that ran, so sqlx marks it unfinished
/// and refuses to go on (`MigrateError::Dirty`), every bridge on the
/// database with it — is run again: every statement of a step is written to
/// run twice safely (`IF NOT EXISTS`, `INSERT IGNORE`). Once: a step that
/// fails again says why.
async fn migrate(pool: &AnyPool, dialect: Dialect, migrator: &Migrator) -> io::Result<()> {
    let failed = |e: MigrateError| io::Error::other(format!("the store's schema could not be brought up to date: {e}"));
    match migrator.run(pool).await {
        Err(MigrateError::Dirty(version)) => {
            tracing::warn!(version, "a schema step was cut short; running it again");
            // Not under the migrator's lock: a bridge starting beside this
            // one may forget the mark of a step it is running, which then
            // runs once more — as every step is written to bear.
            sqlx::query(dialect.sql(&statements::FORGET_UNFINISHED_STEP))
                .bind(version)
                .execute(pool)
                .await
                .map_err(db)?;
            migrator.run(pool).await.map_err(failed)
        }
        done => done.map_err(failed),
    }
}

/// The stores on one database.
#[derive(Clone)]
pub struct SqlStore {
    pool: AnyPool,
    dialect: Dialect,
    /// One change to the server list at a time in this process, from the
    /// database to the cache, so two changes cannot land in the cache out
    /// of the order they reached the database.
    server_edit: Arc<tokio::sync::Mutex<()>>,
}

impl SqlStore {
    /// Connect to `url`, bring its schema up to this bridge's, and check
    /// the master key against it.
    pub async fn connect(url: &str) -> io::Result<Self> {
        install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(POOL_SIZE)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect(url)
            .await
            .map_err(|e| io::Error::other(format!("{}: {e}", redacted(url))))?;
        let dialect = {
            let connection = pool.acquire().await.map_err(db)?;
            Dialect::of(connection.backend_name())?
        };
        // A database holding a migration this bridge does not know refuses
        // it: an older bridge does not start on a newer schema.
        let migrator = match dialect {
            Dialect::Postgres => sqlx::migrate!("migrations/postgres"),
            Dialect::MySql => sqlx::migrate!("migrations/mysql"),
        };
        migrate(&pool, dialect, &migrator).await?;
        let store = Self {
            pool,
            dialect,
            server_edit: Arc::default(),
        };
        store.check_master_key().await?;
        Ok(store)
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// `stmt` as this database spells it.
    fn sql(&self, stmt: &Stmt) -> &'static str {
        self.dialect.sql(stmt)
    }

    /// Every store on this database.
    pub fn stores(self) -> Stores {
        let store = Arc::new(self);
        Stores {
            accounts: store.clone(),
            logins: store.clone(),
            servers: store.clone(),
            trash: store.clone(),
            metrics: store.clone(),
            page_rows: store.clone(),
            key_notes: store.clone(),
            scripts: store.clone(),
            setup: store.clone(),
            audit: Some(Arc::new(audit::SqlAudit::start(store.as_ref().clone()))),
            throttle: Some(store.clone()),
            presence: Some(store.clone()),
            versions: Some(store),
        }
    }

    /// The first process to open the store seals a fixed text with its
    /// master key; every process after it has to open it with its own.
    async fn check_master_key(&self) -> io::Result<()> {
        let sealed = encrypt(MASTER_KEY_CHECK_TEXT).map_err(|e| io::Error::other(e.to_string()))?;
        sqlx::query(self.sql(&statements::META_PUT_IF_NONE))
            .bind(MASTER_KEY_CHECK)
            .bind(sealed)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        let stored: String = sqlx::query(self.sql(&statements::META_GET))
            .bind(MASTER_KEY_CHECK)
            .fetch_one(&self.pool)
            .await
            .map_err(db)?
            .try_get(0)
            .map_err(db)?;
        match decrypt(&stored) {
            Ok(text) if text == MASTER_KEY_CHECK_TEXT => Ok(()),
            _ => Err(io::Error::other(
                "this bridge's master.key is not the one the store's other bridges use: \
                 give every bridge on one store the same master.key",
            )),
        }
    }

    /// Whether the database holds no account, server entry or script — what
    /// `import-local` asks before it copies a directory in.
    pub async fn is_empty(&self) -> io::Result<bool> {
        let count: i64 = sqlx::query(self.sql(&statements::AUTHORED_COUNT))
            .fetch_one(&self.pool)
            .await
            .map_err(db)?
            .try_get(0)
            .map_err(db)?;
        Ok(count == 0)
    }

    /// Move `name`'s counter, in `tx`: another process reads the set again.
    async fn bump(&self, tx: &mut sqlx::Transaction<'static, sqlx::Any>, name: &str) -> io::Result<()> {
        sqlx::query(self.sql(&statements::VERSION_BUMP))
            .bind(name)
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        Ok(())
    }
}

impl StoreVersions for SqlStore {
    fn read(&self) -> StoreFuture<Versions> {
        let store = self.clone();
        Box::pin(async move {
            let rows = sqlx::query(store.sql(&statements::VERSIONS))
                .fetch_all(&store.pool)
                .await
                .map_err(db)?;
            let mut versions = Versions::default();
            for row in rows {
                let name: String = row.try_get(0).map_err(db)?;
                let version = counter(row.try_get(1).map_err(db)?);
                match name.as_str() {
                    "servers" => versions.servers = version,
                    "accounts" => versions.accounts = version,
                    "logins_revoked" => versions.logins_revoked = version,
                    _ => {}
                }
            }
            Ok(versions)
        })
    }
}

/// A database error as the stores answer one.
fn db(e: sqlx::Error) -> io::Error {
    io::Error::other(e)
}

/// A counter as the database keeps it — a `BIGINT` — as the bridge counts.
fn counter(stored: i64) -> u64 {
    u64::try_from(stored).unwrap_or(0)
}

/// A count or a time as the database keeps it.
fn stored(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `url` with what must not reach a log taken out: the user and password,
/// and every parameter. What is left says which database it was.
pub fn redacted(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "a store URL".to_string();
    };
    let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    format!("{scheme}://{rest}")
}
