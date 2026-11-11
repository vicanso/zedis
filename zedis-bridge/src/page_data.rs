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

//! The page's own data (ADR 17, step 4): what the browser build used to keep
//! in the tab's memory until a reload emptied it.
//!
//! Three kinds, by who may see them:
//!
//! - **An account's own rows** ([`PageRowStore`]): favorites, recent keys,
//!   search history and the preferences — one account's, private, never
//!   audited. Command history is not among them: it holds what was typed,
//!   passwords included, and the audit already records the commands.
//! - **Key tags and notes** ([`KeyNoteStore`]): a server's, seen by whoever
//!   sees the server, kept one key at a time with who wrote it and when —
//!   two people tagging two keys must not undo each other, which a whole
//!   document per server would. Written by an account that may write there.
//! - **Saved Lua scripts** ([`ScriptStore`]): private or shared like a server
//!   entry, changed by an account that may write, every save and delete an
//!   audit line, because a shared script is run by others, on production.
//!
//! The page loads all of it in one request before the application starts
//! (`GET /v1/page-data`) and sends each change as it is made.

use crate::api::{
    ApiError, ApiResult, AppState, authorize, authorize_write, refuse_read_only_on, visible_server, visible_to,
};
use crate::audit::{Event, Origin};
use crate::auth::{hex, now_secs};
use crate::static_files::{etag_of, matches_etag};
use crate::store::{KeyNoteStore, PageRowStore, ScriptStore, StoreFuture, blocking};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, put},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use zedis_connection::get_servers;
use zedis_core::fs::write_file_atomic;

/// The tables an account keeps rows in, as the page names them. `protos` is
/// one proto viewer a row — a schema and the keys it decodes — the
/// account's own, as the desktop user's are.
pub const ROW_TABLES: [&str; 5] = ["favorites", "recent_keys", "search_history", "preferences", "protos"];
/// One row: a list of key names, the preferences file, a proto viewer.
pub const MAX_ROW_BYTES: usize = 256 * 1024;
/// Everything one account keeps, so a page in a loop cannot fill the disk.
pub const MAX_ACCOUNT_BYTES: usize = 4 * 1024 * 1024;
/// What a row is filed under: a server and database, or a file name.
pub const MAX_ROW_KEY_BYTES: usize = 512;
/// A Redis key name, as a note is filed under it.
pub const MAX_KEY_BYTES: usize = 4096;
pub const MAX_NOTE_CHARS: usize = 4096;
pub const MAX_SCRIPT_BYTES: usize = 256 * 1024;

/// One account's rows: table → key → value.
pub type AccountRows = BTreeMap<String, BTreeMap<String, String>>;

/// Why a row was not kept.
#[derive(Debug)]
pub enum RowError {
    /// The row, or the account's rows with it, past what is kept.
    TooLarge,
    Store(io::Error),
}

impl From<io::Error> for RowError {
    fn from(e: io::Error) -> Self {
        Self::Store(e)
    }
}

/// A key's tag and note on one server, and who left them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyNote {
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub note: String,
    pub by: String,
    pub at: u64,
}

/// A saved Lua script, as the page stores it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptRecord {
    pub id: String,
    /// The account it belongs to; `None` is shared.
    #[serde(default)]
    pub owner: Option<String>,
    /// The page's `LuaScript`, kept as it came: the bridge reads its name
    /// for the audit and nothing else.
    pub script: Box<RawValue>,
    pub updated_by: String,
    pub updated_at: u64,
}

impl ScriptRecord {
    /// The script's name, for the audit log.
    pub(crate) fn name(&self) -> String {
        #[derive(Deserialize)]
        struct Named {
            #[serde(default)]
            name: String,
        }
        serde_json::from_str::<Named>(self.script.get())
            .map(|named| named.name)
            .unwrap_or_default()
    }

    fn visible_to(&self, account: &str) -> bool {
        self.owner.as_deref().is_none_or(|owner| owner == account)
    }
}

