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

//! Staged connection diagnostics.
//!
//! Splits "connect to this server" into observable stages (DNS → TCP →
//! SSH auth → SSH tunnel → TLS → AUTH → PING) so a failure points at the
//! exact layer instead of surfacing one opaque driver error. The redis
//! crate performs TLS/AUTH internally in a single connect call, so those
//! stages are attributed by classifying the error of one real connection
//! attempt rather than probed separately.

use super::async_connection::{open_single_connection, resolve_connection_timeout};
use super::config::{RedisServer, SERVER_TYPE_SENTINEL};
use super::ssh_tunnel::{
    SshSession, new_ssh_session, resolve_ssh_target, resolve_ssh_target_with, run_in_tokio, user_ssh_config,
};
use crate::error::Error;
use redis::{ErrorKind, cmd};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

/// One diagnostic stage. The set of stages for a server depends on its
/// config — see [`diag_stages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagStage {
    Dns,
    Tcp,
    SshAuth,
    SshTunnel,
    Tls,
    Auth,
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagStatus {
    Success,
    Failed,
    Skipped,
}

/// A remediation hint attached to an outcome; the view maps each variant
/// to a localized message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagHint {
    Dns,
    TcpRefused,
    TcpUnreachable,
    SshAuth,
    SshTunnel,
    Tls,
    AuthRequired,
    AuthRejected,
    AuthNotConfigured,
    Redis,
}

#[derive(Debug, Clone)]
pub struct DiagOutcome {
    pub status: DiagStatus,
    /// Extra context on success/skip: resolved IPs, connected address, …
    pub detail: Option<String>,
    /// The raw error text on failure.
    pub error: Option<String>,
    pub hint: Option<DiagHint>,
    pub elapsed: Duration,
}

impl DiagOutcome {
    pub fn success(detail: Option<String>, elapsed: Duration) -> Self {
        Self {
            status: DiagStatus::Success,
            detail,
            error: None,
            hint: None,
            elapsed,
        }
    }
    pub fn skipped(detail: Option<String>, hint: Option<DiagHint>) -> Self {
        Self {
            status: DiagStatus::Skipped,
            detail,
            error: None,
            hint,
            elapsed: Duration::ZERO,
        }
    }
    pub fn failed(error: String, hint: DiagHint, elapsed: Duration) -> Self {
        Self {
            status: DiagStatus::Failed,
            detail: None,
            error: Some(error),
            hint: Some(hint),
            elapsed,
        }
    }
}

/// The ordered stage list for this server's configuration.
pub fn diag_stages(server: &RedisServer) -> Vec<DiagStage> {
    // A Unix socket has no name to resolve and no port to reach.
    let mut stages = if server.is_unix_socket() {
        Vec::new()
    } else {
        vec![DiagStage::Dns, DiagStage::Tcp]
    };
    if server.is_ssh_tunnel() {
        stages.push(DiagStage::SshAuth);
        stages.push(DiagStage::SshTunnel);
    }
    if server.tls.unwrap_or(false) {
        stages.push(DiagStage::Tls);
    }
    stages.push(DiagStage::Auth);
    stages.push(DiagStage::Ping);
    stages
}

/// The endpoint we actually dial from this machine: the jump host, else
/// the SSH server, when a tunnel is configured (the Redis host is then
/// resolved remotely by the SSH server), otherwise the Redis host itself —
/// the first seed of a multi-address Sentinel entry.
pub fn dial_endpoint(server: &RedisServer) -> (String, u16) {
    dial_endpoint_with(server, user_ssh_config().as_deref())
}

/// [`dial_endpoint`] against a given ssh config text (`None` = no file).
pub(crate) fn dial_endpoint_with(server: &RedisServer, ssh_config: Option<&str>) -> (String, u16) {
    if server.is_ssh_tunnel() {
        let target = resolve_ssh_target_with(server, ssh_config);
        let hop = target.jump.as_deref().unwrap_or(&target);
        return (hop.host.clone(), hop.port);
    }
    server.primary_endpoint()
}

async fn with_timeout<T>(fut: impl Future<Output = T>, timeout: Duration) -> Option<T> {
    let work = async { Some(fut.await) };
    let timer = async {
        smol::Timer::after(timeout).await;
        None
    };
    smol::future::or(work, timer).await
}

