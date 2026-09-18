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

//! The slice of redb this crate uses, in memory, for the browser build.
//!
//! redb is a file database and a page has no file to open, but everything
//! *above* the storage — the JSON documents, the caches, the pruning rules,
//! the "skip a row you cannot read, never delete it" contract — is ordinary
//! logic that should not be written twice. So the managers keep their redb
//! code verbatim and only their storage import is switched (ADR 9), the same
//! way `bridge::BridgeQuery` substitutes for redis-rs's `query_async` without
//! a single call site changing.
//!
//! What that buys, and what it costs: the desktop path stays byte-identical
//! (this module is not compiled there at all), and the browser gets tags,
//! favourites, history and the recycle bin working for the life of the tab.
//! They do not survive a reload — there is no persistence behind this, only a
//! `BTreeMap`. That is a deliberate first step, not the destination: the
//! deployment shares one server list held by the bridge, so the shared local
//! data belongs there too, and this module is the seam a bridge-backed store
//! slots into later without touching a manager again.
//!
//! Keys are encoded so that byte order *is* key order, because `range` is
//! load-bearing: `metrics_history` prunes by `(server, timestamp)` and the
//! recycle bin lists one server's entries by `(server, id)`. A tuple escapes
//! its first component before the separator, so a longer server id can never
//! sort into a shorter one's range.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Bound, RangeBounds};
use std::sync::{Arc, Mutex, MutexGuard};

/// How a type is stored. Mirrors redb's `Value`, including the lifetime
/// gymnastics: `&str` has to come back borrowed from the guard that owns the
/// bytes, while `u32` comes back owned.
pub trait Value {
    /// What `get` hands back for this type.
    ///
    /// `Clone` is not one of redb's requirements; it is here so a range bound
    /// can be turned into bytes without borrowing from the caller's range.
    /// Every shape this crate stores is a `&str`, an integer or a tuple of
    /// them, so the clone is a copy.
    type SelfType<'a>: Clone
    where
        Self: 'a;

    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a;
    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a;
}

/// A [`Value`] whose encoding sorts the way the type does, so `range` over
/// the encoded bytes is a range over the keys.
pub trait Key: Value {}

impl Value for &str {
    type SelfType<'a>
        = &'a str
    where
        Self: 'a;

    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a,
    {
        value.as_bytes().to_vec()
    }

    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        // Only this module ever writes these bytes, and it only ever writes
        // what `encode` produced from a `&str`.
        std::str::from_utf8(data).unwrap_or_default()
    }
}
impl Key for &str {}

impl Value for &[u8] {
    type SelfType<'a>
        = &'a [u8]
    where
        Self: 'a;

    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a,
    {
        value.to_vec()
    }

    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        data
    }
}
impl Key for &[u8] {}

impl Value for u32 {
    type SelfType<'a>
        = u32
    where
        Self: 'a;

    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a,
    {
        value.to_be_bytes().to_vec()
    }

    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        let mut bytes = [0u8; 4];
        let take = data.len().min(4);
        bytes[4 - take..].copy_from_slice(&data[..take]);
        u32::from_be_bytes(bytes)
    }
}
impl Key for u32 {}

impl Value for i64 {
    type SelfType<'a>
        = i64
    where
        Self: 'a;

    /// Big-endian with the sign bit flipped, so negative timestamps sort
    /// before positive ones instead of after them.
    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a,
    {
        ((value as u64) ^ (1 << 63)).to_be_bytes().to_vec()
    }

    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        let mut bytes = [0u8; 8];
        let take = data.len().min(8);
        bytes[8 - take..].copy_from_slice(&data[..take]);
        (u64::from_be_bytes(bytes) ^ (1 << 63)) as i64
    }
}
impl Key for i64 {}

