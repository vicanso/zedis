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

//! Every statement the SQL store sends, in both spellings, side by side
//! (ADR 18). sqlx's `Any` passes a statement through as written: PostgreSQL
//! numbers its placeholders and MySQL does not, and their upserts differ —
//! `ON CONFLICT … DO UPDATE` against MySQL 8.0.19's row alias, `AS new ON
//! DUPLICATE KEY UPDATE`. Both spellings bind their values in the same
//! order, so the code that sends one does not ask which it is.
//!
//! Two MySQL rules shape what is here. "Was a row inserted" is asked of an
//! `INSERT IGNORE`, never of `ON DUPLICATE KEY UPDATE`: sqlx connects with
//! `CLIENT_FOUND_ROWS`, under which an update that changes nothing still
//! counts its row. And a sum is cast to an integer: MySQL's `SUM` answers a
//! `DECIMAL`, which `Any` cannot carry.

/// One statement, as each database spells it.
pub struct Stmt {
    pub pg: &'static str,
    pub my: &'static str,
}

/// The same text for both.
macro_rules! both {
    ($sql:expr) => {
        Stmt { pg: $sql, my: $sql }
    };
}

// ---- the store itself ---------------------------------------------------------

pub const VERSIONS: Stmt = both!("SELECT name, version FROM store_versions");
pub const VERSION_BUMP: Stmt = Stmt {
    pg: "UPDATE store_versions SET version = version + 1 WHERE name = $1",
    my: "UPDATE store_versions SET version = version + 1 WHERE name = ?",
};
/// Moves the counter only while it is the one the caller read.
pub const VERSION_BUMP_FROM: Stmt = Stmt {
    pg: "UPDATE store_versions SET version = version + 1 WHERE name = $1 AND version = $2",
    my: "UPDATE store_versions SET version = version + 1 WHERE name = ? AND version = ?",
};
pub const VERSION_OF: Stmt = Stmt {
    pg: "SELECT version FROM store_versions WHERE name = $1",
    my: "SELECT version FROM store_versions WHERE name = ?",
};
pub const META_GET: Stmt = Stmt {
    pg: "SELECT value FROM store_meta WHERE name = $1",
    my: "SELECT value FROM store_meta WHERE name = ?",
};
pub const META_PUT_IF_NONE: Stmt = Stmt {
    pg: "INSERT INTO store_meta (name, value) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
    my: "INSERT IGNORE INTO store_meta (name, value) VALUES (?, ?)",
};
pub const META_PUT: Stmt = Stmt {
    pg: "INSERT INTO store_meta (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value",
    my: "INSERT INTO store_meta (name, value) VALUES (?, ?) AS new \
         ON DUPLICATE KEY UPDATE value = new.value",
};
pub const META_DELETE: Stmt = Stmt {
    pg: "DELETE FROM store_meta WHERE name = $1",
    my: "DELETE FROM store_meta WHERE name = ?",
};
pub const PRESENCE_LIST: Stmt = both!("SELECT name, value FROM store_meta WHERE name LIKE 'presence:%'");

/// Forget sqlx's mark on a schema step that was cut short, so it runs again
/// (`sql::migrate`): only an unfinished one, never a step that completed.
pub const FORGET_UNFINISHED_STEP: Stmt = Stmt {
    pg: "DELETE FROM _sqlx_migrations WHERE version = $1 AND success = FALSE",
    my: "DELETE FROM _sqlx_migrations WHERE version = ? AND success = FALSE",
};

/// What `import-local` asks before it copies: a database that has any of
/// these is not empty, and is refused.
pub const AUTHORED_COUNT: Stmt =
    both!("SELECT (SELECT COUNT(*) FROM accounts) + (SELECT COUNT(*) FROM servers) + (SELECT COUNT(*) FROM scripts)");

// ---- accounts and invitations ---------------------------------------------------

