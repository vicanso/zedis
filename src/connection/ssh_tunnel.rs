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
use crate::helpers::{TtlCache, get_home_dir, resolve_path};
use redis::{RedisConnectionInfo, aio::MultiplexedConnection, cmd};
use russh::client::AuthResult;
use russh::client::{Handle, Handler};
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::PublicKey;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key, load_secret_key};
use rustls::pki_types::ServerName;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info};

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
        debug!(host = self.host, port = self.port, "check server key");
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
static SSH_SESSION: LazyLock<TtlCache<String, Arc<SshHandle>>> =
    LazyLock::new(|| TtlCache::new(Duration::from_secs(5 * 60)));

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
    let cached_session = SSH_SESSION.get(&id);
    if let Some(session) = cached_session {
        // Validate the cached session is still alive
        if is_alive(session.clone()).await {
            debug!(id, "get ssh session from cache");
            return Ok(session);
        }
    }
    debug!(id, "start to create new ssh session");
    // Create new session if none exists or cached session is dead
    let session = new_ssh_session(addr, user, key, password).await?;
    info!(id, "new ssh session established");
    let session = Arc::new(session);
    // Cache the new session for future reuse
    SSH_SESSION.insert(id, session.clone());
    Ok(session)
}

fn is_pem_format(data: &str) -> bool {
    let data = data.trim();
    data.starts_with("-----BEGIN ") && data.contains("-----END ") && data.ends_with("-----")
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
        keepalive_interval: Some(Duration::from_secs(5 * 60)),
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
        let key_pair = if is_pem_format(key) {
            // Decode key from string content
            decode_secret_key(key, None)?
        } else {
            let key = resolve_path(key);
            // Load key from file path
            load_secret_key(key, None)?
        };
        let key = Arc::new(key_pair);
        let key_with_alg = PrivateKeyWithHashAlg::new(key, None);
        debug!(user, "public key authentication");
        session.authenticate_publickey(user, key_with_alg).await?
    } else if !password.is_empty() {
        debug!(user, "password authentication");
        // Password authentication
        session.authenticate_password(user, password).await?
    } else {
        #[cfg(not(unix))]
        {
            return Err(Error::Invalid {
                message: "Ssh agent is not supported on this platform".to_string(),
            });
        }
        #[cfg(unix)]
        {
            debug!(user, "ssh agent authentication");
            let mut agent = AgentClient::connect_env().await.map_err(|e| Error::Invalid {
                message: format!("Failed to connect to ssh agent: {e:?}"),
            })?;
            let identities = agent.request_identities().await.map_err(|e| Error::Invalid {
                message: format!("Failed to request identities from ssh agent: {e:?}"),
            })?;
            let mut authenticated = false;
            let mut auth_result = None;
            let mut hash_alg = None;
            let mut is_detect_hash_alg = false;
            for key in identities {
                if !is_detect_hash_alg && key.algorithm().is_rsa() {
                    hash_alg = if key.algorithm().is_rsa() {
                        session.best_supported_rsa_hash().await.unwrap_or(None).flatten()
                    } else {
                        None
                    };
                    is_detect_hash_alg = true;
                }
                match session
                    .authenticate_publickey_with(user, key, hash_alg, &mut agent)
                    .await
                {
                    Ok(AuthResult::Success) => {
                        authenticated = true;
                        break;
                    }
                    Ok(AuthResult::Failure {
                        remaining_methods,
                        partial_success,
                    }) => {
                        auth_result = Some(AuthResult::Failure {
                            remaining_methods,
                            partial_success,
                        });
                        continue;
                    }
                    Err(e) => {
                        error!(error = %e, "Error authenticating with agent key");
                        continue;
                    }
                }
            }
            if authenticated {
                AuthResult::Success
            } else if let Some(auth_result) = auth_result {
                auth_result
            } else {
                return Err(Error::Invalid {
                    message: "Ssh authentication failed".to_string(),
                });
            }
        }
    };

    // Verify authentication succeeded
    if !auth_res.success() {
        return Err(Error::Invalid {
            message: format!("Ssh authentication failed, {auth_res:?}"),
        });
    }

    Ok(session)
}

