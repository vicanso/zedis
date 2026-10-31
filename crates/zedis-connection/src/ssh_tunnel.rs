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

use super::async_connection::{client_name, resolve_connection_timeout, resolve_response_timeout};
use super::config::RedisServer;
use super::ssh_stream::SshRedisStream;
use crate::error::Error;
use futures::Stream;
use redis::{Cmd, RedisConnectionInfo, Value, aio::MultiplexedConnection, cmd};
use russh::client::AuthResult;
use russh::Channel;
use russh::client::{Handle, Handler, Msg};
use russh::keys::ssh_key::{HashAlg, PublicKey};
use zedis_core::ssh_config::{expand_identity_file, lookup as ssh_config_lookup};
use zedis_core::string::{split_host_port_or, strip_ipv6_brackets};
// Agent auth needs a local agent transport: a unix socket, or the named
// pipe OpenSSH for Windows listens on.
#[cfg(any(unix, windows))]
use russh::AgentAuthError;
#[cfg(any(unix, windows))]
use russh::keys::agent::client::AgentClient;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate, decode_secret_key, load_secret_key};
use rustls::pki_types::ServerName;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::runtime::Runtime;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};
use zedis_core::fs::{get_home_dir, get_or_create_config_dir, resolve_path};
use zedis_core::ttl_cache::TtlCache;

mod auth;
mod host_keys;
mod monitor;
mod target;

pub use host_keys::{HostKeyApprover, HostKeyDecision, HostKeyPrompt, set_host_key_approver};
pub use monitor::open_ssh_tunnel_monitor;
pub(crate) use target::{SshTarget, resolve_ssh_target, resolve_ssh_target_with, user_ssh_config};
use auth::authenticate;
use crate::tls_connection::{build_tls_connector, tls_server_name};
use host_keys::ClientHandler;

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

pub(crate) type SshHandle = Handle<ClientHandler>;

/// An authenticated session to the tunnel host, plus the jump-host session
/// it rides on when `ProxyJump` is in play: dropping the jump handle would
/// close the channel the inner session lives in, so it is kept beside it.
pub struct SshSession {
    pub handle: SshHandle,
    _jump: Option<SshHandle>,
}

/// Global cache of SSH sessions keyed by [`SshTarget::cache_id`] —
/// `user@host:port`, plus the jump host when there is one. This prevents
/// creating duplicate SSH connections to the same server.
static SSH_SESSION: LazyLock<TtlCache<String, Arc<SshSession>>> =
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
async fn is_alive(session: Arc<SshSession>) -> bool {
    match session.handle.channel_open_session().await {
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
/// # Returns
///
/// An Arc-wrapped SSH session ready for use
pub(crate) async fn get_or_init_ssh_session(target: &SshTarget) -> Result<Arc<SshSession>> {
    // Generate unique identifier for this SSH connection
    let id = target.cache_id();
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
    let session = new_ssh_session(target).await?;
    info!(id, "new ssh session established");
    let session = Arc::new(session);
    // Cache the new session for future reuse
    SSH_SESSION.insert(id, session.clone());
    Ok(session)
}

/// Creates a new SSH session for `target` — through its jump host first
/// when one is configured — and authenticates it.
///
/// # Authentication Methods
///
/// 1. Public Key: If `key` is set, attempts public key authentication
///    - a `.pub` (path or content) pins one agent identity
///    - otherwise the private key is loaded from the path or decoded from
///      the content, unlocked with `key_passphrase` when encrypted
///    - a key that `~/.ssh/config` supplied and that cannot be used here
///      (encrypted without a passphrase, unreadable) falls back to the
///      agent instead of failing — the agent served it before the file
///      was ever read
/// 2. Password: If only `password` is set, uses password authentication
/// 3. Agent: with neither, every agent identity is tried
pub(crate) async fn new_ssh_session(target: &SshTarget) -> Result<SshSession> {
    // Keepalive every 30s: bastions and NAT gateways commonly drop idle TCP
    // sessions after 60–120s, and a dead session is only noticed (and
    // rebuilt by `get_or_init_ssh_session`) on the next use — the old
    // 5-minute interval meant the first click after a pause always failed.
    let config = Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });
    let handler = ClientHandler {
        host: target.host.clone(),
        port: target.port,
    };

    let (mut session, jump) = match &target.jump {
        None => {
            let session = russh::client::connect(config, (target.host.as_str(), target.port), handler)
                .await
                .map_err(host_key_error)?;
            (session, None)
        }
        Some(jump) => {
            // The jump host is a full session of its own; the target's SSH
            // handshake then runs inside a direct-tcpip channel of it —
            // OpenSSH's ProxyJump, one hop.
            let jump_handler = ClientHandler {
                host: jump.host.clone(),
                port: jump.port,
            };
            let mut jump_session =
                russh::client::connect(config.clone(), (jump.host.as_str(), jump.port), jump_handler)
                    .await
                    .map_err(host_key_error)?;
            authenticate(&mut jump_session, jump).await?;
            let channel = open_forward_channel(&jump_session, &jump.cache_id(), &target.host, target.port).await?;
            debug!(
                jump = jump.addr,
                host = target.host,
                port = target.port,
                "jump host channel open"
            );
            let session = russh::client::connect_stream(config, channel.into_stream(), handler)
                .await
                .map_err(host_key_error)?;
            (session, Some(jump_session))
        }
    };
    authenticate(&mut session, target).await?;
    Ok(SshSession {
        handle: session,
        _jump: jump,
    })
}