pub const ACCOUNTS: Stmt = both!("SELECT doc FROM accounts ORDER BY name");
pub const INVITES: Stmt = both!("SELECT doc FROM invites ORDER BY id");
pub const ACCOUNT_PUT: Stmt = Stmt {
    pg: "INSERT INTO accounts (name, doc) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET doc = EXCLUDED.doc",
    my: "INSERT INTO accounts (name, doc) VALUES (?, ?) AS new \
         ON DUPLICATE KEY UPDATE doc = new.doc",
};
pub const ACCOUNT_DELETE: Stmt = Stmt {
    pg: "DELETE FROM accounts WHERE name = $1",
    my: "DELETE FROM accounts WHERE name = ?",
};
pub const INVITE_PUT: Stmt = Stmt {
    pg: "INSERT INTO invites (id, expires_at_secs, doc) VALUES ($1, $2, $3) \
         ON CONFLICT (id) DO UPDATE SET expires_at_secs = EXCLUDED.expires_at_secs, doc = EXCLUDED.doc",
    my: "INSERT INTO invites (id, expires_at_secs, doc) VALUES (?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE expires_at_secs = new.expires_at_secs, doc = new.doc",
};
pub const INVITE_DELETE: Stmt = Stmt {
    pg: "DELETE FROM invites WHERE id = $1",
    my: "DELETE FROM invites WHERE id = ?",
};
pub const SIGNUPS: Stmt = both!("SELECT doc FROM signups ORDER BY id");
pub const SIGNUP_PUT: Stmt = Stmt {
    pg: "INSERT INTO signups (id, expires_at_secs, doc) VALUES ($1, $2, $3) \
         ON CONFLICT (id) DO UPDATE SET expires_at_secs = EXCLUDED.expires_at_secs, doc = EXCLUDED.doc",
    my: "INSERT INTO signups (id, expires_at_secs, doc) VALUES (?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE expires_at_secs = new.expires_at_secs, doc = new.doc",
};
pub const SIGNUP_DELETE: Stmt = Stmt {
    pg: "DELETE FROM signups WHERE id = $1",
    my: "DELETE FROM signups WHERE id = ?",
};

// ---- logins, the setup token and the throttle (leases) ------------------------------

pub const LOGINS: Stmt = both!("SELECT login_key, last_used_secs, doc FROM logins");
pub const LOGIN_GET: Stmt = Stmt {
    pg: "SELECT login_key, last_used_secs, doc FROM logins WHERE login_key = $1",
    my: "SELECT login_key, last_used_secs, doc FROM logins WHERE login_key = ?",
};
/// A login's last use never moves back: two processes flush their own.
pub const LOGIN_PUT: Stmt = Stmt {
    pg: "INSERT INTO logins (login_key, account, last_used_secs, doc) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (login_key) DO UPDATE \
         SET last_used_secs = GREATEST(logins.last_used_secs, EXCLUDED.last_used_secs)",
    my: "INSERT INTO logins (login_key, account, last_used_secs, doc) VALUES (?, ?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE last_used_secs = GREATEST(logins.last_used_secs, new.last_used_secs)",
};
/// A use moves the time forward on a login that is still there, and never
/// puts back one another process signed out.
pub const LOGIN_USED: Stmt = Stmt {
    pg: "UPDATE logins SET last_used_secs = GREATEST(last_used_secs, $1) WHERE login_key = $2",
    my: "UPDATE logins SET last_used_secs = GREATEST(last_used_secs, ?) WHERE login_key = ?",
};
pub const LOGIN_DELETE: Stmt = Stmt {
    pg: "DELETE FROM logins WHERE login_key = $1",
    my: "DELETE FROM logins WHERE login_key = ?",
};

