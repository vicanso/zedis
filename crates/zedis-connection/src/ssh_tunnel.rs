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

use super::async_connection::{resolve_connection_timeout, resolve_response_timeout};
use super::config::RedisServer;
use super::ssh_stream::SshRedisStream;
use crate::error::Error;
use redis::{RedisConnectionInfo, aio::MultiplexedConnection, cmd};
use russh::client::AuthResult;
use russh::client::{Handle, Handler};
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
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};
use zedis_core::fs::{get_home_dir, get_or_create_config_dir, resolve_path};
use zedis_core::ttl_cache::TtlCache;

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

/// An SSH host seen for the first time, put to the user before its key is
/// trusted — see [`set_host_key_approver`].
#[derive(Debug, Clone)]
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    /// Key algorithm as `ssh-keygen -l` names it (`ssh-ed25519`, …).
    pub algorithm: String,
    /// `SHA256:…`, the form `ssh` prints and `ssh-keygen -lf` verifies.
    pub fingerprint: String,
}

pub type HostKeyDecision = Pin<Box<dyn Future<Output = bool> + Send>>;
pub type HostKeyApprover = Arc<dyn Fn(HostKeyPrompt) -> HostKeyDecision + Send + Sync>;

static HOST_KEY_APPROVER: OnceLock<HostKeyApprover> = OnceLock::new();

/// Install the app's confirmation for a host key that no known_hosts file
/// records: it is shown the fingerprint and answers whether to trust it.
/// Without an approver (tests, headless use) the key is trusted on first
/// use, as before. The first installation wins.
pub fn set_host_key_approver(approver: HostKeyApprover) {
    let _ = HOST_KEY_APPROVER.set(approver);
}

impl Handler for ClientHandler {
    type Error = russh::Error;

    /// Verifies the SSH server's public key during connection establishment.
    ///
    /// # Arguments
    ///
    /// * `server_key` - The server's host key, or the certificate carrying it
    ///
    /// # Returns
    ///
    /// `Ok(true)` to accept the connection, `Ok(false)` to reject it
    ///
    /// # Note
    ///
    /// Trust-on-first-use host key verification.
    ///
    /// The presented key is matched against both the user's system
    /// `~/.ssh/known_hosts` (read-only) and Zedis's own known_hosts file in the
    /// config directory. Behaviour:
    /// - host recorded in either file with a matching key → accept;
    /// - host recorded but with a different key → reject (possible MITM);
    /// - host not recorded anywhere → trust it and append the key to *Zedis's*
    ///   known_hosts only (the system file is never modified).
    ///
    /// Both lookups resolve hashed hostnames and the `[host]:port` form.
    ///
    /// russh 0.63 can negotiate host *certificates*, so the callback hands
    /// over either shape. We do not validate a certificate against a CA —
    /// `@cert-authority` lines are skipped when reading known_hosts — so both
    /// go through the same check on the host key the certificate carries.
    /// That is the key this handler already saw before certificates were
    /// negotiable, so the trust model is unchanged: an impostor still has to
    /// present the recorded key.
    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        debug!(host = self.host, port = self.port, "check server key");
        let server_public_key = server_key.public_key();

        let host_port = if self.port == 22 {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        };

        // Verification reads the system file first, then Zedis's own file.
        let app_path = app_known_hosts_path();
        let mut paths = Vec::new();
        if let Some(home) = get_home_dir() {
            paths.push(home.join(".ssh/known_hosts"));
        }
        if let Some(app_path) = &app_path {
            paths.push(app_path.clone());
        }