// ---- the local directory's stores ------------------------------------------

/// Owner-only on unix: what people keep for themselves, and what they wrote
/// about each other's keys.
#[cfg(unix)]
fn private_dir(dir: &FsPath) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}
#[cfg(not(unix))]
fn private_dir(dir: &FsPath) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn restrict(path: &FsPath) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not restrict a file to its owner");
    }
}
#[cfg(not(unix))]
fn restrict(_path: &FsPath) {}

/// `path` as JSON, or the default when there is none yet. One that cannot be
/// read is an error, never empty: the next write would replace it whole.
/// The text a file name's hex spells, if it spells one.
fn unhex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

fn read_json<T: Default + serde::de::DeserializeOwned>(path: &FsPath) -> io::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

fn write_json<T: Serialize>(dir: &FsPath, path: &FsPath, value: &T) -> io::Result<()> {
    private_dir(dir)?;
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_file_atomic(path, &bytes)?;
    restrict(path);
    Ok(())
}

/// A file per account under `bridge-page-data/accounts/`, named by the hex
/// of the account's name — a name may hold `/` or `.`, a file name may not.
#[derive(Clone)]
pub struct LocalPageRows {
    dir: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl LocalPageRows {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(dir),
            lock: Arc::default(),
        }
    }

    fn path(&self, account: &str) -> PathBuf {
        self.dir.join(format!("{}.json", hex(account.as_bytes())))
    }

    fn read(&self, account: &str) -> io::Result<AccountRows> {
        read_json(&self.path(account))
    }

    fn put(&self, account: &str, table: String, key: String, value: Option<String>) -> Result<(), RowError> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows = self.read(account)?;
        match value {
            Some(value) => {
                rows.entry(table).or_default().insert(key, value);
            }
            None => {
                if let Some(table_rows) = rows.get_mut(&table) {
                    table_rows.remove(&key);
                    if table_rows.is_empty() {
                        rows.remove(&table);
                    }
                }
            }
        }
        let total: usize = rows
            .values()
            .flat_map(|table| table.iter())
            .map(|(key, value)| key.len() + value.len())
            .sum();
        if total > MAX_ACCOUNT_BYTES {
            return Err(RowError::TooLarge);
        }
        write_json(&self.dir, &self.path(account), &rows)?;
        Ok(())
    }

    fn forget(&self, account: &str) -> io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        match std::fs::remove_file(self.path(account)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

impl PageRowStore for LocalPageRows {
    fn rows(&self, account: String) -> StoreFuture<AccountRows> {
        let store = self.clone();
        let read = blocking(move || store.read(&account));
        Box::pin(async move { read.await? })
    }

    fn put(
        &self,
        account: String,
        table: String,
        key: String,
        value: Option<String>,
    ) -> BoxFuture<'static, Result<(), RowError>> {
        let store = self.clone();
        let put = blocking(move || store.put(&account, table, key, value));
        Box::pin(async move { put.await? })
    }

    fn forget(&self, account: String) -> StoreFuture<()> {
        let store = self.clone();
        let forget = blocking(move || store.forget(&account));
        Box::pin(async move { forget.await? })
    }
}