/// Escapes `part` so that no encoding of one value is a prefix of another:
/// `0x00` becomes `0x00 0xFF`, and a bare `0x00 0x00` ends the component.
/// Without this, server `"ab"` and server `"a"` would share a range.
fn push_escaped(out: &mut Vec<u8>, part: &[u8]) {
    for byte in part {
        out.push(*byte);
        if *byte == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

impl<A: Key + 'static, B: Key + 'static> Value for (A, B) {
    type SelfType<'a>
        = (A::SelfType<'a>, B::SelfType<'a>)
    where
        Self: 'a;

    fn encode<'a>(value: Self::SelfType<'a>) -> Vec<u8>
    where
        Self: 'a,
    {
        let mut out = Vec::new();
        push_escaped(&mut out, &A::encode(value.0));
        // The last component needs no terminator: nothing follows it, so a
        // plain append keeps the ordering the escaped prefix established.
        out.extend_from_slice(&B::encode(value.1));
        out
    }

    fn decode<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        // The borrow has to outlive this call, and the escaped first
        // component is a fresh `Vec`. Every tuple key in this crate has a
        // `&str` first component whose escaping is a no-op (no NUL in a
        // server id or a Redis key name), so the unescaped bytes are a
        // subslice of the input and can be handed back borrowed.
        let mut end = 0;
        while end + 1 < data.len() {
            if data[end] == 0 && data[end + 1] != 0xFF {
                break;
            }
            end += if data[end] == 0 { 2 } else { 1 };
        }
        let head = &data[..end];
        let tail = &data[(end + 2).min(data.len())..];
        (A::decode(head), B::decode(tail))
    }
}
impl<A: Key + 'static, B: Key + 'static> Key for (A, B) {}

/// A table's name and its key/value types. Same shape as redb's, so the
/// `const TABLE: TableDefinition<&str, &str> = TableDefinition::new("x")`
/// declarations in `lib.rs` are unchanged.
pub struct TableDefinition<'a, K: Key + 'static, V: Value + 'static> {
    name: &'a str,
    _types: std::marker::PhantomData<(K, V)>,
}

impl<'a, K: Key + 'static, V: Value + 'static> TableDefinition<'a, K, V> {
    pub const fn new(name: &'a str) -> Self {
        Self {
            name,
            _types: std::marker::PhantomData,
        }
    }
}

impl<K: Key + 'static, V: Value + 'static> Clone for TableDefinition<'_, K, V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K: Key + 'static, V: Value + 'static> Copy for TableDefinition<'_, K, V> {}

/// redb's trait for reading a table's name. Only `lib.rs`'s schema test
/// asks for it, and that test is desktop-only, so it is not carried into the
/// shipped wasm.
#[cfg(test)]
pub trait TableHandle {
    fn name(&self) -> &str;
}

#[cfg(test)]
impl<K: Key + 'static, V: Value + 'static> TableHandle for TableDefinition<'_, K, V> {
    fn name(&self) -> &str {
        self.name
    }
}

/// The errors redb distinguishes, kept as separate types so `error.rs` keeps
/// one variant per failure the UI can act on.
#[derive(Debug)]
pub struct StorageError(String);
#[derive(Debug)]
pub struct TransactionError(String);
#[derive(Debug)]
pub struct CommitError(String);

/// The one table error the crate matches on by name: a read transaction
/// cannot create a table, so a missing one is how "not written yet" reads.
#[derive(Debug)]
pub enum TableError {
    TableDoesNotExist(String),
}

/// Why opening failed. Nothing here can fail to open — there is no file and
/// no lock — but the type has to exist for `Database::create` to keep redb's
/// signature, and for `error.rs` to keep one variant per failure kind.
#[derive(Debug)]
pub enum DatabaseError {
    Storage(StorageError),
}

