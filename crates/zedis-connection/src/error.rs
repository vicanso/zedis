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

use redis::ErrorKind;
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Invalid: {message}"))]
    Invalid { message: String },
    #[snafu(display("Redis error: {source}"))]
    Redis { source: redis::RedisError },
    #[snafu(display("IO error: {source}"))]
    Io { source: std::io::Error },
    #[snafu(display("Serde json error: {source}"))]
    SerdeJson { source: serde_json::Error },
    #[snafu(display("Serde toml error: {source}"))]
    TomlDe { source: toml::de::Error },
    #[snafu(display("Toml serialize error: {source}"))]
    TomlSe { source: toml::ser::Error },
    #[snafu(display("Ssh error: {source}"))]
    Ssh { source: russh::Error },
    #[snafu(display("Key error: {source}"))]
    Key { source: russh::keys::Error },
}

/// Why a connection or command failed, as far as the driver lets us tell.
/// Lets the UI explain a dropped link ("Connection timed out", "Authentication
/// failed") instead of echoing a raw redis string. Best-effort: `Unknown` when
/// the error doesn't carry enough to disambiguate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionErrorKind {
    /// Couldn't classify — caller should fall back to a generic message.
    #[default]
    Unknown,
    /// Missing or wrong password (NOAUTH / WRONGPASS / auth required).
    Auth,
    /// Authenticated but the ACL user lacks permission (NOPERM).
    Permission,
    /// Connection or response timed out.
    Timeout,
    /// Host refused the connection, dropped it, or is unreachable.
    Network,
    /// The link was dropped on a plaintext connection — the endpoint likely
    /// requires TLS (e.g. Upstash on :6379). Points the user at the TLS toggle.
    Tls,
    /// The SSH tunnel itself failed to establish.
    Tunnel,
    /// `-LOADING`: the server is (re)starting and still loading its dataset.
    Loading,
    /// `-BUSY`: a long-running script / function blocks every other command.
    Busy,
    /// `-MASTERDOWN`: the replica's master is down and it refuses reads.
    MasterDown,
    /// `-READONLY`: a write hit a replica — typically the old master after
    /// a Sentinel / Cluster failover, still cached as "the master".
    ReadOnly,
    /// `-CLUSTERDOWN`: the cluster can't serve (slots uncovered / majority
    /// of masters down).
    ClusterDown,
}

impl ConnectionErrorKind {
    /// i18n key (in the `status_bar` section) naming this reason. `Unknown`
    /// falls back to the generic "offline" label, so callers get a sensible
    /// string for every variant — used by the offline tooltip and by the
    /// live-tail / MONITOR failure toasts.
    pub fn reason_key(self) -> &'static str {
        match self {
            ConnectionErrorKind::Auth => "conn_reason_auth",
            ConnectionErrorKind::Permission => "conn_reason_permission",
            ConnectionErrorKind::Timeout => "conn_reason_timeout",
            ConnectionErrorKind::Network => "conn_reason_network",
            ConnectionErrorKind::Tls => "conn_reason_tls",
            ConnectionErrorKind::Tunnel => "conn_reason_tunnel",
            ConnectionErrorKind::Loading => "conn_reason_loading",
            ConnectionErrorKind::Busy => "conn_reason_busy",
            ConnectionErrorKind::MasterDown => "conn_reason_masterdown",
            ConnectionErrorKind::ReadOnly => "conn_reason_readonly",
            ConnectionErrorKind::ClusterDown => "conn_reason_clusterdown",
            ConnectionErrorKind::Unknown => "conn_offline",
        }
    }

    /// A condition that clears by itself (restart finishing, script ending,
    /// failover completing, network coming back) — worth a retry later,
    /// unlike a bad password or a missing permission.
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            ConnectionErrorKind::Timeout
                | ConnectionErrorKind::Network
                | ConnectionErrorKind::Loading
                | ConnectionErrorKind::Busy
                | ConnectionErrorKind::MasterDown
                | ConnectionErrorKind::ClusterDown
        )
    }

    /// The cached topology is stale: the node we call "master" no longer is
    /// (or is gone). The client must be rebuilt so discovery runs again.
    pub const fn is_topology_change(self) -> bool {
        matches!(self, ConnectionErrorKind::ReadOnly | ConnectionErrorKind::MasterDown)
    }
}