pub const SETUP_COUNT: Stmt = both!("SELECT COUNT(*) FROM setup_token");
pub const SETUP_GET: Stmt = both!("SELECT token_sha256 FROM setup_token WHERE id = 1");
pub const SETUP_PUT: Stmt = Stmt {
    pg: "INSERT INTO setup_token (id, token_sha256, created_at_secs) VALUES (1, $1, $2) \
         ON CONFLICT (id) DO UPDATE SET token_sha256 = EXCLUDED.token_sha256, \
         created_at_secs = EXCLUDED.created_at_secs",
    my: "INSERT INTO setup_token (id, token_sha256, created_at_secs) VALUES (1, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE token_sha256 = new.token_sha256, created_at_secs = new.created_at_secs",
};
pub const SETUP_PUT_IF_NONE: Stmt = Stmt {
    pg: "INSERT INTO setup_token (id, token_sha256, created_at_secs) VALUES (1, $1, $2) \
         ON CONFLICT (id) DO NOTHING",
    my: "INSERT IGNORE INTO setup_token (id, token_sha256, created_at_secs) VALUES (1, ?, ?)",
};
pub const SETUP_TAKE: Stmt = both!("DELETE FROM setup_token");

pub const THROTTLE_ADD: Stmt = Stmt {
    pg: "INSERT INTO throttle (process_id, key_sha256, attempt_key, at_ms) VALUES ($1, $2, $3, $4)",
    my: "INSERT INTO throttle (process_id, key_sha256, attempt_key, at_ms) VALUES (?, ?, ?, ?)",
};
pub const THROTTLE_CLEAR: Stmt = Stmt {
    pg: "DELETE FROM throttle WHERE key_sha256 = $1",
    my: "DELETE FROM throttle WHERE key_sha256 = ?",
};
pub const THROTTLE_OTHERS: Stmt = Stmt {
    pg: "SELECT attempt_key, at_ms FROM throttle WHERE process_id <> $1 AND at_ms >= $2",
    my: "SELECT attempt_key, at_ms FROM throttle WHERE process_id <> ? AND at_ms >= ?",
};
pub const THROTTLE_PRUNE: Stmt = Stmt {
    pg: "DELETE FROM throttle WHERE at_ms < $1",
    my: "DELETE FROM throttle WHERE at_ms < ?",
};

// ---- the server list ------------------------------------------------------------

pub const SERVERS: Stmt = both!("SELECT doc FROM servers ORDER BY id");
pub const SERVER_FOR_UPDATE: Stmt = Stmt {
    pg: "SELECT doc FROM servers WHERE id = $1 FOR UPDATE",
    my: "SELECT doc FROM servers WHERE id = ? FOR UPDATE",
};
pub const SERVER_PUT: Stmt = Stmt {
    pg: "INSERT INTO servers (id, owner, version, doc) VALUES ($1, $2, 1, $3) \
         ON CONFLICT (id) DO UPDATE SET owner = EXCLUDED.owner, version = servers.version + 1, \
         doc = EXCLUDED.doc",
    my: "INSERT INTO servers (id, owner, version, doc) VALUES (?, ?, 1, ?) AS new \
         ON DUPLICATE KEY UPDATE owner = new.owner, version = servers.version + 1, doc = new.doc",
};
pub const SERVER_REPLACE: Stmt = Stmt {
    pg: "UPDATE servers SET owner = $1, version = version + 1, doc = $2 WHERE id = $3",
    my: "UPDATE servers SET owner = ?, version = version + 1, doc = ? WHERE id = ?",
};
pub const SERVER_DELETE: Stmt = Stmt {
    pg: "DELETE FROM servers WHERE id = $1",
    my: "DELETE FROM servers WHERE id = ?",
};

// ---- the recycle bin ------------------------------------------------------------

