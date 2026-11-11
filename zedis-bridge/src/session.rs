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

//! Connections a caller keeps to itself.
//!
//! Most requests are stateless and share the pool. The terminal is not: in
//! Redis, `SELECT`, `AUTH`, `CLIENT SETNAME` and `MULTI`/`EXEC` are
//! *connection* state, so a stateless bridge would scatter a transaction
//! across backend connections and the `EXEC` would run against a connection
//! that never saw the `MULTI` (ADR 4 makes the same point for the desktop
//! terminal, which owns its connection for exactly this reason).
//!
//! A session pins one dedicated connection for one caller. It expires on
//! idle, because a browser tab that closes mid-transaction never sends the
//! DELETE and the backend connection would otherwise be held forever.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use uuid::Uuid;
use zedis_connection::{RedisAsyncConn, get_connection_manager};

/// How long a session may go unused before it is swept. Long enough that a
/// human typing a transaction in the web terminal does not lose it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How many dedicated terminal sessions one account may hold open at once.
pub const MAX_PER_ACCOUNT: usize = 8;
/// Total dedicated terminal sessions the bridge holds across all accounts.
pub const MAX_TOTAL: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    /// Either [`MAX_PER_ACCOUNT`] for this caller or [`MAX_TOTAL`] overall.
    TooMany(&'static str),
    /// Upstream Redis refused or timed out opening the dedicated connection.
    Upstream(String),
}

struct Entry {
    account: String,
    conn: RedisAsyncConn,
    server_id: String,
    db: usize,
    last_used: Instant,
}

/// The live sessions, keyed by the opaque token handed to the caller.
#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, Entry>>>);

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_counts(total: usize, mine: usize) -> Result<(), SessionError> {
        if total >= MAX_TOTAL {
            return Err(SessionError::TooMany(
                "too many active terminal sessions on this bridge; try again shortly",
            ));
        }
        if mine >= MAX_PER_ACCOUNT {
            return Err(SessionError::TooMany(
                "at most 8 terminal sessions at once; close one first",
            ));
        }
        Ok(())
    }

    fn check_limits(guard: &HashMap<String, Entry>, account: &str) -> Result<(), SessionError> {
        let mine = guard.values().filter(|entry| entry.account == account).count();
        Self::check_counts(guard.len(), mine)
    }

    /// Pin a fresh dedicated connection for `account` and return its token.
    ///
    /// Dedicated, never pooled: the whole point is that what this caller does
    /// to the connection is not seen by anyone else.
    pub async fn open(&self, account: &str, server_id: &str, db: usize) -> Result<String, SessionError> {
        {
            let mut guard = self.0.lock().await;
            guard.retain(|_, entry| entry.last_used.elapsed() < IDLE_TIMEOUT);
            Self::check_limits(&guard, account)?;
        }
        let conn = get_connection_manager()
            .open_dedicated_connection(server_id, db)
            .await
            .map_err(|e| SessionError::Upstream(e.to_string()))?;
        let token = Uuid::new_v4().simple().to_string();
        let mut guard = self.0.lock().await;
        guard.retain(|_, entry| entry.last_used.elapsed() < IDLE_TIMEOUT);
        Self::check_limits(&guard, account)?;
        guard.insert(
            token.clone(),
            Entry {
                account: account.to_string(),
                conn,
                server_id: server_id.to_string(),
                db,
                last_used: Instant::now(),
            },
        );
        Ok(token)
    }

    /// The connection behind `token`, if it is live, belongs to `account`, and
    /// addresses the same server and database the request names.
    ///
    /// The account and server/db checks are not bookkeeping: a token that
    /// could be borrowed across accounts or pointed at another entry would let
    /// a caller run a command on a connection authenticated for someone else.
    pub async fn take(&self, token: &str, account: &str, server_id: &str, db: usize) -> Option<RedisAsyncConn> {
        let mut guard = self.0.lock().await;
        let entry = guard.get_mut(token)?;
        if entry.account != account || entry.server_id != server_id || entry.db != db {
            return None;
        }
        if entry.last_used.elapsed() >= IDLE_TIMEOUT {
            guard.remove(token);
            return None;
        }
        entry.last_used = Instant::now();
        Some(entry.conn.clone())
    }

    pub async fn close(&self, token: &str, account: &str) -> bool {
        let mut guard = self.0.lock().await;
        if guard.get(token).is_some_and(|entry| entry.account == account) {
            guard.remove(token).is_some()
        } else {
            false
        }
    }

    /// Drop every session bound to `server_id` (e.g. when the server's
    /// credentials or visibility change, or the entry is deleted).
    pub async fn close_server(&self, server_id: &str) -> usize {
        let mut guard = self.0.lock().await;
        let before = guard.len();
        guard.retain(|_, entry| entry.server_id != server_id);
        before - guard.len()
    }

    /// Drop everything idle past [`IDLE_TIMEOUT`], returning how many went.
    pub async fn sweep(&self) -> usize {
        let mut guard = self.0.lock().await;
        let before = guard.len();
        guard.retain(|_, entry| entry.last_used.elapsed() < IDLE_TIMEOUT);
        before - guard.len()
    }

    pub async fn len(&self) -> usize {
        self.0.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_counts_enforces_per_account_and_total_caps() {
        assert_eq!(Sessions::check_counts(0, 0), Ok(()));
        assert_eq!(Sessions::check_counts(MAX_TOTAL - 1, MAX_PER_ACCOUNT - 1), Ok(()));
        assert!(matches!(
            Sessions::check_counts(MAX_PER_ACCOUNT, MAX_PER_ACCOUNT),
            Err(SessionError::TooMany(_))
        ));
        assert!(matches!(
            Sessions::check_counts(MAX_TOTAL, 0),
            Err(SessionError::TooMany(_))
        ));
    }
}