/// A `direct-tcpip` channel from `handle`'s server to `host:port`. A refusal
/// comes back as [`Error::SshForward`], which names both ends: the address
/// is resolved and dialed *by the SSH server*, so "which address, from
/// where" is the whole diagnosis.
pub(crate) async fn open_forward_channel(
    handle: &SshHandle,
    via: &str,
    host: &str,
    port: u16,
) -> Result<Channel<Msg>> {
    handle
        .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
        .await
        .map_err(|source| Error::SshForward {
            route: format!("{via} could not open a connection to {host}:{port}").into(),
            source,
        })
}

/// russh reports a host key the handler refused — a known_hosts mismatch,
/// or a declined fingerprint prompt — as `UnknownKey`; say so in the
/// user's terms.
fn host_key_error(e: russh::Error) -> Error {
    match e {
        russh::Error::UnknownKey => Error::Invalid {
            message: "the SSH host key was not accepted: it differs from the one in known_hosts, or the \
                      fingerprint prompt was declined"
                .to_string(),
        },
        e => e.into(),
    }
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
    open_single_ssh_tunnel_connection_inner(config, None).await
}

/// RESP3 variant of [`open_single_ssh_tunnel_connection`] for sharded
/// Pub/Sub: server pushes (`ssubscribe` acks, `smessage` payloads) are
/// forwarded onto `push_tx` instead of being dropped.
pub async fn open_single_ssh_tunnel_push_connection(
    config: &RedisServer,
    push_tx: smol::channel::Sender<redis::PushInfo>,
) -> Result<MultiplexedConnection> {
    open_single_ssh_tunnel_connection_inner(config, Some(push_tx)).await
}

/// A forwarded stream, with TLS on it when the entry asks for TLS.
pub(crate) trait TunnelIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TunnelIo for T {}

/// What reaching an entry's endpoint through its tunnel takes, read from the
/// entry before the work moves onto the tokio runtime.
struct TunnelDial {
    target: SshTarget,
    host: String,
    port: u16,
    /// The certificate names the endpoint's DNS name; through a tunnel the
    /// dialed host is often an internal IP, so the user can name it.
    server_name: String,
    connection_timeout: Duration,
    tls_connector: Option<TlsConnector>,
}

impl TunnelDial {
    fn new(config: &RedisServer) -> Result<Self> {
        let (host, port) = config.primary_endpoint();
        Ok(Self {
            target: resolve_ssh_target(config),
            server_name: tls_server_name(config).unwrap_or_else(|| host.clone()),
            host,
            port,
            connection_timeout: resolve_connection_timeout(config),
            tls_connector: if config.tls.unwrap_or(false) {
                Some(build_tls_connector(config)?)
            } else {
                None
            },
        })
    }

