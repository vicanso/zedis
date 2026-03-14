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

use super::config::RedisServer;
use super::ssh_cluster_connection::SshMultiplexedConnection;
use super::ssh_tunnel::open_single_ssh_tunnel_connection;
use crate::error::Error;
use crate::helpers::{TtlCache, now_secs};
use arc_swap::ArcSwap;
use futures::future::try_join_all;
use redis::{
    AsyncConnectionConfig, Client, Cmd, FromRedisValue, Pipeline, RedisFuture, Value,
    aio::{ConnectionLike, MultiplexedConnection},
    cluster_async::ClusterConnection,
    cmd,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::{sync::LazyLock, time::Duration};
use tracing::{debug, error};

const CLIENT_NAME: &str = concat!("zedis:v", env!("CARGO_PKG_VERSION"));

type Result<T, E = Error> = std::result::Result<T, E>;

static DELAY: LazyLock<Option<Duration>> = LazyLock::new(|| {
    let value = std::env::var("REDIS_DELAY").unwrap_or_default();
    humantime::parse_duration(&value).ok()
});

struct MultiplexedConnectionCache {
    conn: MultiplexedConnection,
    check_time: AtomicU64,
}

impl MultiplexedConnectionCache {
    async fn get_connection(&self) -> Option<MultiplexedConnection> {
        let now = now_secs();
        let last_check = self.check_time.load(Ordering::Acquire);
        if now - last_check < 60 {
            return Some(self.conn.clone());
        }
        let mut conn = self.conn.clone();
        if let Ok(()) = cmd("PING").query_async(&mut conn).await {
            self.check_time.store(now, Ordering::Release);
            return Some(conn);
        }
        None
    }
}

/// Global connection pool that caches Redis connections.
/// Key: (config_hash, database_number), Value: MultiplexedConnection
static CONNECTION_POOL: LazyLock<TtlCache<u64, Arc<MultiplexedConnectionCache>>> =
    LazyLock::new(|| TtlCache::new(Duration::from_secs(5 * 60)));

/// Clears expired connections from the connection pool.
pub fn clear_expired_connection_pool() -> (usize, usize) {
    CONNECTION_POOL.clear_expired()
}

struct RedisConfig {
    connection_timeout: Duration,
    response_timeout: Duration,
}

static GLOBAL_REDIS_CONFIG: LazyLock<ArcSwap<RedisConfig>> = LazyLock::new(|| {
    ArcSwap::from_pointee(RedisConfig {
        connection_timeout: Duration::from_secs(30),
        response_timeout: Duration::from_secs(60),
    })
});

pub fn set_redis_connection_timeout(timeout: Duration) {
    let current = GLOBAL_REDIS_CONFIG.load();
    let new_config = RedisConfig {
        connection_timeout: timeout,
        response_timeout: current.response_timeout,
    };
    GLOBAL_REDIS_CONFIG.store(Arc::new(new_config));
}
pub fn set_redis_response_timeout(timeout: Duration) {
    let current = GLOBAL_REDIS_CONFIG.load();
    let new_config = RedisConfig {
        connection_timeout: current.connection_timeout,
        response_timeout: timeout,
    };
    GLOBAL_REDIS_CONFIG.store(Arc::new(new_config));
}

pub fn get_redis_connection_timeout() -> Duration {
    GLOBAL_REDIS_CONFIG.load().connection_timeout
}

pub fn get_redis_response_timeout() -> Duration {
    GLOBAL_REDIS_CONFIG.load().response_timeout
}

pub(crate) async fn set_client_name(conn: &mut impl ConnectionLike) {
    // ignore error
    if let Err(err) = cmd("CLIENT").arg("SETNAME").arg(CLIENT_NAME).exec_async(conn).await {
        error!(error = %err, "set client name failed");
    }
}

/// Opens a single Redis connection with connection pooling support.
///
/// This function attempts to reuse an existing connection from the pool if available
/// and healthy (when caching is enabled). If not, or if caching is bypassed, it creates
/// a new connection (either through SSH tunnel or direct).
/// The connection is then configured to use the specified database.
///
/// # Arguments
///
/// * `config` - Redis server configuration
/// * `db` - Database number to select (0-15 typically)
/// * `use_cache` - If true, attempts to retrieve from and store the connection in the global pool
///
/// # Returns
///
/// A multiplexed Redis connection connected to the specified database
pub async fn open_single_connection(config: &RedisServer, db: usize, use_cache: bool) -> Result<MultiplexedConnection> {
    // Generate a unique key for this connection based on config hash and database number
    let key = config.get_hash(db);

    // Try to reuse an existing connection from the pool if caching is enabled
    if use_cache
        && let Some(conn) = CONNECTION_POOL.get(&key)
        && let Some(conn) = conn.get_connection().await
    {
        debug!(name = config.name, "get connection from pool");
        return Ok(conn);
    }

    // Create a new connection: SSH tunnel or direct connection
    let mut conn = if config.is_ssh_tunnel() {
        open_single_ssh_tunnel_connection(config).await?
    } else {
        let client = open_single_client(config)?;
        // Configure connection with timeouts
        let cfg = AsyncConnectionConfig::default()
            .set_connection_timeout(Some(get_redis_connection_timeout()))
            .set_response_timeout(Some(get_redis_response_timeout()));
        client.get_multiplexed_async_connection_with_config(&cfg).await?
    };

    set_client_name(&mut conn).await;
    // Select the specified database if not the default (db 0)
    if db != 0 {
        let _: () = cmd("SELECT").arg(db).query_async(&mut conn).await?;
        debug!(name = config.name, db, "select database");
    }
    if !use_cache {
        return Ok(conn);
    }

    // Cache the connection in the pool for future reuse if caching is enabled
    CONNECTION_POOL.insert(
        key,
        Arc::new(MultiplexedConnectionCache {
            conn: conn.clone(),
            check_time: AtomicU64::new(now_secs()),
        }),
    );

    Ok(conn)
}
pub fn remove_connection_from_pool(config: &RedisServer, db: usize) {
    let key = config.get_hash(db);
    CONNECTION_POOL.remove(&key);
}
/// Creates a Redis client from the server configuration.
///
/// This function builds either a TLS-enabled or regular Redis client
/// based on the configuration.
///
/// # Arguments
///
/// * `config` - Redis server configuration
///
/// # Returns
///
/// A Redis client ready to establish connections
fn open_single_client(config: &RedisServer) -> Result<Client> {
    let url = config.get_connection_url();
    // Build client with TLS if certificates are provided
    let client = if let Some(certificates) = config.tls_certificates() {
        Client::build_with_tls(url, certificates)?
    } else {
        Client::open(url)?
    };
    Ok(client)
}

/// A wrapper enum for Redis asynchronous connections.
///
/// This unifies `MultiplexedConnection` (for single nodes) and
/// `ClusterConnection` (for clusters) under a single type,
/// allowing generic usage across the application.
#[derive(Clone)]
pub enum RedisAsyncConn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
    SshCluster(ClusterConnection<SshMultiplexedConnection>),
}