/// A rustls `ServerCertVerifier` that accepts any server certificate.
/// Used when the user enables "insecure" / skip-verification mode.
#[derive(Debug)]
struct InsecureServerCertVerifier;

impl rustls::client::danger::ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Builds a `TlsConnector` from the server's TLS configuration.
///
/// Handles insecure mode (skip verification), custom root CA, and
/// optional mTLS (client certificate + key).
fn build_tls_connector(config: &RedisServer) -> Result<TlsConnector> {
    let insecure = config.insecure.unwrap_or(false);

    let builder = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier))
    } else {
        let mut root_store = rustls::RootCertStore::empty();
        if let Some(root_cert) = &config.root_cert
            && !root_cert.is_empty()
        {
            let certs: Vec<_> = CertificateDer::pem_slice_iter(root_cert.as_bytes())
                .filter_map(|r| r.ok())
                .collect();
            root_store.add_parsable_certificates(certs);
        } else {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        rustls::ClientConfig::builder().with_root_certificates(root_store)
    };

    let tls_config = if let Some(client_cert) = &config.client_cert
        && let Some(client_key) = &config.client_key
        && !client_cert.is_empty()
        && !client_key.is_empty()
    {
        let certs: Vec<_> = CertificateDer::pem_slice_iter(client_cert.as_bytes())
            .filter_map(|r| r.ok())
            .collect();
        let key = PrivateKeyDer::from_pem_slice(client_key.as_bytes()).map_err(|e| Error::Invalid {
            message: format!("Failed to parse client key: {e}"),
        })?;
        builder.with_client_auth_cert(certs, key).map_err(|e| Error::Invalid {
            message: format!("TLS client auth config failed: {e}"),
        })?
    } else {
        builder.with_no_client_auth()
    };

    Ok(TlsConnector::from(Arc::new(tls_config)))
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
    let ssh_addr = config.ssh_addr.clone().unwrap_or_default();
    let ssh_user = config.ssh_username.clone().unwrap_or_default();
    let ssh_key = config.ssh_key.clone().unwrap_or_default();
    let ssh_password = config.ssh_password.clone().unwrap_or_default();
    let host = config.host.to_string();
    let port = config.port;
    let username = config.username.clone();
    let password = config.password.clone();
    let tls_connector = if config.tls.unwrap_or(false) {
        Some(build_tls_connector(config)?)
    } else {
        None
    };

    run_in_tokio(async move {
        let session = get_or_init_ssh_session(&ssh_addr, &ssh_user, &ssh_key, &ssh_password).await?;
        let channel = session
            .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 0)
            .await?;
        debug!(ssh_addr, ssh_user, host, port, "open direct tcpip success");
        let ssh_stream = SshRedisStream::new(channel.into_stream());
        let info = RedisConnectionInfo::default();
        let conn_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(get_redis_connection_timeout()))
            .set_response_timeout(Some(get_redis_response_timeout()));

        let mut connection = if let Some(tls_connector) = tls_connector {
            let server_name = ServerName::try_from(host.as_str())
                .map_err(|_| Error::Invalid {
                    message: format!("Invalid TLS server name: {host}"),
                })?
                .to_owned();
            let tls_stream = tls_connector.connect(server_name, ssh_stream).await.map_err(|e| Error::Invalid {
                message: format!("TLS handshake over SSH tunnel failed: {e}"),
            })?;
            debug!("TLS handshake over SSH tunnel succeeded");
            let (conn, driver) = MultiplexedConnection::new_with_config(&info, tls_stream, conn_config).await?;
            tokio::spawn(async move {
                driver.await;
                info!("Redis driver task finished");
            });
            conn
        } else {
            let (conn, driver) = MultiplexedConnection::new_with_config(&info, ssh_stream, conn_config).await?;
            tokio::spawn(async move {
                driver.await;
                info!("Redis driver task finished");
            });
            conn
        };
        if let Some(password) = password {
            let mut auth_cmd = cmd("AUTH");
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

/// Clears expired SSH sessions from the cache.
pub fn clear_expired_ssh_sessions() -> (usize, usize) {
    SSH_SESSION.clear_expired()
}
