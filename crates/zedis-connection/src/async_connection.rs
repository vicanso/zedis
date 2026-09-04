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

use super::config::{RedisServer, SERVER_TYPE_SENTINEL, get_server};
use super::ssh_cluster_connection::SshMultiplexedConnection;
use super::ssh_tunnel::{open_single_sni_tls_connection, open_single_ssh_tunnel_connection, tls_server_name};
use crate::error::{ConnectionErrorKind, Error};
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
use zedis_core::string::split_host_port;
use zedis_core::ttl_cache::{TtlCache, now_secs};

/// Name reported to Redis via `CLIENT SETNAME` — visible in `CLIENT LIST`,
/// `CLIENT INFO` and the slow log, so operators can tell which Zedis version
/// a connection came from.
///
/// The version is this crate's `CARGO_PKG_VERSION`, which tracks the app only
/// because every workspace member inherits `version` from `[workspace.package]`
/// (see the root `Cargo.toml`). Giving this crate a version of its own would
/// silently ship that number to Redis instead — it reported `zedis:v0.1.0` for
/// a while after the crate was split out. `client_name_matches_app_version` in
/// the app crate locks the two together.
const CLIENT_NAME: &str = concat!("zedis:v", env!("CARGO_PKG_VERSION"));

/// The `CLIENT SETNAME` value this build reports to Redis, e.g. `zedis:v0.5.4`.
pub fn client_name() -> &'static str {
    CLIENT_NAME
}

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
        connection_timeout: Duration::from_secs(10),
        response_timeout: Duration::from_secs(20),
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

/// Per-server connection timeout if set, else the global default.
pub fn resolve_connection_timeout(config: &RedisServer) -> Duration {
    config
        .connection_timeout
        .map(Duration::from_secs)
        .unwrap_or_else(get_redis_connection_timeout)
}

/// Per-server response timeout if set, else the global default.
pub fn resolve_response_timeout(config: &RedisServer) -> Duration {
    config
        .response_timeout
        .map(Duration::from_secs)
        .unwrap_or_else(get_redis_response_timeout)
}