macro_rules! display_error {
    ($($t:ty),* $(,)?) => {
        $(
            impl fmt::Display for $t {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}", self.0)
                }
            }
            impl std::error::Error for $t {}
        )*
    };
}
display_error!(StorageError, TransactionError, CommitError);

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableError::TableDoesNotExist(name) => write!(f, "table \"{name}\" does not exist"),
        }
    }
}
impl std::error::Error for TableError {}

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DatabaseError::Storage(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for DatabaseError {}

type Rows = BTreeMap<Vec<u8>, Vec<u8>>;
type Tables = BTreeMap<String, Rows>;

/// The database: every table, behind one lock.
///
/// One lock rather than one per table because a write transaction here is the
/// whole map — `ensure_schema` opens ten tables in a single transaction, and
/// a caller that saw half of one committed would be a bug the desktop cannot
/// have.
pub struct Database {
    tables: Arc<Mutex<Tables>>,
}

impl Database {
    /// The path redb would have used is accepted and ignored, so `lib.rs`
    /// keeps one `init_database`.
    pub fn create(_path: impl AsRef<std::path::Path>) -> Result<Self, DatabaseError> {
        Ok(Self {
            tables: Arc::new(Mutex::new(Tables::new())),
        })
    }
}

/// Reading a database. Mirrors redb's trait so the `use` line is all that
/// differs between the two builds.
pub trait ReadableDatabase {
    fn begin_read(&self) -> Result<ReadTransaction, TransactionError>;
}

impl ReadableDatabase for Database {
    fn begin_read(&self) -> Result<ReadTransaction, TransactionError> {
        Ok(ReadTransaction {
            tables: Arc::clone(&self.tables),
        })
    }
}

impl Database {
    pub fn begin_write(&self) -> Result<WriteTransaction, TransactionError> {
        Ok(WriteTransaction {
            tables: Arc::clone(&self.tables),
            staged: Mutex::new(Tables::new()),
        })
    }
}

pub struct ReadTransaction {
    tables: Arc<Mutex<Tables>>,
}

impl ReadTransaction {
    /// A snapshot of the table, or [`TableError::TableDoesNotExist`] — the
    /// same refusal redb gives, which is what `lib.rs`'s schema test asserts
    /// a read transaction cannot paper over.
    pub fn open_table<K: Key + 'static, V: Value + 'static>(
        &self,
        definition: TableDefinition<'_, K, V>,
    ) -> Result<ReadOnlyTable<K, V>, TableError> {
        let guard = self.lock();
        let rows = guard
            .get(definition.name)
            .ok_or_else(|| TableError::TableDoesNotExist(definition.name.to_string()))?;
        Ok(ReadOnlyTable {
            rows: rows.clone(),
            _types: std::marker::PhantomData,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A write transaction. Changes land in `staged` and reach the database only
/// on [`WriteTransaction::commit`], so a failure part-way leaves nothing
/// behind — the property `ensure_schema` relies on to rule out a
/// half-migrated database.
pub struct WriteTransaction {
    tables: Arc<Mutex<Tables>>,
    staged: Mutex<Tables>,
}

impl WriteTransaction {
    /// Opens the table, creating it if this is its first sight — which is
    /// what makes `ensure_schema` able to add a table to an existing
    /// database.
    pub fn open_table<K: Key + 'static, V: Value + 'static>(
        &self,
        definition: TableDefinition<'_, K, V>,
    ) -> Result<Table<'_, K, V>, TableError> {
        let name = definition.name.to_string();
        let mut staged = self.staged.lock().unwrap_or_else(|e| e.into_inner());
        if !staged.contains_key(&name) {
            let existing = self
                .tables
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&name)
                .cloned()
                .unwrap_or_default();
            staged.insert(name.clone(), existing);
        }
        drop(staged);
        Ok(Table {
            txn: self,
            name,
            _types: std::marker::PhantomData,
        })
    }

    pub fn commit(self) -> Result<(), CommitError> {
        let staged = self.staged.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut live = self.tables.lock().unwrap_or_else(|e| e.into_inner());
        for (name, rows) in staged {
            live.insert(name, rows);
        }
        Ok(())
    }
}

/// What `get` hands back: the stored bytes, and a `value()` that reads them
/// as the table's type. Owned rather than borrowed from the table, because
/// the in-memory table is behind a lock nobody should hold across a decode.
pub struct AccessGuard<V: Value + 'static> {
    data: Vec<u8>,
    _type: std::marker::PhantomData<V>,
}

impl<V: Value + 'static> AccessGuard<V> {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            _type: std::marker::PhantomData,
        }
    }

    pub fn value(&self) -> V::SelfType<'_> {
        V::decode(&self.data)
    }
}

