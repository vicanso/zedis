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

//! [`ServerDb`] — where an operation runs.
//!
//! The view layer is meant to draw and to ask, not to know what a Redis
//! command looks like or what a connection is (`tests/view_layering.rs` holds
//! that line). So the typed operations of this crate that a view calls take
//! a `ServerDb` — *which* database of *which* configured server — and find
//! their own connection. A view gets one from its server state and hands it
//! over; it cannot get a connection out of it.

use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::manager::{RedisClient, get_connection_manager};

/// One database of one configured server, by id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerDb {
    server_id: String,
    db: usize,
}

impl ServerDb {
    pub fn new(server_id: impl Into<String>, db: usize) -> Self {
        Self {
            server_id: server_id.into(),
            db,
        }
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn db(&self) -> usize {
        self.db
    }

    /// The pooled connection the operations of this crate run on. Crate
    /// private on purpose: a caller with a connection in hand is a caller
    /// that builds commands.
    pub(crate) async fn connection(&self) -> Result<RedisAsyncConn, Error> {
        get_connection_manager().get_connection(&self.server_id, self.db).await
    }

    /// The pooled client, for an operation that fans out to every master
    /// (`CLIENT LIST`, `CONFIG SET`, …). Crate private like
    /// [`Self::connection`], and for the same reason.
    pub(crate) async fn client(&self) -> Result<RedisClient, Error> {
        get_connection_manager().get_client(&self.server_id, self.db).await
    }

    /// A connection of the caller's own, built from the pooled client's
    /// topology — the terminal's, where `SELECT` and `MULTI` must not leak
    /// into the pool (ADR 4). A bridge session in the browser.
    pub(crate) async fn dedicated_connection(&self) -> Result<RedisAsyncConn, Error> {
        get_connection_manager()
            .open_dedicated_connection(&self.server_id, self.db)
            .await
    }
}