impl ConnectionLike for RedisAsyncConn {
    #[inline]
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        let cmd_future = match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::SshCluster(conn) => conn.req_packed_command(cmd),
        };
        if let Some(delay) = *DELAY {
            return Box::pin(async move {
                smol::Timer::after(delay).await;
                cmd_future.await
            });
        }
        cmd_future
    }
    #[inline]
    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        let cmd_future = match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisAsyncConn::SshCluster(conn) => conn.req_packed_commands(cmd, offset, count),
        };
        if let Some(delay) = *DELAY {
            return Box::pin(async move {
                smol::Timer::after(delay).await;
                cmd_future.await
            });
        }
        cmd_future
    }
    #[inline]
    fn get_db(&self) -> i64 {
        match self {
            RedisAsyncConn::Single(conn) => conn.get_db(),
            RedisAsyncConn::Cluster(_) => 0,
            RedisAsyncConn::SshCluster(conn) => conn.get_db(),
        }
    }
}

/// Queries multiple Redis master nodes concurrently.
///
/// This function establishes connections to all provided addresses in parallel
/// and executes the corresponding commands.
///
/// # Arguments
///
/// * `addrs` - A vector of Redis connection strings (e.g., "redis://127.0.0.1").
/// * `cmds` - A vector of commands to execute.
pub(crate) async fn query_async_masters<T: FromRedisValue>(
    addrs: Vec<RedisServer>,
    db: usize,
    cmds: Vec<Option<Cmd>>,
) -> Result<Vec<Option<T>>> {
    if addrs.len() != cmds.len() {
        return Err(Error::Invalid {
            message: "Commands and addresses length mismatch".to_string(),
        });
    }
    let tasks = addrs.into_iter().enumerate().map(|(index, addr)| {
        // Clone data to move ownership into the async block.
        let current_cmd = cmds[index].clone();

        async move {
            if let Some(delay) = *DELAY {
                smol::Timer::after(delay).await;
            }
            let Some(current_cmd) = current_cmd else {
                return Ok::<Option<T>, Error>(None);
            };
            // Establish a multiplexed async connection to the specific node.
            let mut conn = open_single_connection(&addr, db, true).await?;

            // Execute the command asynchronously.
            let value: T = current_cmd.query_async(&mut conn).await?;

            Ok::<Option<T>, Error>(Some(value))
        }
    });

    let values = try_join_all(tasks).await?;

    Ok(values)
}

/// Queries multiple Redis master nodes concurrently using pipelines.
///
/// Similar to `query_async_masters`, but executes a `Pipeline` (batched commands)
/// per node instead of a single `Cmd`.
///
/// # Arguments
///
/// * `addrs` - A vector of Redis server configurations.
/// * `db` - Database number to select.
/// * `pipes` - A vector of optional pipelines to execute on each node.
pub(crate) async fn query_async_masters_pipeline(
    addrs: Vec<RedisServer>,
    db: usize,
    pipes: Vec<Option<Pipeline>>,
) -> Result<Vec<Option<Vec<Value>>>> {
    if addrs.len() != pipes.len() {
        return Err(Error::Invalid {
            message: "Pipelines and addresses length mismatch".to_string(),
        });
    }
    let tasks = addrs.into_iter().enumerate().map(|(index, addr)| {
        let current_pipe = pipes[index].clone();

        async move {
            if let Some(delay) = *DELAY {
                smol::Timer::after(delay).await;
            }
            let Some(current_pipe) = current_pipe else {
                return Ok::<Option<Vec<Value>>, Error>(None);
            };
            let mut conn = open_single_connection(&addr, db, true).await?;

            let values: Vec<Value> = current_pipe.query_async(&mut conn).await?;

            Ok::<Option<Vec<Value>>, Error>(Some(values))
        }
    });

    let values = try_join_all(tasks).await?;

    Ok(values)
}