/// Resolve `host` to socket addresses. Skipped (with the parsed address
/// carried through) when `host` is already an IP literal.
pub async fn probe_dns(host: &str, port: u16, timeout: Duration) -> (DiagOutcome, Vec<SocketAddr>) {
    if let Ok(ip) = host.parse::<IpAddr>() {
        let addr = SocketAddr::new(ip, port);
        return (DiagOutcome::skipped(Some(host.to_string()), None), vec![addr]);
    }
    let start = Instant::now();
    let owned_host = host.to_string();
    let resolved = with_timeout(
        smol::unblock(move || {
            (owned_host.as_str(), port)
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<_>>())
        }),
        timeout,
    )
    .await;
    let elapsed = start.elapsed();
    match resolved {
        Some(Ok(addrs)) if !addrs.is_empty() => {
            let mut ips: Vec<String> = addrs.iter().take(3).map(|a| a.ip().to_string()).collect();
            if addrs.len() > 3 {
                ips.push(format!("+{}", addrs.len() - 3));
            }
            (DiagOutcome::success(Some(ips.join(", ")), elapsed), addrs)
        }
        Some(Ok(_)) => (
            DiagOutcome::failed("no address records returned".to_string(), DiagHint::Dns, elapsed),
            vec![],
        ),
        Some(Err(e)) => (DiagOutcome::failed(e.to_string(), DiagHint::Dns, elapsed), vec![]),
        None => (
            DiagOutcome::failed("DNS lookup timed out".to_string(), DiagHint::Dns, elapsed),
            vec![],
        ),
    }
}

/// Try a plain TCP connect to each resolved address until one succeeds.
pub async fn probe_tcp(addrs: &[SocketAddr], timeout: Duration) -> DiagOutcome {
    let start = Instant::now();
    let mut last_error: Option<std::io::Error> = None;
    for addr in addrs {
        match with_timeout(smol::net::TcpStream::connect(*addr), timeout).await {
            Some(Ok(_stream)) => {
                return DiagOutcome::success(Some(addr.to_string()), start.elapsed());
            }
            Some(Err(e)) => last_error = Some(e),
            None => {
                return DiagOutcome::failed(
                    format!("connect to {addr} timed out"),
                    DiagHint::TcpUnreachable,
                    start.elapsed(),
                );
            }
        }
    }
    let elapsed = start.elapsed();
    match last_error {
        Some(e) => {
            let hint = if e.kind() == std::io::ErrorKind::ConnectionRefused {
                DiagHint::TcpRefused
            } else {
                DiagHint::TcpUnreachable
            };
            DiagOutcome::failed(e.to_string(), hint, elapsed)
        }
        None => DiagOutcome::failed(
            "no address to connect to".to_string(),
            DiagHint::TcpUnreachable,
            elapsed,
        ),
    }
}

/// Establish a fresh (uncached) SSH session with the configured
/// credentials. A fresh session is deliberate: the shared session cache is
/// keyed only by `user@addr`, so a live session created with *old*
/// credentials would mask an auth problem in the values being edited.
///
/// The success detail lists what `~/.ssh/config` contributed (and the
/// jump host), so a value the form never showed is visible here.
pub async fn probe_ssh_auth(server: &RedisServer) -> (DiagOutcome, Option<SshSession>) {
    let target = resolve_ssh_target(server);
    let mut notes = target.from_config.clone();
    if let Some(jump) = &target.jump {
        notes.push(format!("via {}@{}:{}", jump.user, jump.host, jump.port));
        notes.extend(jump.from_config.iter().map(|n| format!("jump {n}")));
    }
    let detail = (!notes.is_empty()).then(|| format!("~/.ssh/config: {}", notes.join(", ")));
    let start = Instant::now();
    let result = run_in_tokio(async move { new_ssh_session(&target).await }).await;
    let elapsed = start.elapsed();
    match result {
        Ok(session) => (DiagOutcome::success(detail, elapsed), Some(session)),
        Err(e) => (DiagOutcome::failed(e.to_string(), DiagHint::SshAuth, elapsed), None),
    }
}

