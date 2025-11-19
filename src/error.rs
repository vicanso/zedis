// Copyright 2025 Tree xie.
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
    #[snafu(display("Serde JSON error: {source}"))]
    SerdeJson { source: serde_json::Error },
    #[snafu(display("Serde TOML error: {source}"))]
    TomlDe { source: toml::de::Error },
    #[snafu(display("TOML serialize error: {source}"))]
    TomlSe { source: toml::ser::Error },
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