pub const BIN_ENSURE: Stmt = Stmt {
    pg: "INSERT INTO trash_bins (server_id) VALUES ($1) ON CONFLICT (server_id) DO NOTHING",
    my: "INSERT IGNORE INTO trash_bins (server_id) VALUES (?)",
};
/// Holds the server's bin until the transaction ends: one put at a time
/// makes room in it.
pub const BIN_LOCK: Stmt = Stmt {
    pg: "SELECT server_id FROM trash_bins WHERE server_id = $1 FOR UPDATE",
    my: "SELECT server_id FROM trash_bins WHERE server_id = ? FOR UPDATE",
};
pub const BIN_DELETE: Stmt = Stmt {
    pg: "DELETE FROM trash_bins WHERE server_id = $1",
    my: "DELETE FROM trash_bins WHERE server_id = ?",
};
/// What a bin holds, newest first: a tie in time is broken by the id, which
/// is time-ordered too.
pub const TRASH_SIZES: Stmt = Stmt {
    pg: "SELECT id, size_bytes FROM trash WHERE server_id = $1 AND deleted_at > $2 \
         ORDER BY deleted_at DESC, id DESC",
    my: "SELECT id, size_bytes FROM trash WHERE server_id = ? AND deleted_at > ? \
         ORDER BY deleted_at DESC, id DESC",
};
pub const TRASH_LIST: Stmt = Stmt {
    pg: "SELECT meta FROM trash WHERE server_id = $1 AND deleted_at > $2 ORDER BY deleted_at DESC, id DESC",
    my: "SELECT meta FROM trash WHERE server_id = ? AND deleted_at > ? ORDER BY deleted_at DESC, id DESC",
};
pub const TRASH_ADD: Stmt = Stmt {
    pg: "INSERT INTO trash (server_id, id, deleted_at, size_bytes, meta) VALUES ($1, $2, $3, $4, $5)",
    my: "INSERT INTO trash (server_id, id, deleted_at, size_bytes, meta) VALUES (?, ?, ?, ?, ?)",
};
pub const PAYLOAD_ADD: Stmt = Stmt {
    pg: "INSERT INTO trash_payloads (server_id, id, payload) VALUES ($1, $2, $3)",
    my: "INSERT INTO trash_payloads (server_id, id, payload) VALUES (?, ?, ?)",
};
pub const TRASH_GET: Stmt = Stmt {
    pg: "SELECT t.meta, p.payload FROM trash t \
         JOIN trash_payloads p ON p.server_id = t.server_id AND p.id = t.id \
         WHERE t.server_id = $1 AND t.id = $2 AND t.deleted_at > $3",
    my: "SELECT t.meta, p.payload FROM trash t \
         JOIN trash_payloads p ON p.server_id = t.server_id AND p.id = t.id \
         WHERE t.server_id = ? AND t.id = ? AND t.deleted_at > ?",
};
pub const TRASH_META: Stmt = Stmt {
    pg: "SELECT meta FROM trash WHERE server_id = $1 AND id = $2",
    my: "SELECT meta FROM trash WHERE server_id = ? AND id = ?",
};
pub const TRASH_DELETE: Stmt = Stmt {
    pg: "DELETE FROM trash WHERE server_id = $1 AND id = $2",
    my: "DELETE FROM trash WHERE server_id = ? AND id = ?",
};
pub const PAYLOAD_DELETE: Stmt = Stmt {
    pg: "DELETE FROM trash_payloads WHERE server_id = $1 AND id = $2",
    my: "DELETE FROM trash_payloads WHERE server_id = ? AND id = ?",
};
pub const TRASH_DELETE_SERVER: Stmt = Stmt {
    pg: "DELETE FROM trash WHERE server_id = $1",
    my: "DELETE FROM trash WHERE server_id = ?",
};
pub const PAYLOADS_DELETE_SERVER: Stmt = Stmt {
    pg: "DELETE FROM trash_payloads WHERE server_id = $1",
    my: "DELETE FROM trash_payloads WHERE server_id = ?",
};
pub const PAYLOADS_PURGE: Stmt = Stmt {
    pg: "DELETE FROM trash_payloads WHERE (server_id, id) IN \
         (SELECT server_id, id FROM trash WHERE deleted_at <= $1)",
    my: "DELETE FROM trash_payloads WHERE (server_id, id) IN \
         (SELECT server_id, id FROM trash WHERE deleted_at <= ?)",
};
pub const TRASH_PURGE: Stmt = Stmt {
    pg: "DELETE FROM trash WHERE deleted_at <= $1",
    my: "DELETE FROM trash WHERE deleted_at <= ?",
};