        let mut host_is_known = false;
        for path in &paths {
            // Robust line scan (poison-proof), then augment with russh's parser
            // which additionally resolves hashed hostnames when the file is clean.
            let (mut keys, host_seen) = host_keys_in_file(path, &host_port);
            if host_seen {
                host_is_known = true;
            }
            if let Ok(extra) = russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, path) {
                if !extra.is_empty() {
                    host_is_known = true;
                }
                for (_, key) in extra {
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            if keys.contains(&server_public_key) {
                return Ok(true);
            }
        }

        if host_is_known {
            // Host is recorded but none of the records match — reject.
            error!(
                host = self.host,
                port = self.port,
                "ssh host key mismatch: known_hosts has a different fingerprint, rejecting"
            );
            return Ok(false);
        }

        // First time this host is seen: the app confirms the fingerprint
        // with the user; without an app to ask, trust on first use.
        if let Some(approver) = HOST_KEY_APPROVER.get() {
            let prompt = HostKeyPrompt {
                host: self.host.clone(),
                port: self.port,
                algorithm: server_public_key.algorithm().to_string(),
                fingerprint: server_public_key.fingerprint(HashAlg::Sha256).to_string(),
            };
            if !approver(prompt).await {
                info!(host = self.host, port = self.port, "ssh host key declined by the user");
                return Ok(false);
            }
        }

        // Remember the key in Zedis's own known_hosts so future connections
        // are verified.
        if let Some(app_path) = app_path {
            match russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &server_public_key, &app_path)
            {
                Ok(()) => debug!(
                    host = self.host,
                    port = self.port,
                    "recorded new ssh host key (trust on first use)"
                ),
                Err(e) => error!(error = %e, host = self.host, "failed to record ssh host key"),
            }
        }
        Ok(true)
    }
}

/// Path to Zedis's own known_hosts file, kept next to the server config so
/// trust-on-first-use records persist without touching `~/.ssh/known_hosts`.
fn app_known_hosts_path() -> Option<std::path::PathBuf> {
    get_or_create_config_dir().ok().map(|dir| dir.join("known_hosts"))
}

/// Scan a known_hosts file line by line for entries matching `host_port`.
///
/// Returns the parseable keys recorded for the host, and whether the hostname
/// appeared on any line at all (`host_seen`) — *including* lines whose key
/// could not be parsed. The caller treats `host_seen == true` as "this host is
/// known", so a corrupt/unparsable entry forces a reject instead of silently
/// failing open to "host unknown → trust".
///
/// Unlike russh's `known_host_keys_path`, a single malformed entry does not
/// discard the whole file: blank lines, comments, `@cert-authority`/`@revoked`
/// markers, and lines whose key fails to parse are skipped individually.
/// Hashed hostnames (`|1|salt|hash`) are not matched here — the caller augments
/// the result with russh's parser, which does resolve them.
fn host_keys_in_file(path: &std::path::Path, host_port: &str) -> (Vec<PublicKey>, bool) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (Vec::new(), false);
    };
    let mut keys = Vec::new();
    let mut host_seen = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(hosts), Some(_kind), Some(key_b64)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let matched = hosts
            .split(',')
            .any(|entry| !entry.starts_with("|1|") && entry == host_port);
        if !matched {
            continue;
        }
        // Hostname matched — the host is known even if the key is unparsable.
        host_seen = true;
        if let Ok(key) = russh::keys::parse_public_key_base64(key_b64) {
            keys.push(key);
        }
    }
    (keys, host_seen)
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

/// Where a tunnel session goes and how it authenticates, once
/// `~/.ssh/config` has filled in what the form left blank.
#[derive(Debug, Clone)]
pub(crate) struct SshTarget {
    /// `host[:port]` as configured — the alias looked up in ssh config and,
    /// with the user, the session-cache key.
    pub addr: String,
    /// The host actually dialed: `HostName` from ssh config, else the alias.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: String,
    pub password: String,
    pub key_passphrase: String,
    /// `key` came from ssh config's `IdentityFile`, not the form.
    pub key_from_config: bool,
    pub jump: Option<Box<SshTarget>>,
    /// What ssh config contributed, as `Option=value` notes — the
    /// diagnostics detail line, so nothing applies invisibly.
    pub from_config: Vec<String>,
}

impl SshTarget {
    /// Session-cache key: `user@addr`, with the jump host appended so a
    /// direct and a jumped session to the same host never share an entry.
    pub fn cache_id(&self) -> String {
        match &self.jump {
            Some(jump) => format!("{}@{} via {}@{}", self.user, self.addr, jump.user, jump.addr),
            None => format!("{}@{}", self.user, self.addr),
        }
    }
}