/// A file per server under `bridge-page-data/key-notes/`, named by the hex
/// of its id, holding `key → note`; changed one key at a time.
#[derive(Clone)]
pub struct LocalKeyNotes {
    dir: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl LocalKeyNotes {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(dir),
            lock: Arc::default(),
        }
    }

    fn path(&self, server_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", hex(server_id.as_bytes())))
    }

    /// The servers that have notes here, by id: what `import-local` copies.
    pub fn servers(&self) -> Vec<String> {
        let Ok(files) = std::fs::read_dir(self.dir.as_path()) else {
            return Vec::new();
        };
        let mut servers: Vec<String> = files
            .flatten()
            .filter_map(|f| f.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".json").and_then(unhex))
            .collect();
        servers.sort();
        servers
    }

    fn list(&self, server_id: &str) -> io::Result<BTreeMap<String, KeyNote>> {
        read_json(&self.path(server_id))
    }

    fn put(&self, server_id: &str, key: String, note: Option<KeyNote>) -> io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut notes = self.list(server_id)?;
        match note {
            Some(note) => {
                notes.insert(key, note);
            }
            None => {
                notes.remove(&key);
            }
        }
        let path = self.path(server_id);
        if notes.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        write_json(&self.dir, &path, &notes)
    }

    fn remove_server(&self, server_id: &str) -> io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        match std::fs::remove_file(self.path(server_id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

impl KeyNoteStore for LocalKeyNotes {
    fn list(&self, server_id: String) -> StoreFuture<BTreeMap<String, KeyNote>> {
        let store = self.clone();
        let list = blocking(move || store.list(&server_id));
        Box::pin(async move { list.await? })
    }

    fn put(&self, server_id: String, key: String, note: Option<KeyNote>) -> StoreFuture<()> {
        let store = self.clone();
        let put = blocking(move || store.put(&server_id, key, note));
        Box::pin(async move { put.await? })
    }

    fn remove_server(&self, server_id: String) -> StoreFuture<()> {
        let store = self.clone();
        let remove = blocking(move || store.remove_server(&server_id));
        Box::pin(async move { remove.await? })
    }
}

/// `bridge-scripts.json`: every saved script, read and written whole, one
/// change at a time.
#[derive(Clone)]
pub struct LocalScripts {
    path: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl LocalScripts {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            lock: Arc::default(),
        }
    }

    fn list(&self) -> io::Result<Vec<ScriptRecord>> {
        read_json(&self.path)
    }

    fn edit<T>(&self, change: impl FnOnce(&mut Vec<ScriptRecord>) -> T) -> io::Result<T> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut scripts = self.list()?;
        let answer = change(&mut scripts);
        let dir = self.path.parent().map(FsPath::to_path_buf).unwrap_or_default();
        write_json(&dir, &self.path, &scripts)?;
        Ok(answer)
    }
}

impl ScriptStore for LocalScripts {
    fn list(&self) -> StoreFuture<Vec<ScriptRecord>> {
        let store = self.clone();
        let list = blocking(move || store.list());
        Box::pin(async move { list.await? })
    }

    fn put(&self, record: ScriptRecord) -> StoreFuture<()> {
        let store = self.clone();
        let put = blocking(move || {
            store.edit(|scripts| match scripts.iter_mut().find(|s| s.id == record.id) {
                Some(slot) => *slot = record,
                None => scripts.push(record),
            })
        });
        Box::pin(async move { put.await? })
    }

    fn remove(&self, id: String) -> StoreFuture<Option<ScriptRecord>> {
        let store = self.clone();
        let remove = blocking(move || {
            store.edit(|scripts| {
                let at = scripts.iter().position(|s| s.id == id)?;
                Some(scripts.remove(at))
            })
        });
        Box::pin(async move { remove.await? })
    }
}

// ---- routes -----------------------------------------------------------------

pub(crate) fn routes(router: Router<AppState>, at: &impl Fn(&str) -> String) -> Router<AppState> {
    router
        .route(&at("/v1/page-data"), get(page_data))
        .route(&at("/v1/page-data/rows"), put(put_row))
        .route(&at("/v1/servers/{id}/key-notes"), put(put_key_note))
        .route(&at("/v1/scripts/{id}"), put(put_script).delete(delete_script))
}

fn store_error(e: io::Error) -> ApiError {
    tracing::error!(error = %e, "the page's data could not be kept");
    ApiError::Upstream(format!("the bridge could not keep it: {e}"))
}

#[derive(Serialize)]
struct Row {
    table: String,
    key: String,
    value: String,
}

