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

//! `zedis-bridge import-local` (ADR 18): what a local directory keeps,
//! copied once into an empty database — the accounts, the invitations, the
//! requests waiting for an administrator, the logins, the server list, the
//! saved scripts, each account's rows, the key notes, the recycle bin and
//! the metrics history, and a waiting setup token. Each thing keeps what it
//! was: a deleted key its id and its time, a sample the minute it was taken,
//! a login its last use, so nobody signs in again and nothing in the bin
//! ages out early or late.
//!
//! It runs with the directory's master key, which the database's check then
//! names: every bridge on the database uses that `master.key` afterwards.
//! There is no way back but the database's own backups.

use crate::accounts::{AccountOp, LocalAccounts, LocalSetup};
use crate::audit::entry_of;
use crate::auth::{LocalLogins, LoginChange};
use crate::metrics::LocalMetrics;
use crate::page_data::{LocalKeyNotes, LocalPageRows, LocalScripts, RowError};
use crate::redis_store::RedisStore;
use crate::sql::SqlStore;
use crate::sql::audit::write_batch;
use crate::store::{
    AccountStore, AuditEntry, KeyNoteStore, LoginStore, PageRowStore, ScriptStore, SetupStore, Stores,
    local_paths as at,
};
use crate::trash::{LocalTrash, Meta};
use serde_json::value::RawValue;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::Path;
use zedis_connection::read_servers_file;

/// Any shared store (`SqlStore` or `RedisStore`) that `import-local` can copy
/// a local directory into.
pub trait ImportTarget: Clone {
    fn stores(&self) -> Stores;
    fn is_empty(&self) -> impl Future<Output = io::Result<bool>>;
    fn import_trash(&self, server_id: &str, meta: &Meta, payload: Vec<u8>) -> impl Future<Output = io::Result<()>>;
    fn import_metric(&self, server_id: &str, ts_ms: i64, sample: &RawValue) -> impl Future<Output = io::Result<()>>;
    fn write_audit_batch(&self, batch: Vec<AuditEntry>) -> impl Future<Output = io::Result<()>>;
}

impl ImportTarget for SqlStore {
    fn stores(&self) -> Stores {
        self.clone().stores()
    }

    async fn is_empty(&self) -> io::Result<bool> {
        SqlStore::is_empty(self).await
    }

    async fn import_trash(&self, server_id: &str, meta: &Meta, payload: Vec<u8>) -> io::Result<()> {
        SqlStore::import_trash(self, server_id, meta, payload).await
    }

    async fn import_metric(&self, server_id: &str, ts_ms: i64, sample: &RawValue) -> io::Result<()> {
        SqlStore::import_metric(self, server_id, ts_ms, sample).await
    }

    async fn write_audit_batch(&self, batch: Vec<AuditEntry>) -> io::Result<()> {
        write_batch(self, batch).await
    }
}

impl ImportTarget for RedisStore {
    fn stores(&self) -> Stores {
        self.clone().stores()
    }

    async fn is_empty(&self) -> io::Result<bool> {
        RedisStore::is_empty(self).await
    }

    async fn import_trash(&self, server_id: &str, meta: &Meta, payload: Vec<u8>) -> io::Result<()> {
        RedisStore::import_trash(self, server_id, meta, payload).await
    }

    async fn import_metric(&self, server_id: &str, ts_ms: i64, sample: &RawValue) -> io::Result<()> {
        RedisStore::import_metric(self, server_id, ts_ms, sample).await
    }

    async fn write_audit_batch(&self, batch: Vec<AuditEntry>) -> io::Result<()> {
        RedisStore::write_audit_batch(self, batch).await
    }
}

/// How much of each kind went in.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub accounts: usize,
    pub invites: usize,
    pub signups: usize,
    pub logins: usize,
    pub servers: usize,
    pub scripts: usize,
    pub page_rows: usize,
    pub key_notes: usize,
    pub trash: usize,
    pub metrics: usize,
    pub audit: usize,
    pub setup_token: bool,
}

impl fmt::Display for Imported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  accounts         {}", self.accounts)?;
        writeln!(f, "  invitations      {}", self.invites)?;
        writeln!(f, "  requests         {}", self.signups)?;
        writeln!(f, "  logins           {}", self.logins)?;
        writeln!(f, "  server entries   {}", self.servers)?;
        writeln!(f, "  scripts          {}", self.scripts)?;
        writeln!(f, "  account rows     {}", self.page_rows)?;
        writeln!(f, "  key notes        {}", self.key_notes)?;
        writeln!(f, "  recycle bin      {}", self.trash)?;
        writeln!(f, "  metrics samples  {}", self.metrics)?;
        writeln!(f, "  audit lines      {}", self.audit)?;
        write!(
            f,
            "  setup token      {}",
            if self.setup_token { "waiting" } else { "none" }
        )
    }
}

/// Copy `from` into `to`, which must hold no account, server entry or
/// script: an import is a move, not a merge. `audit` is the file
/// `--audit-log` named, if its lines go too.
pub async fn import_local<T: ImportTarget>(from: &Path, audit: Option<&Path>, to: &T) -> io::Result<Imported> {
    if !to.is_empty().await? {
        return Err(io::Error::other(
            "the database already holds accounts, server entries or scripts: \
             import-local copies a directory into an empty one",
        ));
    }
    // Opened now, so a wrong path is said before anything is copied.
    if let Some(audit) = audit {
        std::fs::File::open(audit).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", audit.display())))?;
    }
    let mut done = copy(from, to).await?;
    if let Some(audit) = audit {
        done.audit = copy_audit(audit, to).await?;
    }
    Ok(done)
}

