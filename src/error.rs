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

use snafu::Snafu;

pub use zedis_connection::error::ConnectionErrorKind;

type ConnectionError = zedis_connection::error::Error;

/// App-level error. Connection-layer failures pass through transparently;
/// the extra variants exist only for errors the app itself produces (proto
/// decoding, the local redb database) so `zedis-connection` doesn't have to
/// depend on those crates just to name them.
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(transparent)]
    Connection { source: ConnectionError },
    #[snafu(transparent)]
    Db { source: zedis_db::error::Error },
    #[snafu(display("Invalid: {message}"))]
    Invalid { message: String },
}

impl Error {
    /// Delegates to the connection layer's classifier; app-domain errors are
    /// never link failures, so they report `Unknown`.
    pub fn connection_kind(&self) -> ConnectionErrorKind {
        match self {
            Error::Connection { source } => source.connection_kind(),
            _ => ConnectionErrorKind::Unknown,
        }
    }

    /// See [`zedis_connection::error::Error::connection_kind_tls_aware`].
    pub fn connection_kind_tls_aware(&self, tls_enabled: bool) -> ConnectionErrorKind {
        match self {
            Error::Connection { source } => source.connection_kind_tls_aware(tls_enabled),
            _ => ConnectionErrorKind::Unknown,
        }
    }
}

// External errors the app converts with `?` — routed through the connection
// error (which owns their From impls) so classification keeps working.
macro_rules! via_connection {
    ($($ty:ty),+ $(,)?) => {$(
        impl From<$ty> for Error {
            fn from(source: $ty) -> Self {
                Error::Connection {
                    source: ConnectionError::from(source),
                }
            }
        }
    )+};
}
via_connection!(
    redis::RedisError,
    std::io::Error,
    serde_json::Error,
    toml::de::Error,
    toml::ser::Error,
);