    /// The forwarded stream to the endpoint, TLS already negotiated. Runs on
    /// the tokio runtime, like everything russh does.
    async fn open(self) -> Result<Box<dyn TunnelIo>> {
        let Self {
            target,
            host,
            port,
            server_name,
            connection_timeout,
            tls_connector,
        } = self;
        // The connection timeout covers the tunnel too: russh puts no limit
        // on the TCP dial, the handshake or the channel open, so a bastion
        // that accepts and then stalls held a connect (and a Test click)
        // for as long as it liked.
        let channel = tokio::time::timeout(connection_timeout, async {
            let session = get_or_init_ssh_session(&target).await?;
            open_forward_channel(&session.handle, &target.cache_id(), &host, port).await
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "the SSH tunnel through {} did not open within {}s",
                    target.cache_id(),
                    connection_timeout.as_secs()
                ),
            )
        })??;
        debug!(ssh = target.cache_id(), host, port, "open direct tcpip success");
        let ssh_stream = SshRedisStream::new(channel.into_stream());
        let Some(tls_connector) = tls_connector else {
            return Ok(Box::new(ssh_stream));
        };
        let server_name = ServerName::try_from(server_name.as_str())
            .map_err(|_| Error::Invalid {
                message: format!("Invalid TLS server name: {server_name}"),
            })?
            .to_owned();
        let tls_stream = tls_connector
            .connect(server_name, ssh_stream)
            .await
            .map_err(|e| Error::Invalid {
                message: format!("TLS handshake over SSH tunnel failed: {e}"),
            })?;
        debug!("TLS handshake over SSH tunnel succeeded");
        Ok(Box::new(tls_stream))
    }
}

/// The entry's endpoint through its SSH tunnel as a plain byte stream, TLS
/// negotiated when the entry asks for it — for a connection that reads the
/// server's replies itself (`raw_connection.rs`). Must run on the tokio
/// runtime, like everything russh does.
pub(crate) async fn open_tunnel_stream(config: &RedisServer) -> Result<Box<dyn TunnelIo>> {
    TunnelDial::new(config)?.open().await
}

async fn open_single_ssh_tunnel_connection_inner(
    config: &RedisServer,
    push_tx: Option<smol::channel::Sender<redis::PushInfo>>,
) -> Result<MultiplexedConnection> {
    let dial = TunnelDial::new(config)?;
    let connection_timeout = dial.connection_timeout;
    let response_timeout = resolve_response_timeout(config);
    let username = config.username.clone();
    let password = config.password.clone();

    run_in_tokio(async move {
        let stream = dial.open().await?;
        let mut info = RedisConnectionInfo::default();
        let mut conn_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(connection_timeout))
            .set_response_timeout(Some(response_timeout));
        if let Some(push_tx) = push_tx {
            // Server pushes only exist on RESP3; the closure satisfies
            // `AsyncPushSender` via redis's blanket `Fn` impl.
            info = info.set_protocol(redis::ProtocolVersion::RESP3);
            conn_config = conn_config.set_push_sender(move |push_info| push_tx.try_send(push_info));
        }
        let (mut connection, driver) = MultiplexedConnection::new_with_config(&info, stream, conn_config).await?;
        tokio::spawn(async move {
            driver.await;
            info!("Redis driver task finished");
        });
        authenticate_redis(&mut connection, username, password).await?;

        Ok(connection)
    })
    .await
}

/// `AUTH` on a hand-built connection (the URL-based client does this
/// itself). No password means no `AUTH` at all.
pub(crate) async fn authenticate_redis(
    connection: &mut MultiplexedConnection,
    username: Option<String>,
    password: Option<String>,
) -> Result<()> {
    if let Some(password) = password {
        let mut auth_cmd = cmd("AUTH");
        if let Some(user) = username {
            auth_cmd.arg(user);
        }
        auth_cmd.arg(password);
        let _: () = auth_cmd.query_async(connection).await?;
    }
    Ok(())
}

/// Clears expired SSH sessions from the cache.
pub fn clear_expired_ssh_sessions() -> (usize, usize) {
    SSH_SESSION.clear_expired()
}