/// Reading a table, whichever kind of transaction opened it.
pub trait ReadableTable<K: Key + 'static, V: Value + 'static> {
    fn get(&self, key: K::SelfType<'_>) -> Result<Option<AccessGuard<V>>, StorageError>;
    fn iter(&self) -> Result<TableIter<K, V>, StorageError>;
    fn range<'a, KR>(&self, range: impl RangeBounds<KR>) -> Result<TableIter<K, V>, StorageError>
    where
        K: 'a,
        KR: Borrow<K::SelfType<'a>>;
}

/// One row, as redb's iterators yield it.
type Row<K, V> = Result<(AccessGuard<K>, AccessGuard<V>), StorageError>;

pub struct TableIter<K: Key + 'static, V: Value + 'static> {
    rows: std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
    _types: std::marker::PhantomData<(K, V)>,
}

impl<K: Key + 'static, V: Value + 'static> Iterator for TableIter<K, V> {
    type Item = Row<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows
            .next()
            .map(|(key, value)| Ok((AccessGuard::new(key), AccessGuard::new(value))))
    }
}

/// `..`, `a..b` and `a..=b` translated onto the encoded keys.
fn encoded_bounds<'a, K, KR>(range: &impl RangeBounds<KR>) -> (Bound<Vec<u8>>, Bound<Vec<u8>>)
where
    K: Key + 'static + 'a,
    KR: Borrow<K::SelfType<'a>>,
{
    fn map<'a, K, KR>(bound: Bound<&KR>) -> Bound<Vec<u8>>
    where
        K: Key + 'static + 'a,
        KR: Borrow<K::SelfType<'a>>,
    {
        match bound {
            Bound::Included(v) => Bound::Included(K::encode(v.borrow().clone())),
            Bound::Excluded(v) => Bound::Excluded(K::encode(v.borrow().clone())),
            Bound::Unbounded => Bound::Unbounded,
        }
    }
    (map::<K, KR>(range.start_bound()), map::<K, KR>(range.end_bound()))
}

fn collect_range(rows: &Rows, bounds: (Bound<Vec<u8>>, Bound<Vec<u8>>)) -> Vec<(Vec<u8>, Vec<u8>)> {
    rows.range(bounds)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// A table inside a write transaction. Every change is staged.
pub struct Table<'txn, K: Key + 'static, V: Value + 'static> {
    txn: &'txn WriteTransaction,
    name: String,
    _types: std::marker::PhantomData<(K, V)>,
}

impl<K: Key + 'static, V: Value + 'static> Table<'_, K, V> {
    fn with_rows<T>(&self, f: impl FnOnce(&mut Rows) -> T) -> T {
        let mut staged = self.txn.staged.lock().unwrap_or_else(|e| e.into_inner());
        f(staged.entry(self.name.clone()).or_default())
    }

    pub fn insert(&mut self, key: K::SelfType<'_>, value: V::SelfType<'_>) -> Result<(), StorageError> {
        let (key, value) = (K::encode(key), V::encode(value));
        self.with_rows(|rows| rows.insert(key, value));
        Ok(())
    }

    /// Returns what was there, the way redb does, so a caller can tell a
    /// delete that removed something from one that found nothing.
    pub fn remove(&mut self, key: K::SelfType<'_>) -> Result<Option<AccessGuard<V>>, StorageError> {
        let key = K::encode(key);
        Ok(self.with_rows(|rows| rows.remove(&key)).map(AccessGuard::new))
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadableTable<K, V> for Table<'_, K, V> {
    fn get(&self, key: K::SelfType<'_>) -> Result<Option<AccessGuard<V>>, StorageError> {
        let key = K::encode(key);
        Ok(self.with_rows(|rows| rows.get(&key).cloned()).map(AccessGuard::new))
    }

    fn iter(&self) -> Result<TableIter<K, V>, StorageError> {
        let rows = self.with_rows(|rows| collect_range(rows, (Bound::Unbounded, Bound::Unbounded)));
        Ok(TableIter {
            rows: rows.into_iter(),
            _types: std::marker::PhantomData,
        })
    }

    fn range<'a, KR>(&self, range: impl RangeBounds<KR>) -> Result<TableIter<K, V>, StorageError>
    where
        K: 'a,
        KR: Borrow<K::SelfType<'a>>,
    {
        let bounds = encoded_bounds::<K, KR>(&range);
        let rows = self.with_rows(|rows| collect_range(rows, bounds));
        Ok(TableIter {
            rows: rows.into_iter(),
            _types: std::marker::PhantomData,
        })
    }
}