/// The tunnel endpoint for `config`: the form's values, with `~/.ssh/config`
/// filling in `HostName` / `User` / `Port` / `IdentityFile` / `ProxyJump`
/// where the form is blank — read the way `ssh` itself reads it, and only
/// where it applies.
pub(crate) fn resolve_ssh_target(config: &RedisServer) -> SshTarget {
    resolve_ssh_target_with(config, user_ssh_config().as_deref())
}

/// [`resolve_ssh_target`] against a given ssh config text (`None` = no
/// file), so tests never depend on the machine's own `~/.ssh/config`.
pub(crate) fn resolve_ssh_target_with(config: &RedisServer, ssh_config: Option<&str>) -> SshTarget {
    let addr = config.ssh_addr.clone().unwrap_or_default();
    let mut target = ssh_target_from_addr(
        &addr,
        config.ssh_username.clone().unwrap_or_default(),
        config.ssh_key.clone().unwrap_or_default(),
        config.ssh_password.clone().unwrap_or_default(),
        config.ssh_key_passphrase.clone().unwrap_or_default(),
    );
    let jump_spec = config
        .ssh_jump
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    apply_user_ssh_config(&mut target, ssh_config, jump_spec, true);
    target
}

fn ssh_target_from_addr(addr: &str, user: String, key: String, password: String, key_passphrase: String) -> SshTarget {
    // Port 0 stands for "none given", so ssh config's `Port` can apply.
    let (host, port) = split_host_port_or(addr, 0);
    SshTarget {
        addr: addr.trim().to_string(),
        host: host.to_string(),
        port,
        user,
        key,
        password,
        key_passphrase,
        key_from_config: false,
        jump: None,
        from_config: Vec::new(),
    }
}

/// `[user@]host[:port]` — a ProxyJump spec from the form or ssh config.
/// Credentials default to the target's; ssh config may then supply the
/// jump's own HostName / User / Port / IdentityFile. One hop: a `a,b`
/// chain uses `a`.
fn resolve_jump_target(spec: &str, base: &SshTarget, ssh_config: Option<&str>) -> SshTarget {
    let first = spec.split(',').next().unwrap_or(spec).trim();
    if first != spec.trim() {
        warn!(spec, "ProxyJump chains are not supported; using the first hop only");
    }
    // The jump's user: `user@` in the spec, else its own ssh config block,
    // else the target's user — OpenSSH's order.
    let (user, addr) = match first.rsplit_once('@') {
        Some((user, addr)) => (user.to_string(), addr),
        None => (String::new(), first),
    };
    let mut jump = ssh_target_from_addr(
        addr,
        user,
        base.key.clone(),
        base.password.clone(),
        base.key_passphrase.clone(),
    );
    jump.key_from_config = base.key_from_config;
    apply_user_ssh_config(&mut jump, ssh_config, None, false);
    if jump.user.is_empty() {
        jump.user = base.user.clone();
    }
    jump
}

/// Fill `target`'s blanks from the ssh config block(s) matching its alias.
/// `allow_jump` is false for a jump host itself: no second hop, and no
/// way for two aliases pointing at each other to recurse.
fn apply_user_ssh_config(
    target: &mut SshTarget,
    ssh_config: Option<&str>,
    jump_spec: Option<String>,
    allow_jump: bool,
) {
    let alias = target.host.clone();
    let found = ssh_config
        .map(|text| ssh_config_lookup(text, &alias))
        .unwrap_or_default();
    let mut jump_spec = jump_spec;
    if let Some(name) = found.host_name {
        target.host = strip_ipv6_brackets(&name).to_string();
        target.from_config.push(format!("HostName={name}"));
    }
    if target.port == 0
        && let Some(port) = found.port
    {
        target.port = port;
        target.from_config.push(format!("Port={port}"));
    }
    if target.user.is_empty()
        && let Some(user) = found.user
    {
        target.from_config.push(format!("User={user}"));
        target.user = user;
    }
    if target.key.is_empty()
        && target.password.is_empty()
        && let Some(file) = found.identity_file
    {
        let expanded = expand_identity_file(&file, get_home_dir().as_deref(), &alias);
        target.from_config.push(format!("IdentityFile={expanded}"));
        target.key = expanded;
        target.key_from_config = true;
    }
    if allow_jump
        && jump_spec.is_none()
        && let Some(jump) = found.proxy_jump
    {
        target.from_config.push(format!("ProxyJump={jump}"));
        jump_spec = Some(jump);
    }
    if target.port == 0 {
        target.port = 22;
    }
    if allow_jump && let Some(spec) = jump_spec {
        let jump = resolve_jump_target(&spec, target, ssh_config);
        target.jump = Some(Box::new(jump));
    }
}