#[cfg(test)]
mod tests {
    use super::auth::try_parse_public_key;
    use super::monitor::{forward_monitor_lines, monitor_line, start_monitor};
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn a_monitor_line_is_the_simple_string_without_its_framing() {
        assert_eq!(
            monitor_line(b"+1700000000.1 [0 127.0.0.1:1] \"GET\" \"k\"\r\n").as_deref(),
            Some("1700000000.1 [0 127.0.0.1:1] \"GET\" \"k\"")
        );
        assert_eq!(monitor_line(b"+OK\n").as_deref(), Some("OK"));
        assert_eq!(monitor_line(b"-ERR killed\r\n"), None);
        assert_eq!(monitor_line(b"$3\r\n"), None);
    }

    fn handshake() -> Vec<(Cmd, bool)> {
        let mut auth = cmd("AUTH");
        auth.arg("secret");
        let mut setname = cmd("CLIENT");
        setname.arg("SETNAME").arg("zedis");
        vec![(auth, true), (setname, false), (cmd("MONITOR"), true)]
    }

    /// The whole exchange over an in-memory stream in place of the tunnel:
    /// one write for the handshake, a refused courtesy that does not matter,
    /// then one line per command until the error a killed client is sent.
    #[tokio::test]
    async fn a_tunnelled_monitor_starts_then_hands_over_one_line_per_command() {
        let (ours, mut server) = tokio::io::duplex(4096);
        let mut stream = BufReader::new(Box::new(ours) as Box<dyn TunnelIo>);
        server
            .write_all(
                b"+OK\r\n-NOPERM no names here\r\n+OK\r\n\
                  +1.5 [0 10.0.0.1:5000] \"GET\" \"k\"\r\n\
                  +1.6 [0 10.0.0.1:5000] \"PING\"\r\n\
                  -ERR killed\r\n\
                  +never read\r\n",
            )
            .await
            .expect("write the replies");
        start_monitor(&mut stream, &handshake()).await.expect("the handshake");

        let (tx, lines) = smol::channel::bounded(8);
        forward_monitor_lines(stream, tx).await;
        let lines: Vec<String> = std::iter::from_fn(|| lines.try_recv().ok()).collect();
        assert_eq!(
            lines,
            ["1.5 [0 10.0.0.1:5000] \"GET\" \"k\"", "1.6 [0 10.0.0.1:5000] \"PING\""]
        );

        let mut sent = vec![0; 256];
        let n = server.read(&mut sent).await.expect("read what was sent");
        let sent = String::from_utf8_lossy(&sent[..n]);
        let order = ["AUTH", "SETNAME", "MONITOR"].map(|word| sent.find(word));
        assert!(
            order.iter().all(Option::is_some) && order.is_sorted(),
            "the handshake went out as {sent:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_login_or_monitor_is_an_error() {
        for replies in [
            b"-WRONGPASS invalid password\r\n+OK\r\n+OK\r\n".as_slice(),
            b"+OK\r\n+OK\r\n-NOPERM this user has no permissions to run the 'monitor' command\r\n",
        ] {
            let (ours, mut server) = tokio::io::duplex(4096);
            let mut stream = BufReader::new(Box::new(ours) as Box<dyn TunnelIo>);
            server.write_all(replies).await.expect("write the replies");
            assert!(start_monitor(&mut stream, &handshake()).await.is_err());
        }
        // A server that hangs up mid-handshake.
        let (ours, server) = tokio::io::duplex(4096);
        drop(server);
        let mut stream = BufReader::new(Box::new(ours) as Box<dyn TunnelIo>);
        assert!(start_monitor(&mut stream, &handshake()).await.is_err());
    }

    // Throwaway key generated for this test — never used anywhere.
    const SAMPLE_PUB: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII32jQOkordvaQmkre2sGOqkzt4jSxZbSS5/axMDPpQK zedis-test";

    #[test]
    fn parse_public_key_accepts_pasted_content() {
        let parsed = try_parse_public_key(SAMPLE_PUB).expect("valid public key content");
        assert_eq!(parsed.comment().to_string(), "zedis-test");
        // Surrounding whitespace must not matter.
        assert!(try_parse_public_key(&format!("  {SAMPLE_PUB}\n")).is_some());
    }

    #[test]
    fn parse_public_key_accepts_pub_file_path() {
        let dir = std::env::temp_dir().join("zedis-ssh-tunnel-test");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("testkey.pub");
        std::fs::write(&path, format!("{SAMPLE_PUB}\n")).expect("write pub file");
        let parsed = try_parse_public_key(&path.to_string_lossy()).expect("valid .pub path");
        assert!(parsed.algorithm().to_string().contains("ed25519"));
    }

    #[test]
    fn parse_public_key_rejects_private_material() {
        // Pasted PEM private key must fall through to the private-key flow.
        assert!(
            try_parse_public_key("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----")
                .is_none()
        );
        // Nonexistent path / garbage content.
        assert!(try_parse_public_key("~/.ssh/definitely-missing-key").is_none());
        assert!(try_parse_public_key("not a key at all").is_none());
    }

    const SSH_CONFIG: &str = r#"
Host prod
    HostName 10.0.0.5
    User ops
    Port 2222
    IdentityFile /keys/prod
    ProxyJump bastion

Host bastion
    HostName bastion.example.com
    User jump

Host *
    User fallback
    IdentityFile /keys/default
"#;

    fn tunnel(addr: &str) -> RedisServer {
        RedisServer {
            ssh_tunnel: Some(true),
            ssh_addr: Some(addr.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn ssh_config_fills_only_the_blanks() {
        // Everything blank: the alias resolves to HostName, and the block's
        // user, port, key and jump apply — the jump's own block too.
        let target = resolve_ssh_target_with(&tunnel("prod"), Some(SSH_CONFIG));
        assert_eq!(
            (target.host.as_str(), target.port, target.user.as_str()),
            ("10.0.0.5", 2222, "ops")
        );
        assert_eq!(target.key, "/keys/prod");
        assert!(target.key_from_config);
        let jump = target.jump.as_deref().expect("ProxyJump from config");
        assert_eq!(
            (jump.host.as_str(), jump.port, jump.user.as_str()),
            ("bastion.example.com", 22, "jump")
        );
        assert!(jump.jump.is_none(), "one hop only");
        assert_eq!(target.cache_id(), "ops@prod via jump@bastion");
        assert!(target.from_config.iter().any(|n| n == "ProxyJump=bastion"));

        // The form wins wherever it says something.
        let mut server = tunnel("prod:22");
        server.ssh_username = Some("me".into());
        server.ssh_password = Some("pw".into());
        server.ssh_jump = Some("relay@relay.example.com:2200".into());
        let target = resolve_ssh_target_with(&server, Some(SSH_CONFIG));
        assert_eq!(
            (target.host.as_str(), target.port, target.user.as_str()),
            ("10.0.0.5", 22, "me")
        );
        assert!(
            target.key.is_empty() && !target.key_from_config,
            "a password rules out IdentityFile"
        );
        let jump = target.jump.as_deref().expect("ProxyJump from the form");
        assert_eq!(
            (jump.host.as_str(), jump.port, jump.user.as_str()),
            ("relay.example.com", 2200, "relay")
        );
        assert_eq!(jump.password, "pw", "the jump host takes the target's credentials");
    }

    #[test]
    fn ssh_config_absent_leaves_the_form_alone() {
        let mut server = tunnel("[2001:db8::1]:2200");
        server.ssh_username = Some("root".into());
        let target = resolve_ssh_target_with(&server, None);
        assert_eq!(
            (target.host.as_str(), target.port, target.user.as_str()),
            ("2001:db8::1", 2200, "root")
        );
        assert!(target.jump.is_none() && target.from_config.is_empty());
        assert_eq!(target.cache_id(), "root@[2001:db8::1]:2200");
        // An alias no block names still gets `Host *`.
        let target = resolve_ssh_target_with(&tunnel("db.internal"), Some(SSH_CONFIG));
        assert_eq!(target.user, "fallback");
        assert_eq!(target.key, "/keys/default");
        assert_eq!(target.port, 22);
    }
}