/// Ask the SSH server to open a direct-tcpip channel to the Redis
/// host:port — this verifies the Redis endpoint is reachable *from the
/// SSH server*, which is the leg a local TCP probe cannot see.
pub async fn probe_ssh_tunnel(session: SshSession, server: &RedisServer) -> DiagOutcome {
    let (host, port) = server.primary_endpoint();
    let start = Instant::now();
    let result = run_in_tokio(async move {
        let channel = session
            .handle
            .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 0)
            .await?;
        let _ = channel.close().await;
        // The session is dropped here, closing the probe connection.
        Ok::<(), russh::Error>(())
    })
    .await;
    let elapsed = start.elapsed();
    match result {
        Ok(()) => DiagOutcome::success(None, elapsed),
        Err(e) => DiagOutcome::failed(e.to_string(), DiagHint::SshTunnel, elapsed),
    }
}

/// Which layer a full connection attempt's error belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    Auth,
    Tls,
    Other,
}

/// Attribute a connect/PING error to TLS, AUTH, or "other". Auth is
/// checked first: its messages never contain TLS vocabulary, while a TLS
/// mismatch frequently surfaces as a generic-looking IO error.
fn classify_redis_failure(message: &str, is_auth_kind: bool) -> FailureClass {
    let lower = message.to_lowercase();
    if is_auth_kind
        || lower.contains("noauth")
        || lower.contains("wrongpass")
        || lower.contains("invalid username-password")
        || lower.contains("client sent auth")
    {
        return FailureClass::Auth;
    }
    if lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("corrupt message")
        || lower.contains("invalid peer")
    {
        return FailureClass::Tls;
    }
    FailureClass::Other
}

/// Outcomes of the real end-to-end connection attempt, split into the
/// stages the UI shows.
pub struct RedisProbe {
    pub tls: DiagOutcome,
    pub auth: DiagOutcome,
    pub ping: DiagOutcome,
}

/// Run one real connection (the same path the app itself uses, including
/// the SSH tunnel when configured) plus a PING, and attribute the result.
pub async fn probe_redis(server: &RedisServer) -> RedisProbe {
    // The configured host of a Sentinel entry *is* a sentinel: dial it with
    // the sentinel's own credentials when it has some.
    let dialed = if server.server_type == Some(SERVER_TYPE_SENTINEL) && server.has_sentinel_credentials() {
        server.sentinel_login()
    } else {
        server.clone()
    };
    let server = &dialed;
    let tls_enabled = server.tls.unwrap_or(false);
    let has_credentials = server.password.as_deref().is_some_and(|p| !p.trim().is_empty())
        || server.username.as_deref().is_some_and(|u| !u.trim().is_empty());
    let start = Instant::now();
    let result = async {
        let mut conn = open_single_connection(server, 0, false).await?;
        let _: () = cmd("PING").query_async(&mut conn).await?;
        Ok::<(), Error>(())
    }
    .await;
    let elapsed = start.elapsed();

    let tls_ok = || {
        if tls_enabled {
            DiagOutcome::success(None, Duration::ZERO)
        } else {
            DiagOutcome::skipped(None, None)
        }
    };
    match result {
        Ok(()) => {
            let auth = if has_credentials {
                DiagOutcome::success(None, Duration::ZERO)
            } else {
                DiagOutcome::skipped(None, Some(DiagHint::AuthNotConfigured))
            };
            RedisProbe {
                tls: tls_ok(),
                auth,
                ping: DiagOutcome::success(None, elapsed),
            }
        }
        Err(e) => {
            let message = e.to_string();
            let is_auth_kind =
                matches!(&e, Error::Redis { source } if source.kind() == ErrorKind::AuthenticationFailed);
            match classify_redis_failure(&message, is_auth_kind) {
                FailureClass::Auth => {
                    let hint = if has_credentials {
                        DiagHint::AuthRejected
                    } else {
                        DiagHint::AuthRequired
                    };
                    RedisProbe {
                        tls: tls_ok(),
                        auth: DiagOutcome::failed(message, hint, elapsed),
                        ping: DiagOutcome::skipped(None, None),
                    }
                }
                FailureClass::Tls if tls_enabled => RedisProbe {
                    tls: DiagOutcome::failed(message, DiagHint::Tls, elapsed),
                    auth: DiagOutcome::skipped(None, None),
                    ping: DiagOutcome::skipped(None, None),
                },
                // Unclassified (or TLS-looking without TLS enabled): blame
                // the Redis handshake stage; TLS truthfully stays unknown.
                _ => RedisProbe {
                    tls: DiagOutcome::skipped(None, None),
                    auth: DiagOutcome::skipped(None, None),
                    ping: DiagOutcome::failed(message, DiagHint::Redis, elapsed),
                },
            }
        }
    }
}