/// `~/.ssh/config`, when there is a home directory to find it in (the
/// sandboxed build has none) and the file exists. Read afresh on every
/// resolution, so an edit applies to the next connection. Read-only, like
/// the system known_hosts.
pub(crate) fn user_ssh_config() -> Option<String> {
    let path = get_home_dir()?.join(".ssh").join("config");
    std::fs::read_to_string(path).ok()
}

fn is_pem_format(data: &str) -> bool {
    let data = data.trim();
    data.starts_with("-----BEGIN ") && data.contains("-----END ") && data.ends_with("-----")
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
            let channel = jump_session
                .channel_open_direct_tcpip(&target.host, target.port as u32, "127.0.0.1", 0)
                .await?;
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

async fn authenticate(session: &mut SshHandle, target: &SshTarget) -> Result<()> {
    let user = target.user.as_str();
    let key = target.key.as_str();
    // Also keys the "last successful agent key" memory (see
    // `remember_agent_fingerprint`), matching the session-cache id.
    let cache_id = target.cache_id();

    let auth_res = if !key.is_empty() {
        if let Some(pinned) = try_parse_public_key(key) {
            // The key field holds a *public* key (a `.pub` path or pasted
            // content): OpenSSH `IdentityFile xxx.pub` semantics — ask the
            // agent to sign with exactly that key and try nothing else, so a
            // many-keyed agent can never trip the server's MaxAuthTries.
            debug!(user, "ssh agent authentication (pinned public key)");
            authenticate_via_agent(session, user, &cache_id, Some(pinned)).await?
        } else {
            match load_private_key(key, &target.key_passphrase) {
                Ok(key_pair) => {
                    let key_with_alg = PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);
                    debug!(user, "public key authentication");
                    session.authenticate_publickey(user, key_with_alg).await?
                }
                Err(e) if target.key_from_config => {
                    info!(user, key, error = %e, "IdentityFile from ~/.ssh/config unusable here, trying the agent");
                    authenticate_via_agent(session, user, &cache_id, None).await?
                }
                Err(e) => return Err(e),
            }
        }
    } else if !target.password.is_empty() {
        debug!(user, "password authentication");
        session.authenticate_password(user, target.password.as_str()).await?
    } else {
        debug!(user, "ssh agent authentication");
        authenticate_via_agent(session, user, &cache_id, None).await?
    };

    if !auth_res.success() {
        return Err(Error::Invalid {
            message: format!("Ssh authentication failed, {auth_res:?}"),
        });
    }
    Ok(())
}

/// The private key in `key` — a path, or pasted PEM / OpenSSH content —
/// unlocked with `key_passphrase` when it is encrypted.
fn load_private_key(key: &str, key_passphrase: &str) -> Result<russh::keys::PrivateKey> {
    let passphrase = {
        let trimmed = key_passphrase.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    };
    let decoded = if is_pem_format(key) {
        decode_secret_key(key, passphrase)
    } else {
        load_secret_key(resolve_path(key), passphrase)
    };
    decoded.map_err(|e| match e {
        // Encrypted key, no passphrase configured — point at the field
        // instead of surfacing a bare parse error.
        russh::keys::Error::KeyIsEncrypted => Error::Invalid {
            message: "the SSH key is encrypted — fill in the SSH key passphrase in the server settings".to_string(),
        },
        // A passphrase was supplied but decoding still failed: almost
        // always a wrong passphrase (russh reports it as a parse error).
        e if passphrase.is_some() => Error::Invalid {
            message: format!("could not decrypt the SSH key (wrong passphrase?): {e}"),
        },
        e => e.into(),
    })
}