/// A table opened from a read transaction: a snapshot, so a concurrent write
/// cannot change what a loop is walking.
pub struct ReadOnlyTable<K: Key + 'static, V: Value + 'static> {
    rows: Rows,
    _types: std::marker::PhantomData<(K, V)>,
}

impl<K: Key + 'static, V: Value + 'static> ReadableTable<K, V> for ReadOnlyTable<K, V> {
    fn get(&self, key: K::SelfType<'_>) -> Result<Option<AccessGuard<V>>, StorageError> {
        Ok(self.rows.get(&K::encode(key)).cloned().map(AccessGuard::new))
    }

    fn iter(&self) -> Result<TableIter<K, V>, StorageError> {
        Ok(TableIter {
            rows: collect_range(&self.rows, (Bound::Unbounded, Bound::Unbounded)).into_iter(),
            _types: std::marker::PhantomData,
        })
    }

    fn range<'a, KR>(&self, range: impl RangeBounds<KR>) -> Result<TableIter<K, V>, StorageError>
    where
        K: 'a,
        KR: Borrow<K::SelfType<'a>>,
    {
        Ok(TableIter {
            rows: collect_range(&self.rows, encoded_bounds::<K, KR>(&range)).into_iter(),
            _types: std::marker::PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRINGS: TableDefinition<&str, &str> = TableDefinition::new("strings");
    const SAMPLES: TableDefinition<(&str, i64), &[u8]> = TableDefinition::new("samples");
    const ENTRIES: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("entries");

    fn db() -> Database {
        Database::create("ignored").expect("create")
    }

    #[test]
    fn a_read_transaction_cannot_create_a_table() {
        let db = db();
        let read = db.begin_read().expect("read");
        assert!(
            matches!(read.open_table(STRINGS), Err(TableError::TableDoesNotExist(_))),
            "a missing table must refuse, not appear empty"
        );
    }

    #[test]
    fn nothing_is_visible_until_the_transaction_commits() {
        let db = db();
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(STRINGS).expect("open");
            table.insert("a", "1").expect("insert");
        }
        let read = db.begin_read().expect("read");
        assert!(read.open_table(STRINGS).is_err(), "an uncommitted table must not exist");

        write.commit().expect("commit");
        let read = db.begin_read().expect("read");
        let table = read.open_table(STRINGS).expect("open");
        assert_eq!(
            table.get("a").expect("get").map(|v| v.value().to_string()),
            Some("1".into())
        );
    }

    #[test]
    fn a_dropped_transaction_leaves_nothing_behind() {
        let db = db();
        {
            let write = db.begin_write().expect("write");
            let mut table = write.open_table(STRINGS).expect("open");
            table.insert("a", "1").expect("insert");
        }
        assert!(
            db.begin_read().expect("read").open_table(STRINGS).is_err(),
            "rolling back is what makes a half-migrated schema impossible"
        );
    }

    #[test]
    fn remove_reports_what_it_took() {
        let db = db();
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(STRINGS).expect("open");
            table.insert("a", "1").expect("insert");
            assert_eq!(
                table.remove("a").expect("remove").map(|v| v.value().to_string()),
                Some("1".into())
            );
            assert!(table.remove("a").expect("remove").is_none());
        }
        write.commit().expect("commit");
    }

    #[test]
    fn a_timestamp_range_is_ordered_and_stays_inside_its_server() {
        let db = db();
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(SAMPLES).expect("open");
            for ms in [-30i64, 5, 10, 20, 30] {
                table.insert(("s1", ms), b"x".as_slice()).expect("insert");
            }
            table.insert(("s2", 15), b"other".as_slice()).expect("insert");
            // A server id that *starts with* another one must not bleed in;
            // the escaped separator is what keeps them apart.
            table.insert(("s10", 15), b"neighbour".as_slice()).expect("insert");
        }
        write.commit().expect("commit");

        let read = db.begin_read().expect("read");
        let table = read.open_table(SAMPLES).expect("open");
        let found: Vec<i64> = table
            .range(("s1", i64::MIN)..("s1", i64::MAX))
            .expect("range")
            .filter_map(|row| row.ok())
            .map(|(key, _)| key.value().1)
            .collect();
        assert_eq!(found, vec![-30, 5, 10, 20, 30], "negative timestamps sort first");

        let from_ten: Vec<i64> = table
            .range(("s1", 10)..("s1", i64::MAX))
            .expect("range")
            .filter_map(|row| row.ok())
            .map(|(key, _)| key.value().1)
            .collect();
        assert_eq!(from_ten, vec![10, 20, 30], "the start bound is inclusive");
    }