#[derive(Serialize)]
struct ServerNote {
    server: String,
    key: String,
    #[serde(flatten)]
    note: KeyNote,
}

#[derive(Serialize)]
struct PageScript {
    id: String,
    shared: bool,
    /// Who it belongs to, or who last saved a shared one.
    owner: Option<String>,
    updated_by: String,
    updated_at: u64,
    script: Box<RawValue>,
}

#[derive(Serialize)]
struct PageData {
    account: String,
    read_only: bool,
    /// Whether the bridge keeps this account's own rows: not for a locked
    /// one, whose favorites, history and preferences stay in each browser.
    rows_kept: bool,
    rows: Vec<Row>,
    key_notes: Vec<ServerNote>,
    scripts: Vec<PageScript>,
}

/// Everything the page keeps for this account, before it starts: its own
/// rows, the notes on every server it sees, and the scripts it may use.
///
/// Asked again every minute or so by a page that is open, and when it opens
/// a server or the script library, to see what other accounts changed — so
/// it carries an `ETag`, and an unchanged answer is a `304` with no body.
async fn page_data(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let data = gather(&state, &headers, peer).await?;
    let body = serde_json::to_vec(&data).map_err(|e| ApiError::Upstream(e.to_string()))?;
    let etag = etag_of(&body);
    let unchanged = headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|tags| matches_etag(Some(tags), &etag));
    let response = Response::builder()
        .header("etag", &etag)
        .header("cache-control", "no-cache");
    let response = if unchanged {
        response.status(StatusCode::NOT_MODIFIED).body(Body::empty())
    } else {
        response
            .header("content-type", "application/json")
            .body(Body::from(body))
    };
    response.map_err(|e| ApiError::Upstream(e.to_string()))
}

async fn gather(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> ApiResult<PageData> {
    let account = authorize(state, headers, &mut Origin::new(peer, headers)).await?;
    let rows_kept = !state.accounts.is_locked(&account);
    let rows = if rows_kept {
        state.page_rows.rows(account.clone()).await.map_err(store_error)?
    } else {
        Default::default()
    };
    let rows = rows
        .into_iter()
        .flat_map(|(table, rows)| {
            rows.into_iter().map(move |(key, value)| Row {
                table: table.clone(),
                key,
                value,
            })
        })
        .collect();
    let mut key_notes = Vec::new();
    for server in get_servers().unwrap_or_default() {
        if !visible_to(&state.accounts, &server, &account) {
            continue;
        }
        let notes = state.key_notes.list(server.id.clone()).await.map_err(store_error)?;
        key_notes.extend(notes.into_iter().map(|(key, note)| ServerNote {
            server: server.id.clone(),
            key,
            note,
        }));
    }
    let scripts = state
        .scripts
        .list()
        .await
        .map_err(store_error)?
        .into_iter()
        .filter(|script| script.visible_to(&account))
        .map(|script| PageScript {
            id: script.id,
            shared: script.owner.is_none(),
            owner: script.owner,
            updated_by: script.updated_by,
            updated_at: script.updated_at,
            script: script.script,
        })
        .collect();
    Ok(PageData {
        read_only: state.accounts.is_read_only(&account),
        rows_kept,
        account,
        rows,
        key_notes,
        scripts,
    })
}

#[derive(Deserialize)]
struct RowRequest {
    table: String,
    key: String,
    /// The row, or `null` to forget it.
    #[serde(default)]
    value: Option<String>,
}

/// One of the account's own rows. A read-only account keeps its own too:
/// they change nothing anybody else sees. A locked one keeps none: everyone
/// who signs in to it would share them.
async fn put_row(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RowRequest>,
) -> ApiResult<StatusCode> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    if state.accounts.is_locked(&account) {
        return Err(ApiError::Forbidden(
            "this account is locked: the bridge keeps no rows of its own for it".to_string(),
        ));
    }
    if !ROW_TABLES.contains(&req.table.as_str()) {
        return Err(ApiError::BadRequest(format!("no table {:?}", req.table)));
    }
    if req.key.is_empty() || req.key.len() > MAX_ROW_KEY_BYTES {
        return Err(ApiError::BadRequest("a row's key is 1 to 512 bytes".to_string()));
    }
    if req.value.as_ref().is_some_and(|value| value.len() > MAX_ROW_BYTES) {
        return Err(ApiError::BadRequest("the row is too large to keep".to_string()));
    }
    match state.page_rows.put(account, req.table, req.key, req.value).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(RowError::TooLarge) => Err(ApiError::BadRequest("this account keeps too much already".to_string())),
        Err(RowError::Store(e)) => Err(store_error(e)),
    }
}