/// Per-stage probe timeout: the server's connect timeout.
pub fn diag_timeout(server: &RedisServer) -> Duration {
    resolve_connection_timeout(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: u16) -> RedisServer {
        RedisServer {
            host: host.to_string(),
            port,
            ..Default::default()
        }
    }

    #[test]
    fn test_dial_endpoint() {
        let s = server("redis.internal", 6379);
        assert_eq!(dial_endpoint_with(&s, None), ("redis.internal".to_string(), 6379));
        // A multi-address Sentinel entry dials its first seed.
        let seeds = server("s1.internal:26379, s2.internal:26380", 26379);
        assert_eq!(dial_endpoint_with(&seeds, None), ("s1.internal".to_string(), 26379));

        let mut tunneled = server("10.0.0.5", 6380);
        tunneled.ssh_tunnel = Some(true);
        tunneled.ssh_addr = Some("bastion.example.com:2222".to_string());
        assert_eq!(
            dial_endpoint_with(&tunneled, None),
            ("bastion.example.com".to_string(), 2222)
        );

        tunneled.ssh_addr = Some("bastion.example.com".to_string());
        assert_eq!(
            dial_endpoint_with(&tunneled, None),
            ("bastion.example.com".to_string(), 22)
        );

        // ssh config: the alias resolves, and a ProxyJump is what gets dialed.
        let config = "Host bastion\n  HostName 10.9.9.9\n  Port 2200\nHost prod\n  ProxyJump bastion\n";
        tunneled.ssh_addr = Some("bastion".to_string());
        assert_eq!(
            dial_endpoint_with(&tunneled, Some(config)),
            ("10.9.9.9".to_string(), 2200)
        );
        tunneled.ssh_addr = Some("prod".to_string());
        assert_eq!(
            dial_endpoint_with(&tunneled, Some(config)),
            ("10.9.9.9".to_string(), 2200)
        );
    }

    #[test]
    fn unix_socket_skips_the_network_stages() {
        let s = server("/var/run/redis.sock", 0);
        assert_eq!(diag_stages(&s), vec![DiagStage::Auth, DiagStage::Ping]);
    }

    #[test]
    fn test_diag_stages() {
        let plain = server("127.0.0.1", 6379);
        assert_eq!(
            diag_stages(&plain),
            vec![DiagStage::Dns, DiagStage::Tcp, DiagStage::Auth, DiagStage::Ping]
        );

        let mut full = server("127.0.0.1", 6379);
        full.tls = Some(true);
        full.ssh_tunnel = Some(true);
        full.ssh_addr = Some("bastion:22".to_string());
        assert_eq!(
            diag_stages(&full),
            vec![
                DiagStage::Dns,
                DiagStage::Tcp,
                DiagStage::SshAuth,
                DiagStage::SshTunnel,
                DiagStage::Tls,
                DiagStage::Auth,
                DiagStage::Ping
            ]
        );
    }

    #[test]
    fn test_classify_redis_failure() {
        assert_eq!(
            classify_redis_failure("NOAUTH Authentication required.", false),
            FailureClass::Auth
        );
        assert_eq!(
            classify_redis_failure("WRONGPASS invalid username-password pair", true),
            FailureClass::Auth
        );
        assert_eq!(
            classify_redis_failure("ERR Client sent AUTH, but no password is set", false),
            FailureClass::Auth
        );
        assert_eq!(
            classify_redis_failure("invalid peer certificate: UnknownIssuer", false),
            FailureClass::Tls
        );
        assert_eq!(
            classify_redis_failure("TLS handshake over SSH tunnel failed: oops", false),
            FailureClass::Tls
        );
        assert_eq!(
            classify_redis_failure("Connection reset by peer (os error 54)", false),
            FailureClass::Other
        );
    }

    #[test]
    fn test_probe_dns_ip_literal_is_skipped() {
        let (outcome, addrs) = smol::block_on(probe_dns("127.0.0.1", 6379, Duration::from_secs(1)));
        assert_eq!(outcome.status, DiagStatus::Skipped);
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].port(), 6379);
    }
}
