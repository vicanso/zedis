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