#[derive(Deserialize)]
struct KeyNoteRequest {
    key: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    note: String,
}

/// A key's tag and note, or neither: what an account that may write on the
/// server leaves for everyone who sees it (ADR 17). Not audited — the note
/// carries who wrote it and when.
async fn put_key_note(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<KeyNoteRequest>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &server, &origin, "key_note")?;
    if req.key.is_empty() || req.key.len() > MAX_KEY_BYTES {
        return Err(ApiError::BadRequest("a key is 1 to 4096 bytes".to_string()));
    }
    if req.note.chars().count() > MAX_NOTE_CHARS {
        return Err(ApiError::BadRequest("a note is at most 4096 characters".to_string()));
    }
    let tag = req.tag.filter(|tag| !tag.trim().is_empty());
    if tag
        .as_deref()
        .is_some_and(|tag| tag.len() > 32 || !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    {
        return Err(ApiError::BadRequest("a tag is a colour's name".to_string()));
    }
    let note = (tag.is_some() || !req.note.trim().is_empty()).then(|| KeyNote {
        tag,
        note: req.note,
        by: account,
        at: now_secs(),
    });
    state
        .key_notes
        .put(server.id, req.key, note)
        .await
        .map_err(store_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ScriptRequest {
    script: Box<RawValue>,
    #[serde(default)]
    shared: bool,
}

/// A saved script, new or changed, private or shared — settled the way a
/// server entry's owner is: by who is signed in, never by the request.
/// Making someone else's shared script private makes it the saver's.
async fn put_script(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ScriptRequest>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "script_save").await?;
    if id.is_empty() || id.len() > 128 {
        return Err(ApiError::BadRequest("a script's id is 1 to 128 bytes".to_string()));
    }
    if req.script.get().len() > MAX_SCRIPT_BYTES {
        return Err(ApiError::BadRequest("the script is too large to keep".to_string()));
    }
    let scripts = state.scripts.list().await.map_err(store_error)?;
    let stored = scripts.into_iter().find(|script| script.id == id);
    if stored.as_ref().is_some_and(|script| !script.visible_to(&account)) {
        return Err(ApiError::NotFound(format!("no script {id}")));
    }
    let record = ScriptRecord {
        id: id.clone(),
        owner: (!req.shared).then(|| account.clone()),
        script: req.script,
        updated_by: account.clone(),
        updated_at: now_secs(),
    };
    let name = record.name();
    state.scripts.put(record).await.map_err(store_error)?;
    state.audit.record(
        &account,
        &origin,
        Event::ScriptSaved {
            id,
            name,
            shared: req.shared,
            created: stored.is_none(),
        },
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_script(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "script_delete").await?;
    let visible = state
        .scripts
        .list()
        .await
        .map_err(store_error)?
        .into_iter()
        .any(|script| script.id == id && script.visible_to(&account));
    if !visible {
        return Err(ApiError::NotFound(format!("no script {id}")));
    }
    if let Some(removed) = state.scripts.remove(id.clone()).await.map_err(store_error)? {
        let name = removed.name();
        state.audit.record(&account, &origin, Event::ScriptDeleted { id, name });
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
