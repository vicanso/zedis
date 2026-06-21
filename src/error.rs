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
    #[snafu(display("Redb error: {source}"))]
    Redb { source: redb::Error },
    #[snafu(display("Redb database error: {source}"))]
    RedbDatabase { source: redb::DatabaseError },
    #[snafu(display("Redb transaction error: {source}"))]
    RedbTransaction { source: redb::TransactionError },

    #[snafu(display("Redb table error: {source}"))]
    RedbTable { source: redb::TableError },
    #[snafu(display("Redb commit error: {source}"))]
    RedbCommit { source: redb::CommitError },

    #[snafu(display("Redb storage error: {source}"))]
    RedbStorage { source: redb::StorageError },

    #[snafu(display("Protox error: {source}"))]
    Protox { source: protox::Error },

    #[snafu(display("Prost reflect descriptor error: {source}"))]
    ProstReflectDescriptor { source: prost_reflect::DescriptorError },

    #[snafu(display("Prost reflect decode error: {source}"))]
    ProstReflectDecode { source: prost_reflect::prost::DecodeError },
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
    /// The SSH tunnel itself failed to establish.
    Tunnel,
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

impl From<redb::Error> for Error {
    fn from(source: redb::Error) -> Self {
        Error::Redb { source }
    }
}

impl From<redb::DatabaseError> for Error {
    fn from(source: redb::DatabaseError) -> Self {
        Error::RedbDatabase { source }
    }
}

impl From<redb::TransactionError> for Error {
    fn from(source: redb::TransactionError) -> Self {
        Error::RedbTransaction { source }
    }
}

impl From<redb::TableError> for Error {
    fn from(source: redb::TableError) -> Self {
        Error::RedbTable { source }
    }
}

impl From<redb::CommitError> for Error {
    fn from(source: redb::CommitError) -> Self {
        Error::RedbCommit { source }
    }
}

impl From<redb::StorageError> for Error {
    fn from(source: redb::StorageError) -> Self {
        Error::RedbStorage { source }
    }
}

impl From<protox::Error> for Error {
    fn from(source: protox::Error) -> Self {
        Error::Protox { source }
    }
}

impl From<prost_reflect::DescriptorError> for Error {
    fn from(source: prost_reflect::DescriptorError) -> Self {
        Error::ProstReflectDescriptor { source }
    }
}

impl From<prost_reflect::prost::DecodeError> for Error {
    fn from(source: prost_reflect::prost::DecodeError) -> Self {
        Error::ProstReflectDecode { source }
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
}
