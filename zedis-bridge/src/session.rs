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

struct Entry {
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

    /// Pin a fresh dedicated connection and return its token.
    ///
    /// Dedicated, never pooled: the whole point is that what this caller does
    /// to the connection is not seen by anyone else.
    pub async fn open(&self, server_id: &str, db: usize) -> Result<String, String> {
        let conn = get_connection_manager()
            .open_dedicated_connection(server_id, db)
            .await
            .map_err(|e| e.to_string())?;
        let token = Uuid::new_v4().simple().to_string();
        self.0.lock().await.insert(
            token.clone(),
            Entry {
                conn,
                server_id: server_id.to_string(),
                db,
                last_used: Instant::now(),
            },
        );
        Ok(token)
    }

    /// The connection behind `token`, if it is live and addresses the same
    /// server and database the request names.
    ///
    /// The server/db check is not bookkeeping: a token that could be pointed
    /// at another entry would let a caller run a command on a connection
    /// authenticated for a different server.
    pub async fn take(&self, token: &str, server_id: &str, db: usize) -> Option<RedisAsyncConn> {
        let mut guard = self.0.lock().await;
        let entry = guard.get_mut(token)?;
        if entry.server_id != server_id || entry.db != db {
            return None;
        }
        entry.last_used = Instant::now();
        Some(entry.conn.clone())
    }

    pub async fn close(&self, token: &str) -> bool {
        self.0.lock().await.remove(token).is_some()
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