/// Interpret the configured ssh key value as a *public* key when possible:
/// pasted OpenSSH public-key content (`ssh-ed25519 AAAA… comment`) or a path
/// to a `.pub` file. Anything else (PEM/OpenSSH private key content, private
/// key paths, garbage) returns `None` so the caller keeps the existing
/// private-key flow — existing configs are untouched.
fn try_parse_public_key(key: &str) -> Option<PublicKey> {
    let trimmed = key.trim();
    if let Ok(public_key) = PublicKey::from_openssh(trimmed) {
        return Some(public_key);
    }
    if is_pem_format(trimmed) {
        // Pasted private-key content — not a path, don't touch the fs.
        return None;
    }
    let content = std::fs::read_to_string(resolve_path(trimmed)).ok()?;
    PublicKey::from_openssh(content.trim()).ok()
}

/// The error for a session whose event loop died mid-authentication (the
/// server closed the connection). Points the user at the `.pub` escape hatch
/// since "too many keys in the agent" is the usual trigger. Agent-auth
/// only.
#[cfg(any(unix, windows))]
fn dead_session_error() -> Error {
    Error::Invalid {
        message: "Ssh server closed the connection during agent authentication (usually MaxAuthTries \
                  exceeded because the agent holds many keys). Set the connection's SSH key to the \
                  matching public key file (e.g. ~/.ssh/id_ed25519.pub) so only that key is offered."
            .to_string(),
    }
}

/// Authenticate through the ssh-agent; the private key never leaves the agent.
///
/// With `pinned` (a configured `.pub`), only the matching agent identity is
/// tried — mirroring OpenSSH's `IdentityFile xxx.pub` + agent semantics.
/// Without it every agent identity is tried, but the key that last succeeded
/// for `cache_id` is moved to the front so repeat connections authenticate on
/// the first attempt instead of re-burning the server's MaxAuthTries budget.
async fn authenticate_via_agent(
    session: &mut SshHandle,
    user: &str,
    cache_id: &str,
    pinned: Option<PublicKey>,
) -> Result<AuthResult> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (session, user, cache_id, pinned);
        Err(Error::Invalid {
            message: "Ssh agent is not supported on this platform".to_string(),
        })
    }
    #[cfg(any(unix, windows))]
    {
        let mut agent = connect_agent().await?;
        let identities = agent.request_identities().await.map_err(|e| Error::Invalid {
            message: format!("Failed to request identities from ssh agent: {e:?}"),
        })?;
        let mut candidates: Vec<PublicKey> = identities.iter().map(|key| key.public_key().into_owned()).collect();

        if let Some(target) = &pinned {
            // Comment differs between the `.pub` file and the agent — compare
            // key material only.
            candidates.retain(|key| key.key_data() == target.key_data());
            if candidates.is_empty() {
                return Err(Error::Invalid {
                    message: format!(
                        "Ssh agent has no key matching the configured public key {} (list loaded keys with `ssh-add -l`)",
                        target.fingerprint(HashAlg::Sha256)
                    ),
                });
            }
        } else if let Some(fingerprint) = remembered_agent_fingerprint(cache_id)
            && let Some(pos) = candidates
                .iter()
                .position(|key| key.fingerprint(HashAlg::Sha256).to_string() == fingerprint)
            && pos > 0
        {
            let remembered = candidates.remove(pos);
            candidates.insert(0, remembered);
        }

        if candidates.is_empty() {
            return Err(Error::Invalid {
                message: "Ssh agent holds no identities (add one with `ssh-add`)".to_string(),
            });
        }

        let mut last_failure = None;
        let mut hash_alg = None;
        let mut is_detect_hash_alg = false;
        for public_key in candidates {
            if !is_detect_hash_alg && public_key.algorithm().is_rsa() {
                hash_alg = session.best_supported_rsa_hash().await.unwrap_or(None).flatten();
                is_detect_hash_alg = true;
            }
            match session
                .authenticate_publickey_with(user, public_key.clone(), hash_alg, &mut agent)
                .await
            {
                Ok(AuthResult::Success) => {
                    remember_agent_fingerprint(cache_id, &public_key.fingerprint(HashAlg::Sha256).to_string());
                    return Ok(AuthResult::Success);
                }
                Ok(AuthResult::Failure {
                    remaining_methods,
                    partial_success,
                }) => {
                    // An empty method set is russh's synthetic reply after the
                    // session event loop already died (server disconnected) —
                    // not a real server response. Stop instead of burning the
                    // remaining keys on a dead session.
                    if remaining_methods.is_empty() {
                        return Err(dead_session_error());
                    }
                    last_failure = Some(AuthResult::Failure {
                        remaining_methods,
                        partial_success,
                    });
                }
                // Session event loop unreachable: the server closed the
                // connection (typically MaxAuthTries). Every remaining key
                // would fail the same way — stop with a useful error.
                Err(AgentAuthError::Send(_)) => return Err(dead_session_error()),
                Err(e) => {
                    // Agent-side failure for this key (e.g. it refused to
                    // sign); the session is still alive, keep trying.
                    error!(error = %e, "Error authenticating with agent key");
                }
            }
        }
        last_failure.ok_or_else(|| Error::Invalid {
            message: "Ssh authentication failed".to_string(),
        })
    }
}