// ---- the metrics history ----------------------------------------------------------

/// Kept unless the server has this minute's or a later one — what a page
/// that sends late finds — and nothing when two processes race for one
/// minute: the key refuses the second.
pub const METRIC_ADD: Stmt = Stmt {
    pg: "INSERT INTO metrics (server_id, minute, ts_ms, sample) SELECT $1, $2, $3, $4 \
         WHERE NOT EXISTS (SELECT 1 FROM metrics WHERE server_id = $5 AND minute >= $6) \
         ON CONFLICT (server_id, minute) DO NOTHING",
    my: "INSERT IGNORE INTO metrics (server_id, minute, ts_ms, sample) SELECT ?, ?, ?, ? FROM DUAL \
         WHERE NOT EXISTS (SELECT 1 FROM metrics WHERE server_id = ? AND minute >= ?)",
};
/// A sample copied by `import-local`, at the time it was taken.
pub const METRIC_IMPORT: Stmt = Stmt {
    pg: "INSERT INTO metrics (server_id, minute, ts_ms, sample) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (server_id, minute) DO NOTHING",
    my: "INSERT IGNORE INTO metrics (server_id, minute, ts_ms, sample) VALUES (?, ?, ?, ?)",
};
pub const METRICS_SINCE: Stmt = Stmt {
    pg: "SELECT sample FROM metrics WHERE server_id = $1 AND ts_ms >= $2 ORDER BY ts_ms",
    my: "SELECT sample FROM metrics WHERE server_id = ? AND ts_ms >= ? ORDER BY ts_ms",
};
pub const METRICS_DELETE_SERVER: Stmt = Stmt {
    pg: "DELETE FROM metrics WHERE server_id = $1",
    my: "DELETE FROM metrics WHERE server_id = ?",
};
pub const METRICS_PRUNE: Stmt = Stmt {
    pg: "DELETE FROM metrics WHERE ts_ms < $1",
    my: "DELETE FROM metrics WHERE ts_ms < ?",
};

// ---- the page's data ----------------------------------------------------------------

pub const ROWS_OF: Stmt = Stmt {
    pg: "SELECT row_table, row_key, row_value FROM page_rows WHERE account = $1 ORDER BY row_table, row_key",
    my: "SELECT row_table, row_key, row_value FROM page_rows WHERE account = ? ORDER BY row_table, row_key",
};
pub const ROW_PUT: Stmt = Stmt {
    pg: "INSERT INTO page_rows (account, row_table, row_key, row_value) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (account, row_table, row_key) DO UPDATE SET row_value = EXCLUDED.row_value",
    my: "INSERT INTO page_rows (account, row_table, row_key, row_value) VALUES (?, ?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE row_value = new.row_value",
};
pub const ROW_DELETE: Stmt = Stmt {
    pg: "DELETE FROM page_rows WHERE account = $1 AND row_table = $2 AND row_key = $3",
    my: "DELETE FROM page_rows WHERE account = ? AND row_table = ? AND row_key = ?",
};
/// What an account keeps, in bytes, as the local directory counts it.
pub const ROWS_SIZE: Stmt = Stmt {
    pg: "SELECT CAST(COALESCE(SUM(OCTET_LENGTH(row_key) + OCTET_LENGTH(row_value)), 0) AS BIGINT) \
         FROM page_rows WHERE account = $1",
    my: "SELECT CAST(COALESCE(SUM(OCTET_LENGTH(row_key) + OCTET_LENGTH(row_value)), 0) AS SIGNED) \
         FROM page_rows WHERE account = ?",
};
pub const ROWS_FORGET: Stmt = Stmt {
    pg: "DELETE FROM page_rows WHERE account = $1",
    my: "DELETE FROM page_rows WHERE account = ?",
};