/// Post-connect client setup, all best-effort.
///
/// - `SETNAME` identifies the connection in `CLIENT LIST` / the slow log.
///   Failure is logged at error level — SETNAME exists everywhere, so an
///   error here is real signal.
/// - `NO-EVICT ON` (Redis ≥ 7.0): an admin GUI should stay connected
///   exactly when the server is under memory pressure and evicting
///   clients — the moment the operator needs it most.
/// - `NO-TOUCH ON` (Redis ≥ 7.2): browsing a key must not distort the
///   LRU/LFU accounting the memory analyzer's `OBJECT IDLETIME`/`FREQ`
///   heat column reports — observation shouldn't perturb the observed.
///
/// The two flags are silently skipped where unsupported (older servers,
/// proxies, NOPERM-restricted users) — debug log only.
pub(crate) async fn configure_client_connection(conn: &mut impl ConnectionLike) {
    if let Err(err) = cmd("CLIENT").arg("SETNAME").arg(CLIENT_NAME).exec_async(conn).await {
        error!(error = %err, "set client name failed");
    }
    // `CLIENT SETINFO` (7.2+) fills the `lib-name` / `lib-ver` columns of
    // `CLIENT LIST`; older servers answer "unknown subcommand".
    for (field, value) in [("LIB-NAME", "zedis"), ("LIB-VER", env!("CARGO_PKG_VERSION"))] {
        if let Err(err) = cmd("CLIENT")
            .arg("SETINFO")
            .arg(field)
            .arg(value)
            .exec_async(conn)
            .await
        {
            debug!(error = %err, field, "client setinfo not applied");
        }
    }
    for flag in ["NO-EVICT", "NO-TOUCH"] {
        if let Err(err) = cmd("CLIENT").arg(flag).arg("ON").exec_async(conn).await {
            debug!(error = %err, flag, "client flag not applied");
        }
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

    // Create a new connection: SSH tunnel, TLS with its own server name, or
    // the URL-based client for everything else (plain, TLS, Unix socket).
    let mut conn = if config.is_ssh_tunnel() {
        if config.is_unix_socket() {
            return Err(Error::Invalid {
                message: "a Unix socket cannot be reached through an SSH tunnel".to_string(),
            });
        }
        open_single_ssh_tunnel_connection(config).await?
    } else if config.tls.unwrap_or(false)
        && let Some(server_name) = tls_server_name(config)
    {
        open_single_sni_tls_connection(config, &server_name).await?
    } else {
        let client = open_single_client(config)?;
        // Configure connection with timeouts
        let cfg = AsyncConnectionConfig::default()
            .set_connection_timeout(Some(resolve_connection_timeout(config)))
            .set_response_timeout(Some(resolve_response_timeout(config)));
        client.get_multiplexed_async_connection_with_config(&cfg).await?
    };

    configure_client_connection(&mut conn).await;
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
/// The first connection to a configured endpoint, before its topology is
/// known — what discovery, the form's Test button and the diagnostics dial.
///
/// Sentinel nodes may carry credentials of their own. With
/// `sentinel_username` / `sentinel_password` set they are used outright on
/// an entry pinned to Sentinel, and as the retry when the data-node
/// credentials are refused on an auto-detected one. Without them an
/// authentication failure retries with no password at all — the legacy
/// accommodation for an auth-less sentinel in front of a protected master,
/// which until now was the *only* way a sentinel could differ from its
/// data nodes.
///
/// A Sentinel entry may list several seed addresses (`host:port, …` in
/// its host field): they are tried in order, and only a failure that is
/// not about credentials moves on to the next.
pub async fn open_seed_connection(config: &RedisServer) -> Result<MultiplexedConnection> {
    let endpoints = config.seed_endpoints();
    if endpoints.len() <= 1 {
        return open_seed_endpoint(config).await;
    }
    let mut last_error = None;
    for (host, port) in endpoints {
        let mut seed = config.clone();
        seed.host = host;
        seed.port = port;
        match open_seed_endpoint(&seed).await {
            Ok(conn) => return Ok(conn),
            Err(e) if e.connection_kind() == ConnectionErrorKind::Auth => return Err(e),
            Err(e) => {
                debug!(name = config.name, host = seed.host, port = seed.port, error = %e, "seed unreachable, trying the next");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| Error::Invalid {
        message: "no seed address configured".to_string(),
    }))
}

pub(crate) async fn open_seed_endpoint(config: &RedisServer) -> Result<MultiplexedConnection> {
    if config.server_type == Some(SERVER_TYPE_SENTINEL) && config.has_sentinel_credentials() {
        return open_single_connection(&config.sentinel_login(), 0, false).await;
    }
    match open_single_connection(config, 0, false).await {
        Ok(conn) => Ok(conn),
        Err(e) if e.connection_kind() == ConnectionErrorKind::Auth => {
            let retry = if config.has_sentinel_credentials() {
                debug!(
                    name = config.name,
                    "data credentials refused, retrying with the sentinel's"
                );
                config.sentinel_login()
            } else {
                debug!(
                    name = config.name,
                    "credentials refused, retrying without a password (sentinel?)"
                );
                let mut anonymous = config.clone();
                anonymous.password = None;
                anonymous
            };
            open_single_connection(&retry, 0, false).await
        }
        Err(e) => Err(e),
    }
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
pub fn open_single_client(config: &RedisServer) -> Result<Client> {
    let url = config.get_connection_url();
    // Build client with TLS if certificates are provided
    let client = if let Some(certificates) = config.tls_certificates()? {
        Client::build_with_tls(url, certificates)?
    } else {
        Client::open(url)?
    };
    Ok(client)
}

/// Opens a dedicated `Monitor` connection for the given Redis server.
///
/// This creates a non-multiplexed connection suitable for the Redis MONITOR
/// command, which streams all commands received by the server.
pub async fn open_monitor_connection(config: &RedisServer) -> Result<redis::aio::Monitor> {
    let client = open_single_client(config)?;
    let monitor = client.get_async_monitor().await?;
    Ok(monitor)
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
    addrs: &[RedisServer],
    db: usize,
    cmds: Vec<Option<Cmd>>,
) -> Result<Vec<Option<T>>> {
    if addrs.len() != cmds.len() {
        return Err(Error::Invalid {
            message: "Commands and addresses length mismatch".to_string(),
        });
    }
    let tasks = addrs.iter().enumerate().map(|(index, addr)| {
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
            let mut conn = open_single_connection(addr, db, true).await?;

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

/// Open a one-shot connection to a specific cluster node by `host:port`,
/// reusing the named server's stored auth (password / TLS / SSH tunnel
/// config). Bypasses the connection cache (`use_cache=false`) — cluster
/// topology ops are rare administrative actions, so caching one
/// `MultiplexedConnection` per node would bloat the pool for ops that
/// don't repeat. The returned conn is expected to be dropped after the
/// single command runs.
///
/// Used by `CLUSTER FAILOVER` and `CLUSTER REPLICATE`, both of which
/// must execute *on* the target node (Redis routes them to the local
/// instance only — they are not gossiped), so an ad-hoc connection
/// to that specific host:port is the correct shape.
pub async fn open_node_connection(server_name: &str, host_port: &str) -> Result<MultiplexedConnection> {
    open_node_connection_inner(server_name, host_port, false).await
}

/// Cache-backed variant of [`open_node_connection`] for *recurring* per-node
/// traffic — the Topology Load poll samples every master on an interval, and
/// rebuilding a fresh TCP+auth (+TLS/SSH) handshake per node per tick is
/// pure churn. The pooled `MultiplexedConnection` is shared and must not be
/// used for connection-scoped state.
pub async fn open_node_connection_cached(server_name: &str, host_port: &str) -> Result<MultiplexedConnection> {
    open_node_connection_inner(server_name, host_port, true).await
}

async fn open_node_connection_inner(
    server_name: &str,
    host_port: &str,
    use_cache: bool,
) -> Result<MultiplexedConnection> {
    // Last colon is the port separator, so an IPv6 literal parses whether
    // it arrives bracketed (a label) or bare (`CLUSTER NODES`).
    let (host, port) = split_host_port(host_port).ok_or_else(|| Error::Invalid {
        message: format!("invalid host:port \"{host_port}\""),
    })?;
    let mut config = get_server(server_name)?;
    config.host = host.to_string();
    config.port = port;
    open_single_connection(&config, 0, use_cache).await
}
