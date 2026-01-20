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

use super::async_connection::{get_redis_connection_timeout, get_redis_response_timeout};
use super::config::RedisServer;
use super::ssh_stream::SshRedisStream;
use crate::error::Error;
use crate::helpers::get_home_dir;
use dashmap::DashMap;
use redis::{RedisConnectionInfo, aio::MultiplexedConnection, cmd};
use russh::client::{Handle, Handler};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key, load_secret_key};
use std::path::Path;
use std::sync::Arc;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tokio::runtime::Runtime;
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Global Tokio runtime for SSH tunnel operations.
/// Initialized lazily on first use and persists for the application lifetime.
static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Gets or initializes the global Tokio runtime for SSH operations.
///
/// This creates a dedicated multi-threaded runtime with 2 worker threads
/// specifically for handling SSH tunnel operations, separate from the main
/// application runtime to avoid blocking.
///
/// # Returns
///
/// A static reference to the Tokio runtime
fn get_tokio_runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("ssh-tunnel-worker")
            .build()
            .expect("Failed to build Tokio runtime")
    })
}

/// Runs an async future in the dedicated SSH tunnel Tokio runtime.
///
/// This function spawns the provided future in the dedicated SSH runtime
/// and waits for its completion. It's used to ensure SSH operations
/// run in their own runtime context without interfering with the main
/// application runtime.
///
/// # Arguments
///
/// * `future` - The async operation to execute
///
/// # Returns
///
/// The result of the future execution
pub async fn run_in_tokio<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let rt = get_tokio_runtime();
    let join_handle = rt.spawn(future);

    match join_handle.await {
        Ok(res) => res,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// SSH client handler for managing SSH connections.
///
/// This handler is used by the russh library to handle SSH client events
/// and callbacks during the connection lifecycle.
#[derive(Clone)]
pub struct ClientHandler {
    /// The remote SSH server hostname or IP address
    host: String,
    /// The remote SSH server port
    port: u16,
}

impl Handler for ClientHandler {
    type Error = russh::Error;

    /// Verifies the SSH server's public key during connection establishment.
    ///
    /// # Arguments
    ///
    /// * `_server_public_key` - The server's public key to validate
    ///
    /// # Returns
    ///
    /// `Ok(true)` to accept the connection, `Ok(false)` to reject it
    ///
    /// # Note
    ///
    /// Currently accepts all server keys without validation.
    /// TODO: Implement proper validation against ~/.ssh/known_hosts
    async fn check_server_key(&mut self, server_public_key: &PublicKey) -> Result<bool, Self::Error> {
        info!(host = self.host, port = self.port, "check server key");
        let Ok(public_key) = server_public_key.to_openssh() else {
            return Ok(false);
        };
        let Some(home) = get_home_dir() else {
            return Ok(true);
        };
        let known_hosts = home.join(".ssh/known_hosts");
        if known_hosts.exists() {
            let known_hosts = std::fs::read_to_string(known_hosts)?;
            // simply check if the public key is in the known_hosts file
            return Ok(known_hosts.contains(public_key.as_str()));
        }
        Ok(true)
    }
}

type SshHandle = Handle<ClientHandler>;

/// Global cache of SSH sessions keyed by "user@host:port" identifier.
/// This prevents creating duplicate SSH connections to the same server.
static SSH_SESSION: LazyLock<DashMap<String, Arc<SshHandle>>> = LazyLock::new(DashMap::new);

/// Checks if an SSH session is still alive and functional.
///
/// This attempts to open a session channel on the SSH connection.
/// If successful, the channel is immediately closed and the function
/// returns true, indicating the session is active.
///
/// # Arguments
///
/// * `session` - The SSH session handle to check
///
/// # Returns
///
/// `true` if the session is alive, `false` otherwise
async fn is_alive(session: Arc<SshHandle>) -> bool {
    match session.channel_open_session().await {
        Ok(channel) => {
            let _ = channel.close().await;
            true
        }
        Err(_) => false,
    }
}

/// Gets an existing SSH session from the cache or creates a new one.
///
/// This function first attempts to retrieve a cached SSH session for the
/// specified address and user. If found, it validates the session is still
/// alive before returning it. If no valid cached session exists, a new
/// SSH connection is established and cached for future use.
///
/// # Arguments
///
/// * `addr` - SSH server address in "host:port" or "host" format (defaults to port 22)
/// * `user` - SSH username for authentication
/// * `key` - Optional SSH private key (file path or key content)
/// * `password` - Optional password for key decryption or password authentication
///
/// # Returns
///
/// An Arc-wrapped SSH session handle ready for use
pub async fn get_or_init_ssh_session(addr: &str, user: &str, key: &str, password: &str) -> Result<Arc<SshHandle>> {
    // Generate unique identifier for this SSH connection
    let id = format!("{user}@{addr}");
    // Check cache for existing session
    let cached_session = SSH_SESSION.get(&id).map(|entry| entry.value().clone());
    if let Some(session) = cached_session {
        // Validate the cached session is still alive
        if is_alive(session.clone()).await {
            return Ok(session);
        }
    }
    // Create new session if none exists or cached session is dead
    let session = new_ssh_session(addr, user, key, password).await?;
    let session = Arc::new(session);
    // Cache the new session for future reuse
    SSH_SESSION.insert(id, session.clone());
    Ok(session)
}

/// Creates a new SSH session with the specified authentication method.
///
/// This function establishes a new SSH connection to the remote server using
/// either public key authentication or password authentication. It supports
/// SSH keys provided as file paths or direct key content.
///
/// # Arguments
///
/// * `addr` - SSH server address in "host:port" or "host" format (defaults to port 22)
/// * `user` - SSH username for authentication
/// * `key` - Optional SSH private key (file path or PEM/OpenSSH format content)
/// * `password` - Optional password for key decryption or password authentication
///
/// # Returns
///
/// An authenticated SSH session handle
///
/// # Authentication Methods
///
/// 1. Public Key: If `key` is provided, attempts public key authentication
///    - If key is a valid file path, loads the key from disk
///    - Otherwise, decodes the key from the string content
/// 2. Password: If only `password` is provided, uses password authentication
/// 3. Error: If neither key nor password is provided, returns an error
async fn new_ssh_session(addr: &str, user: &str, key: &str, password: &str) -> Result<SshHandle> {
    // Configure SSH client with keepalive to maintain connection
    let config = russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    };
    let config = Arc::new(config);

    // Parse host and port from address string
    let (host, port) = if let Some((host, port)) = addr.split_once(':') {
        let host = host.to_string();
        let port = port.parse::<u16>().unwrap_or(22);
        (host.to_string(), port)
    } else {
        (addr.to_string(), 22)
    };

    let handler = ClientHandler {
        host: host.clone(),
        port,
    };

    // Establish SSH connection
    let mut session = russh::client::connect(config, (host, port), handler).await?;

    // Authenticate using provided credentials
    let auth_res = if !key.is_empty() {
        let key = if key.starts_with("~")
            && let Some(home_dir) = get_home_dir()
        {
            format!("{}{}", home_dir.to_string_lossy(), &key[1..])
        } else {
            key.to_string()
        };
        // Public key authentication
        let key_pair = if Path::new(&key).exists() {
            // Load key from file path
            load_secret_key(key, None)?
        } else {
            // Decode key from string content
            decode_secret_key(&key, None)?
        };
        let key = Arc::new(key_pair);
        let key_with_alg = PrivateKeyWithHashAlg::new(key, None);
        session.authenticate_publickey(user, key_with_alg).await?
    } else if !password.is_empty() {
        // Password authentication
        session.authenticate_password(user, password).await?
    } else {
        // TODO: Add SSH agent support
        return Err(Error::Invalid {
            message: "Ssh authentication failed".to_string(),
        });
    };

    // Verify authentication succeeded
    if !auth_res.success() {
        return Err(Error::Invalid {
            message: format!("Ssh authentication failed, {auth_res:?}"),
        });
    }
    info!(addr, user, "ssh session established");

    Ok(session)
}

