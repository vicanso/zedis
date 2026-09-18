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

// The storage engine, whichever this target has: redb's file database on the
// desktop, `mem_store`'s `BTreeMap` in a browser tab (ADR 9). Aliased rather
// than named twice so every variant below reads the same on both.
#[cfg(target_family = "wasm")]
use crate::mem_store as store;
#[cfg(not(target_family = "wasm"))]
use redb as store;

/// Errors of the local storage layer: the embedded redb database plus the
/// proto-descriptor compilation the proto manager performs.
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Invalid: {message}"))]
    Invalid { message: String },
    #[snafu(display("IO error: {source}"))]
    Io { source: std::io::Error },
    #[snafu(display("Serde json error: {source}"))]
    SerdeJson { source: serde_json::Error },
    #[cfg(not(target_family = "wasm"))]
    #[snafu(display("Redb error: {source}"))]
    Redb { source: store::Error },
    #[snafu(display("Redb database error: {source}"))]
    RedbDatabase { source: store::DatabaseError },
    #[snafu(display("Redb transaction error: {source}"))]
    RedbTransaction { source: store::TransactionError },
    #[snafu(display("Redb table error: {source}"))]
    RedbTable { source: store::TableError },
    #[snafu(display("Redb commit error: {source}"))]
    RedbCommit { source: store::CommitError },
    #[snafu(display("Redb storage error: {source}"))]
    RedbStorage { source: store::StorageError },
    #[cfg(not(target_family = "wasm"))]
    #[snafu(display("Protox error: {source}"))]
    Protox { source: protox::Error },
    #[cfg(not(target_family = "wasm"))]
    #[snafu(display("Prost reflect descriptor error: {source}"))]
    ProstReflectDescriptor { source: prost_reflect::DescriptorError },
    #[cfg(not(target_family = "wasm"))]
    #[snafu(display("Prost reflect decode error: {source}"))]
    ProstReflectDecode { source: prost_reflect::prost::DecodeError },
    /// The file carries a schema version this build doesn't know — it was
    /// written by a newer Zedis. Refusing is the safe move: a downgrade that
    /// "migrated" forward would corrupt what the newer version wrote.
    #[snafu(display("local database schema v{found} is newer than this Zedis supports (v{supported})"))]
    SchemaTooNew { found: u32, supported: u32 },
}

/// `From` for the errors every target has.
macro_rules! direct {
    ($($ty:ty => $variant:ident),+ $(,)?) => {$(
        impl From<$ty> for Error {
            fn from(source: $ty) -> Self {
                Error::$variant { source }
            }
        }
    )+};
}
direct!(
    std::io::Error => Io,
    serde_json::Error => SerdeJson,
    store::DatabaseError => RedbDatabase,
    store::TransactionError => RedbTransaction,
    store::TableError => RedbTable,
    store::CommitError => RedbCommit,
    store::StorageError => RedbStorage,
);

#[cfg(not(target_family = "wasm"))]
direct!(
    store::Error => Redb,
    protox::Error => Protox,
    prost_reflect::DescriptorError => ProstReflectDescriptor,
    prost_reflect::prost::DecodeError => ProstReflectDecode,
);