/// The local ssh-agent: `SSH_AUTH_SOCK` on unix; on Windows the named pipe
/// OpenSSH's agent service listens on, unless `SSH_AUTH_SOCK` names another
/// pipe (1Password and friends do).
#[cfg(unix)]
async fn connect_agent() -> Result<AgentClient<tokio::net::UnixStream>> {
    AgentClient::connect_env().await.map_err(|e| Error::Invalid {
        message: format!("Failed to connect to ssh agent: {e:?}"),
    })
}

#[cfg(windows)]
async fn connect_agent() -> Result<AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>> {
    const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    let pipe = std::env::var("SSH_AUTH_SOCK")
        .ok()
        .filter(|s| s.starts_with(r"\\.\pipe\"))
        .unwrap_or_else(|| OPENSSH_AGENT_PIPE.to_string());
    AgentClient::connect_named_pipe(&pipe).await.map_err(|e| Error::Invalid {
        message: format!(
            "Failed to connect to the ssh agent pipe {pipe}: {e:?} (is the OpenSSH Authentication Agent service running?)"
        ),
    })
}

/// File remembering which agent key last authenticated each `user@addr`
/// (public SHA256 fingerprints only — no secrets). Lives in the config dir
/// next to Zedis's known_hosts; one `<id> <fingerprint>` pair per line.
#[cfg(any(unix, windows))]
fn agent_key_memory_path() -> Option<std::path::PathBuf> {
    get_or_create_config_dir().ok().map(|dir| dir.join("ssh_agent_keys"))
}

/// SHA256 fingerprint of the agent key that last succeeded for `cache_id`.
#[cfg(any(unix, windows))]
fn remembered_agent_fingerprint(cache_id: &str) -> Option<String> {
    let content = std::fs::read_to_string(agent_key_memory_path()?).ok()?;
    content.lines().find_map(|line| {
        let (id, fingerprint) = line.trim().split_once(' ')?;
        (id == cache_id).then(|| fingerprint.trim().to_string())
    })
}

/// Record `fingerprint` as the working agent key for `cache_id` (upsert).
/// Best-effort: a write failure only costs the fast path next time.
#[cfg(any(unix, windows))]
fn remember_agent_fingerprint(cache_id: &str, fingerprint: &str) {
    if remembered_agent_fingerprint(cache_id).as_deref() == Some(fingerprint) {
        return;
    }
    let Some(path) = agent_key_memory_path() else {
        return;
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && line.split_once(' ').is_none_or(|(id, _)| id != cache_id))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!("{cache_id} {fingerprint}"));
    if let Err(e) = std::fs::write(&path, lines.join("\n") + "\n") {
        error!(error = %e, "failed to record ssh agent key fingerprint");
    }
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
        if let Some(root_cert) = config.root_cert_pem()? {
            let certs: Vec<_> = CertificateDer::pem_slice_iter(&root_cert)
                .filter_map(|r| r.ok())
                .collect();
            root_store.add_parsable_certificates(certs);
        } else {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        rustls::ClientConfig::builder().with_root_certificates(root_store)
    };

    let tls_config = if let Some(client_cert) = config.client_cert_pem()?
        && let Some(client_key) = config.client_key_pem()?
    {
        let certs: Vec<_> = CertificateDer::pem_slice_iter(&client_cert)
            .filter_map(|r| r.ok())
            .collect();
        let key = PrivateKeyDer::from_pem_slice(&client_key).map_err(|e| Error::Invalid {
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

async fn open_single_ssh_tunnel_connection_inner(
    config: &RedisServer,
    push_tx: Option<smol::channel::Sender<redis::PushInfo>>,
) -> Result<MultiplexedConnection> {
    let target = resolve_ssh_target(config);
    let (host, port) = config.primary_endpoint();
    // The certificate names the endpoint's DNS name; through a tunnel the
    // dialed host is often an internal IP, so the user can name it.
    let server_name = tls_server_name(config).unwrap_or_else(|| host.clone());
    let connection_timeout = resolve_connection_timeout(config);
    let response_timeout = resolve_response_timeout(config);
    let username = config.username.clone();
    let password = config.password.clone();
    let tls_connector = if config.tls.unwrap_or(false) {
        Some(build_tls_connector(config)?)
    } else {
        None
    };

    run_in_tokio(async move {
        let session = get_or_init_ssh_session(&target).await?;
        let channel = session
            .handle
            .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 0)
            .await?;
        debug!(ssh = target.cache_id(), host, port, "open direct tcpip success");
        let ssh_stream = SshRedisStream::new(channel.into_stream());
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

        let mut connection = if let Some(tls_connector) = tls_connector {
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
        authenticate_redis(&mut connection, username, password).await?;

        Ok(connection)
    })
    .await
}

/// `AUTH` on a hand-built connection (the URL-based client does this
/// itself). No password means no `AUTH` at all.
async fn authenticate_redis(
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

/// The configured TLS server name, if any.
pub(crate) fn tls_server_name(config: &RedisServer) -> Option<String> {
    config
        .tls_server_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// A direct TLS connection whose certificate check and SNI use
/// `server_name` instead of the dialed host. redis-rs's own connector
/// ties both to the URL host, which fails for an endpoint reached by IP:
/// the certificate carries its DNS name. Same stream plumbing as the
/// tunnel path, over a plain TCP socket.
pub async fn open_single_sni_tls_connection(config: &RedisServer, server_name: &str) -> Result<MultiplexedConnection> {
    let (host, port) = config.primary_endpoint();
    let connection_timeout = resolve_connection_timeout(config);
    let response_timeout = resolve_response_timeout(config);
    let tls_connector = build_tls_connector(config)?;
    let server_name = ServerName::try_from(server_name)
        .map_err(|_| Error::Invalid {
            message: format!("Invalid TLS server name: {server_name}"),
        })?
        .to_owned();
    let username = config.username.clone();
    let password = config.password.clone();

    run_in_tokio(async move {
        let tcp = tokio::time::timeout(
            connection_timeout,
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .map_err(|_| Error::Invalid {
            message: format!("connect to {host}:{port} timed out"),
        })??;
        let tls_stream = tls_connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::Invalid {
                message: format!("TLS handshake failed: {e}"),
            })?;
        let conn_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(connection_timeout))
            .set_response_timeout(Some(response_timeout));
        let (mut connection, driver) =
            MultiplexedConnection::new_with_config(&RedisConnectionInfo::default(), tls_stream, conn_config).await?;
        tokio::spawn(async move {
            driver.await;
            info!("Redis driver task finished");
        });
        authenticate_redis(&mut connection, username, password).await?;
        Ok(connection)
    })
    .await
}

/// Clears expired SSH sessions from the cache.
pub fn clear_expired_ssh_sessions() -> (usize, usize) {
    SSH_SESSION.clear_expired()
}

#[cfg(test)]
mod tests {
    use super::*;

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