/// Opens a Redis connection through an SSH tunnel.
///
/// This function establishes an SSH session using the provided configuration,
/// creates a TCP channel through the SSH tunnel to the Redis server,
/// wraps it in a Redis-compatible stream, and authenticates if credentials are provided.
///
/// # Arguments
///
/// * `config` - Redis server configuration containing SSH and Redis connection details
///
/// # Returns
///
/// A multiplexed Redis connection ready for use
pub async fn open_single_ssh_tunnel_connection(config: &RedisServer) -> Result<MultiplexedConnection> {
    // Extract SSH tunnel configuration
    let ssh_addr = config.ssh_addr.clone().unwrap_or_default();
    let ssh_user = config.ssh_username.clone().unwrap_or_default();
    let ssh_key = config.ssh_key.clone().unwrap_or_default();
    let ssh_password = config.ssh_password.clone().unwrap_or_default();
    // Extract Redis server details
    let host = config.host.to_string();
    let port = config.port;
    let username = config.username.clone();
    let password = config.password.clone();
    run_in_tokio(async move {
        // Get or initialize an SSH session
        let session = get_or_init_ssh_session(&ssh_addr, &ssh_user, &ssh_key, &ssh_password).await?;
        // Open a direct TCP channel through the SSH tunnel to the Redis server
        let channel = session
            .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 0)
            .await?;
        info!(ssh_addr, ssh_user, "open direct tcpip success");
        // Wrap the SSH channel in a Redis-compatible stream
        let compat_stream = SshRedisStream::new(channel.into_stream());
        let info = RedisConnectionInfo::default();
        let conn_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(get_redis_connection_timeout()))
            .set_response_timeout(Some(get_redis_response_timeout()));
        // Create a multiplexed connection with the stream
        let (mut connection, driver) =
            MultiplexedConnection::new_with_config(&info, compat_stream, conn_config).await?;
        // Spawn a background task to drive the connection
        tokio::spawn(async move {
            driver.await;
            info!("Redis driver task finished");
        });
        // Authenticate with Redis if password is provided
        if let Some(password) = password {
            let mut auth_cmd = cmd("AUTH");
            // Use ACL authentication (username + password) if username is provided
            if let Some(user) = username {
                auth_cmd.arg(user);
            }
            auth_cmd.arg(password);
            let _: () = auth_cmd.query_async(&mut connection).await?;
        }

        Ok(connection)
    })
    .await
}