/// The lines of an audit file, in the order they were written, a batch at a
/// time. A line that is not one — cut short by a crash — is left out.
pub(crate) async fn copy_audit<T: ImportTarget>(path: &Path, to: &T) -> io::Result<usize> {
    use std::io::BufRead;
    const BATCH: usize = 500;
    let file = std::fs::File::open(path)?;
    let mut batch = Vec::with_capacity(BATCH);
    let mut count = 0;
    for line in std::io::BufReader::new(file).split(b'\n') {
        if let Some(entry) = entry_of(&line?) {
            batch.push(entry);
        }
        if batch.len() == BATCH {
            count += batch.len();
            to.write_audit_batch(std::mem::take(&mut batch)).await?;
        }
    }
    count += batch.len();
    if !batch.is_empty() {
        to.write_audit_batch(batch).await?;
    }
    Ok(count)
}

/// The copy itself, without the check — the tests' door, on a database
/// other tests share.
///
/// Everything that can be read wrong — the accounts, the logins, the server
/// file, the scripts, the rows and the notes — is read before anything is
/// written: a copy refused halfway would leave a database that the next run
/// refuses as not empty. The bin and the metrics, which can be large, are
/// read as they are written; a part of them that cannot be read is skipped,
/// as the local stores themselves skip it.
pub(crate) async fn copy<T: ImportTarget>(from: &Path, to: &T) -> io::Result<Imported> {
    let accounts = LocalAccounts::new(from.join(at::ACCOUNTS)).load().await?;
    let logins = LocalLogins::new(from.join(at::LOGINS)).load().await?;
    // The file itself, not the list this process may have cached.
    let servers =
        read_servers_file(&from.join(at::SERVERS)).map_err(|e| io::Error::other(format!("{}: {e}", at::SERVERS)))?;
    let scripts = ScriptStore::list(&LocalScripts::new(from.join(at::SCRIPTS))).await?;
    let page_rows = LocalPageRows::new(from.join(at::PAGE_ROWS));
    let mut rows = Vec::new();
    for account in &accounts.accounts {
        for (table, keyed) in page_rows.rows(account.name.clone()).await? {
            for (key, value) in keyed {
                rows.push((account.name.clone(), table.clone(), key, value));
            }
        }
    }
    let key_notes = LocalKeyNotes::new(from.join(at::KEY_NOTES));
    let mut notes = Vec::new();
    for server in key_notes.servers() {
        for (key, note) in KeyNoteStore::list(&key_notes, server.clone()).await? {
            notes.push((server.clone(), key, note));
        }
    }
    let token = LocalSetup::new(from.join(at::SETUP_TOKEN)).shown().await?;

    let target = to.stores();
    let mut done = Imported {
        accounts: accounts.accounts.len(),
        invites: accounts.invites.len(),
        signups: accounts.signups.len(),
        logins: logins.len(),
        servers: servers.len(),
        scripts: scripts.len(),
        page_rows: rows.len(),
        key_notes: notes.len(),
        setup_token: token.is_some(),
        ..Imported::default()
    };

    // The accounts, invitations and requests, as one change.
    let ops: Vec<AccountOp> = accounts
        .accounts
        .into_iter()
        .map(AccountOp::PutAccount)
        .chain(accounts.invites.into_iter().map(AccountOp::PutInvite))
        .chain(accounts.signups.into_iter().map(AccountOp::PutSignup))
        .collect();
    if !ops.is_empty() {
        let version = target.accounts.version().await?;
        if target.accounts.apply(version, ops).await?.is_none() {
            return Err(io::Error::other("the accounts changed while they were being imported"));
        }
    }
    target
        .logins
        .save(
            logins
                .into_iter()
                .map(|(key, login)| (key, LoginChange::Opened(login)))
                .collect(),
        )
        .await?;
    for server in servers {
        target.servers.put(server).await?;
    }
    for script in scripts {
        target.scripts.put(script).await?;
    }
    for (account, table, key, value) in rows {
        target
            .page_rows
            .put(account.clone(), table, key, Some(value))
            .await
            .map_err(|e| match e {
                RowError::TooLarge => io::Error::other(format!("{account}'s rows are past the limit")),
                RowError::Store(e) => e,
            })?;
    }
    for (server, key, note) in notes {
        target.key_notes.put(server, key, Some(note)).await?;
    }

    let trash = LocalTrash::new(from.join(at::TRASH));
    for server in trash.servers() {
        for listed in trash.list(&server) {
            if let Some((meta, payload)) = trash.get(&server, &listed.id) {
                to.import_trash(&server, &meta, payload).await?;
                done.trash += 1;
            }
        }
    }
    let metrics = LocalMetrics::new(from.join(at::METRICS));
    for server in metrics.servers() {
        for (ts_ms, sample) in metrics.history(&server) {
            to.import_metric(&server, ts_ms, &sample).await?;
            done.metrics += 1;
        }
    }
    if let Some(token) = token {
        target.setup.put(token).await?;
    }
    Ok(done)
}