pub const NOTES_OF: Stmt = Stmt {
    pg: "SELECT redis_key, doc FROM key_notes WHERE server_id = $1",
    my: "SELECT redis_key, doc FROM key_notes WHERE server_id = ?",
};
pub const NOTE_PUT: Stmt = Stmt {
    pg: "INSERT INTO key_notes (server_id, key_sha256, redis_key, doc) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (server_id, key_sha256) DO UPDATE SET redis_key = EXCLUDED.redis_key, doc = EXCLUDED.doc",
    my: "INSERT INTO key_notes (server_id, key_sha256, redis_key, doc) VALUES (?, ?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE redis_key = new.redis_key, doc = new.doc",
};
pub const NOTE_DELETE: Stmt = Stmt {
    pg: "DELETE FROM key_notes WHERE server_id = $1 AND key_sha256 = $2",
    my: "DELETE FROM key_notes WHERE server_id = ? AND key_sha256 = ?",
};
pub const NOTES_DELETE_SERVER: Stmt = Stmt {
    pg: "DELETE FROM key_notes WHERE server_id = $1",
    my: "DELETE FROM key_notes WHERE server_id = ?",
};

pub const SCRIPTS: Stmt = both!("SELECT doc FROM scripts ORDER BY id");
pub const SCRIPT_GET: Stmt = Stmt {
    pg: "SELECT doc FROM scripts WHERE id = $1",
    my: "SELECT doc FROM scripts WHERE id = ?",
};
pub const SCRIPT_PUT: Stmt = Stmt {
    pg: "INSERT INTO scripts (id, owner, doc) VALUES ($1, $2, $3) \
         ON CONFLICT (id) DO UPDATE SET owner = EXCLUDED.owner, doc = EXCLUDED.doc",
    my: "INSERT INTO scripts (id, owner, doc) VALUES (?, ?, ?) AS new \
         ON DUPLICATE KEY UPDATE owner = new.owner, doc = new.doc",
};
pub const SCRIPT_DELETE: Stmt = Stmt {
    pg: "DELETE FROM scripts WHERE id = $1",
    my: "DELETE FROM scripts WHERE id = ?",
};

// ---- the audit --------------------------------------------------------------------------

pub const AUDIT_ADD: Stmt = Stmt {
    pg: "INSERT INTO audit (at_ms, account, event, server_id, line) VALUES ($1, $2, $3, $4, $5)",
    my: "INSERT INTO audit (at_ms, account, event, server_id, line) VALUES (?, ?, ?, ?, ?)",
};
/// Every narrowing is two binds, whether it is asked and its value, so one
/// text serves every query the viewer makes.
pub const AUDIT_QUERY: Stmt = Stmt {
    pg: "SELECT id, line FROM audit \
         WHERE ($1 IS NULL OR at_ms >= $2) AND ($3 IS NULL OR at_ms <= $4) \
         AND ($5 IS NULL OR account = $6) AND ($7 IS NULL OR event = $8) \
         AND ($9 IS NULL OR server_id = $10) AND ($11 IS NULL OR id < $12) \
         ORDER BY id DESC LIMIT $13",
    my: "SELECT id, line FROM audit \
         WHERE (? IS NULL OR at_ms >= ?) AND (? IS NULL OR at_ms <= ?) \
         AND (? IS NULL OR account = ?) AND (? IS NULL OR event = ?) \
         AND (? IS NULL OR server_id = ?) AND (? IS NULL OR id < ?) \
         ORDER BY id DESC LIMIT ?",
};
pub const AUDIT_PRUNE: Stmt = Stmt {
    pg: "DELETE FROM audit WHERE at_ms < $1",
    my: "DELETE FROM audit WHERE at_ms < ?",
};