    #[test]
    fn an_inclusive_string_range_lists_one_servers_entries() {
        let db = db();
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(ENTRIES).expect("open");
            for id in ["a", "b", "c"] {
                table.insert(("s1", id), b"v".as_slice()).expect("insert");
            }
            table.insert(("s1x", "a"), b"v".as_slice()).expect("insert");
            table.insert(("s2", "a"), b"v".as_slice()).expect("insert");
        }
        write.commit().expect("commit");

        let read = db.begin_read().expect("read");
        let table = read.open_table(ENTRIES).expect("open");
        let ids: Vec<String> = table
            .range(("s1", "")..=("s1", "\u{10FFFF}"))
            .expect("range")
            .filter_map(|row| row.ok())
            .map(|(key, _)| key.value().1.to_string())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"], "a neighbouring server id must not appear");
    }

    #[test]
    fn a_key_round_trips_through_its_encoding() {
        let encoded = <(&str, i64)>::encode(("server-1", -7));
        let (server, ms) = <(&str, i64)>::decode(&encoded);
        assert_eq!((server, ms), ("server-1", -7));

        let encoded = <(&str, &str)>::encode(("s", "id:with:colons"));
        assert_eq!(<(&str, &str)>::decode(&encoded), ("s", "id:with:colons"));
    }

    #[test]
    fn a_table_knows_its_name() {
        assert_eq!(STRINGS.name(), "strings");
    }

    #[test]
    fn every_failure_says_which_one_it_was() {
        assert_eq!(
            TableError::TableDoesNotExist("strings".into()).to_string(),
            "table \"strings\" does not exist"
        );
        assert_eq!(DatabaseError::Storage(StorageError("disk".into())).to_string(), "disk");
        assert_eq!(TransactionError("no txn".into()).to_string(), "no txn");
        assert_eq!(CommitError("no commit".into()).to_string(), "no commit");
    }

    #[test]
    fn iter_walks_every_row_in_key_order() {
        let db = db();
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(STRINGS).expect("open");
            for key in ["c", "a", "b"] {
                table.insert(key, key).expect("insert");
            }
        }
        write.commit().expect("commit");

        let read = db.begin_read().expect("read");
        let table = read.open_table(STRINGS).expect("open");
        let keys: Vec<String> = table
            .iter()
            .expect("iter")
            .filter_map(|row| row.ok())
            .map(|(key, _)| key.value().to_string())
            .collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }
}