impl Error {
    /// Best-effort semantic classification of a connection/command failure,
    /// used to tell the user *why* a link went down rather than surfacing a
    /// raw driver string. Order matters: timeouts also report as IO errors, so
    /// they're checked first.
    pub fn connection_kind(&self) -> ConnectionErrorKind {
        use ConnectionErrorKind as K;
        match self {
            Error::Redis { source } => {
                if source.is_timeout() {
                    return K::Timeout;
                }
                if matches!(source.kind(), ErrorKind::AuthenticationFailed) {
                    return K::Auth;
                }
                match source.code() {
                    Some("NOAUTH" | "WRONGPASS") => return K::Auth,
                    Some("NOPERM") => return K::Permission,
                    Some("LOADING") => return K::Loading,
                    Some("BUSY") => return K::Busy,
                    Some("MASTERDOWN") => return K::MasterDown,
                    Some("READONLY") => return K::ReadOnly,
                    Some("CLUSTERDOWN") => return K::ClusterDown,
                    _ => {}
                }
                if source.is_connection_refusal() || source.is_connection_dropped() || source.is_io_error() {
                    K::Network
                } else {
                    K::Unknown
                }
            }
            Error::Io { .. } => K::Network,
            Error::Ssh { .. } | Error::Key { .. } => K::Tunnel,
            _ => K::Unknown,
        }
    }

    /// Like [`Self::connection_kind`], but reports [`ConnectionErrorKind::Tls`]
    /// when the link was *dropped* on a plaintext connection — a handshake that
    /// gets accepted then reset on `:6379` usually means the endpoint requires
    /// TLS. Only applies when TLS is not already enabled.
    pub fn connection_kind_tls_aware(&self, tls_enabled: bool) -> ConnectionErrorKind {
        let kind = self.connection_kind();
        if kind == ConnectionErrorKind::Network && !tls_enabled && self.is_connection_dropped() {
            return ConnectionErrorKind::Tls;
        }
        kind
    }

    /// Whether the connection was accepted and then dropped (broken pipe /
    /// reset / unexpected EOF), as opposed to refused outright.
    fn is_connection_dropped(&self) -> bool {
        match self {
            Error::Redis { source } => source.is_connection_dropped(),
            Error::Io { source } => matches!(
                source.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            ),
            _ => false,
        }
    }
}

impl From<redis::RedisError> for Error {
    fn from(source: redis::RedisError) -> Self {
        Error::Redis { source }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { source }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error::SerdeJson { source }
    }
}

impl From<toml::de::Error> for Error {
    fn from(source: toml::de::Error) -> Self {
        Error::TomlDe { source }
    }
}

impl From<toml::ser::Error> for Error {
    fn from(source: toml::ser::Error) -> Self {
        Error::TomlSe { source }
    }
}

impl From<russh::Error> for Error {
    fn from(source: russh::Error) -> Self {
        Error::Ssh { source }
    }
}

impl From<russh::keys::Error> for Error {
    fn from(source: russh::keys::Error) -> Self {
        Error::Key { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_connection_failures() {
        // A raw IO failure reads as an unreachable host.
        let io = Error::Io {
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
        };
        assert_eq!(io.connection_kind(), ConnectionErrorKind::Network);

        // A redis auth-kind error maps to Auth.
        let auth = Error::Redis {
            source: redis::RedisError::from((ErrorKind::AuthenticationFailed, "auth failed")),
        };
        assert_eq!(auth.connection_kind(), ConnectionErrorKind::Auth);

        // Anything that isn't a connection/command failure stays Unknown so
        // the UI falls back to a generic message rather than mislabeling it.
        let other = Error::Invalid { message: "x".into() };
        assert_eq!(other.connection_kind(), ConnectionErrorKind::Unknown);
    }

    #[test]
    fn classifies_server_state_replies() {
        use redis::ServerErrorKind as S;
        let kind = |server: S| {
            Error::Redis {
                source: redis::RedisError::from((ErrorKind::Server(server), "server state")),
            }
            .connection_kind()
        };
        assert_eq!(kind(S::BusyLoading), ConnectionErrorKind::Loading);
        assert_eq!(kind(S::MasterDown), ConnectionErrorKind::MasterDown);
        assert_eq!(kind(S::ReadOnly), ConnectionErrorKind::ReadOnly);
        assert_eq!(kind(S::ClusterDown), ConnectionErrorKind::ClusterDown);
        assert_eq!(kind(S::NoPerm), ConnectionErrorKind::Permission);
        // Transient vs. topology-change vs. the rest drive the recovery path.
        assert!(ConnectionErrorKind::Loading.is_transient());
        assert!(ConnectionErrorKind::Network.is_transient());
        assert!(!ConnectionErrorKind::Auth.is_transient());
        assert!(ConnectionErrorKind::ReadOnly.is_topology_change());
        assert!(!ConnectionErrorKind::Loading.is_topology_change());
        // Every kind names a reason key.
        for k in [
            ConnectionErrorKind::Loading,
            ConnectionErrorKind::Busy,
            ConnectionErrorKind::MasterDown,
            ConnectionErrorKind::ReadOnly,
            ConnectionErrorKind::ClusterDown,
        ] {
            assert!(k.reason_key().starts_with("conn_reason_"), "{k:?}");
        }
    }
}
