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

//! Atomic slot migration: `CLUSTER MIGRATESLOTS` / `GETSLOTMIGRATIONS` /
//! `CANCELSLOTMIGRATIONS` on Valkey 9, `CLUSTER MIGRATION IMPORT` /
//! `STATUS` / `CANCEL` on Redis 8.4 — one job, two dialects.
//!
//! The legacy reshard the app drives itself — `SETSLOT MIGRATING` /
//! `IMPORTING`, a `GETKEYSINSLOT` + `MIGRATE` loop per slot, then
//! `SETSLOT NODE` fanned out — moves keys one batch at a time and leaves a
//! slot marked on both ends if anything interrupts it. Both servers move
//! whole slots server-side instead: one command starts the job, the server
//! owns it, and closing the app cannot strand a slot. The legacy path is
//! still there on both, so this is a second route gated by
//! [`floors::ATOMIC_SLOT_MIGRATION`](crate::floors::ATOMIC_SLOT_MIGRATION),
//! not a replacement.
//!
//! The dialects differ in who is told. Valkey's job is the **source's**:
//! `MIGRATESLOTS … NODE target` goes to the node giving the slots up, which
//! pushes them. Redis's is the **target's**: `MIGRATION IMPORT start end`
//! goes to the node taking them, which pulls them from their owners. Both
//! track the job on both ends under one id and answer the status from
//! either, so the poll is the same. [`SlotMigrationDialect`] names which
//! command each end takes; `cluster_ops` picks the node. The rows are read
//! into one [`AtomicSlotMigration`] in Valkey's vocabulary: Redis's
//! `migrate` / `import` operations become `EXPORT` / `IMPORT`, its
//! millisecond times seconds, its `completed` done beside Valkey's `success`.

use crate::error::Error;
use redis::aio::ConnectionLike;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Which server's spelling of the job this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotMigrationDialect {
    /// Valkey 9: `CLUSTER MIGRATESLOTS`, told to the source.
    Valkey,
    /// Redis 8.4: `CLUSTER MIGRATION IMPORT`, told to the target.
    Redis,
}

impl SlotMigrationDialect {
    pub fn for_flavor(is_valkey: bool) -> Self {
        if is_valkey { Self::Valkey } else { Self::Redis }
    }

    /// Whether the start command goes to the node *taking* the slots
    /// rather than the one giving them up.
    pub fn starts_on_target(self) -> bool {
        matches!(self, Self::Redis)
    }

    pub fn start_command(self) -> &'static str {
        match self {
            Self::Valkey => "CLUSTER MIGRATESLOTS",
            Self::Redis => "CLUSTER MIGRATION IMPORT",
        }
    }

    pub fn status_command(self) -> &'static str {
        match self {
            Self::Valkey => "CLUSTER GETSLOTMIGRATIONS",
            Self::Redis => "CLUSTER MIGRATION STATUS",
        }
    }

    pub fn cancel_command(self) -> &'static str {
        match self {
            Self::Valkey => "CLUSTER CANCELSLOTMIGRATIONS",
            Self::Redis => "CLUSTER MIGRATION CANCEL",
        }
    }
}

/// One row of `CLUSTER GETSLOTMIGRATIONS` (Valkey) or `CLUSTER MIGRATION
/// STATUS ALL` (Redis), in Valkey's words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtomicSlotMigration {
    /// The migration's 40-byte name (Redis: its task id), and what a cancel
    /// reports against.
    pub name: String,
    /// `EXPORT` on the source node, `IMPORT` on the target — the same job
    /// seen from either end. Redis says `migrate` for the source's side and
    /// is read as `EXPORT`.
    pub operation: String,
    /// The slot ranges as the server prints them (`"0-10 20-30"`).
    pub slot_ranges: String,
    pub target_node: String,
    pub source_node: String,
    /// Unix seconds.
    pub create_time: i64,
    pub last_update_time: i64,
    pub last_ack_time: i64,
    /// `success` / `completed`, `failed` and `cancelled` are terminal;
    /// anything else is a migration still running.
    pub state: String,
    /// The server's explanation when a migration failed.
    pub message: String,
    /// Copy-on-write memory the fork is holding, in bytes (Valkey only).
    pub cow_size: u64,
    /// Bytes still to send (Valkey only).
    pub remaining_repl_size: u64,
}

impl AtomicSlotMigration {
    /// Still running. The terminal states are the ones each server
    /// documents; treating an unknown state as active is deliberate, so a
    /// state added later shows up as in-flight rather than finished.
    pub fn is_active(&self) -> bool {
        !matches!(
            self.state.as_str(),
            "success" | "completed" | "failed" | "cancelled" | "canceled"
        )
    }

    /// The source's side of the job — the end the Reshard tab lists, so a
    /// job is one row and not two.
    pub fn is_export(&self) -> bool {
        self.operation.eq_ignore_ascii_case("EXPORT")
    }
}

/// Start moving `ranges` to `target_id`. On Valkey `conn` is the **source**
/// (`CLUSTER MIGRATESLOTS SLOTSRANGE … NODE target`), on Redis the
/// **target** (`CLUSTER MIGRATION IMPORT start end …`, which needs no node
/// id: the slots' owners are the sources) — [`SlotMigrationDialect::starts_on_target`]
/// says which. Returns as soon as the server accepted the job; the
/// migration itself is watched through [`cluster_get_slot_migrations`].
pub async fn cluster_migrate_slots<C: ConnectionLike + Send>(
    conn: &mut C,
    dialect: SlotMigrationDialect,
    ranges: &[(u16, u16)],
    target_id: &str,
) -> Result<()> {
    if ranges.is_empty() {
        return Err(Error::Invalid {
            message: "no slot ranges to migrate".to_string(),
        });
    }
    let mut c = cmd("CLUSTER");
    match dialect {
        SlotMigrationDialect::Valkey => {
            c.arg("MIGRATESLOTS").arg("SLOTSRANGE");
            for (start, end) in ranges {
                c.arg(*start).arg(*end);
            }
            c.arg("NODE").arg(target_id);
        }
        SlotMigrationDialect::Redis => {
            c.arg("MIGRATION").arg("IMPORT");
            for (start, end) in ranges {
                c.arg(*start).arg(*end);
            }
        }
    }
    // Valkey answers OK, Redis the task id; the status list carries the
    // id either way, so neither is kept here.
    let _: String = c.query_async(conn).await?;
    Ok(())
}

/// Every in-flight job on this node plus the recently finished ones the
/// server still remembers, from whichever end `conn` is.
pub async fn cluster_get_slot_migrations<C: ConnectionLike + Send>(
    conn: &mut C,
    dialect: SlotMigrationDialect,
) -> Result<Vec<AtomicSlotMigration>> {
    let value: Value = match dialect {
        SlotMigrationDialect::Valkey => cmd("CLUSTER").arg("GETSLOTMIGRATIONS").query_async(conn).await?,
        SlotMigrationDialect::Redis => {
            cmd("CLUSTER")
                .arg("MIGRATION")
                .arg("STATUS")
                .arg("ALL")
                .query_async(conn)
                .await?
        }
    };
    let Value::Array(items) = value else {
        return Ok(Vec::new());
    };
    // A row this build cannot read is skipped, never fatal: the list is
    // diagnostic and one odd entry must not hide the others.
    Ok(items
        .iter()
        .filter_map(|row| match dialect {
            SlotMigrationDialect::Valkey => parse_migration(row),
            SlotMigrationDialect::Redis => parse_redis_migration(row),
        })
        .collect())
}

/// Abort every migration this node has a hand in. On Valkey only the source
/// can cancel; Redis takes the cancel on either end and answers with a
/// count, which is why `cluster_ops` tells both ends there.
pub async fn cluster_cancel_slot_migrations<C: ConnectionLike + Send>(
    conn: &mut C,
    dialect: SlotMigrationDialect,
) -> Result<()> {
    match dialect {
        SlotMigrationDialect::Valkey => {
            let _: String = cmd("CLUSTER").arg("CANCELSLOTMIGRATIONS").query_async(conn).await?;
        }
        SlotMigrationDialect::Redis => {
            let _: i64 = cmd("CLUSTER")
                .arg("MIGRATION")
                .arg("CANCEL")
                .arg("ALL")
                .query_async(conn)
                .await?;
        }
    }
    Ok(())
}

fn parse_migration(value: &Value) -> Option<AtomicSlotMigration> {
    let mut migration = AtomicSlotMigration::default();
    for (key, val) in extract_pairs(value)? {
        match key.as_str() {
            "name" => migration.name = text(&val),
            "operation" => migration.operation = text(&val),
            "slot_ranges" => migration.slot_ranges = text(&val),
            "target_node" => migration.target_node = text(&val),
            "source_node" => migration.source_node = text(&val),
            "create_time" => migration.create_time = number(&val) as i64,
            "last_update_time" => migration.last_update_time = number(&val) as i64,
            "last_ack_time" => migration.last_ack_time = number(&val) as i64,
            "state" => migration.state = text(&val),
            "message" => migration.message = text(&val),
            "cow_size" => migration.cow_size = number(&val),
            "remaining_repl_size" => migration.remaining_repl_size = number(&val),
            _ => {}
        }
    }
    Some(migration)
}

/// A `CLUSTER MIGRATION STATUS` row: `id`, `slots`, `source`, `dest`,
/// `operation` (`import` / `migrate`), `state`, `last_error`, `retries`,
/// `create_time` / `start_time` / `end_time` in milliseconds,
/// `write_pause_ms` — read into Valkey's fields.
fn parse_redis_migration(value: &Value) -> Option<AtomicSlotMigration> {
    let mut migration = AtomicSlotMigration::default();
    let (mut created, mut started, mut ended) = (0u64, 0u64, 0u64);
    for (key, val) in extract_pairs(value)? {
        match key.as_str() {
            "id" => migration.name = text(&val),
            "slots" => migration.slot_ranges = text(&val),
            "source" => migration.source_node = text(&val),
            "dest" => migration.target_node = text(&val),
            "operation" => {
                migration.operation = match text(&val).to_ascii_lowercase().as_str() {
                    "migrate" => "EXPORT".to_string(),
                    "import" => "IMPORT".to_string(),
                    other => other.to_ascii_uppercase(),
                }
            }
            "state" => migration.state = text(&val),
            "last_error" => migration.message = text(&val),
            "create_time" => created = number(&val),
            "start_time" => started = number(&val),
            "end_time" => ended = number(&val),
            _ => {}
        }
    }
    migration.create_time = (created / 1000) as i64;
    let updated = ended.max(started).max(created) / 1000;
    migration.last_update_time = updated as i64;
    migration.last_ack_time = updated as i64;
    Some(migration)
}

/// RESP2 delivers a map as a flat `[k, v, k, v, …]` array; RESP3 as a map.
fn extract_pairs(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Array(items) => {
            let mut pairs = Vec::with_capacity(items.len() / 2);
            for pair in items.chunks(2) {
                let [key, val] = pair else { return None };
                pairs.push((text(key), val.clone()));
            }
            Some(pairs)
        }
        Value::Map(items) => Some(items.iter().map(|(k, v)| (text(k), v.clone())).collect()),
        _ => None,
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => s.clone(),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Int(n) => n.to_string(),
        _ => String::new(),
    }
}

fn number(value: &Value) -> u64 {
    match value {
        Value::Int(n) => (*n).max(0) as u64,
        other => text(other).parse().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn a_migration_row_reads_every_field() {
        let row = Value::Array(vec![
            bulk("name"),
            bulk("2a1f".repeat(10).as_str()),
            bulk("operation"),
            bulk("EXPORT"),
            bulk("slot_ranges"),
            bulk("0-10 20-30"),
            bulk("target_node"),
            bulk("target-id"),
            bulk("source_node"),
            bulk("source-id"),
            bulk("create_time"),
            Value::Int(1_700_000_000),
            bulk("last_update_time"),
            Value::Int(1_700_000_005),
            bulk("last_ack_time"),
            Value::Int(1_700_000_004),
            bulk("state"),
            bulk("snapshotting"),
            bulk("message"),
            bulk(""),
            bulk("cow_size"),
            Value::Int(4096),
            bulk("remaining_repl_size"),
            Value::Int(2048),
        ]);
        let parsed = parse_migration(&row).expect("row parses");
        assert_eq!(parsed.operation, "EXPORT");
        assert_eq!(parsed.slot_ranges, "0-10 20-30");
        assert_eq!(parsed.source_node, "source-id");
        assert_eq!(parsed.cow_size, 4096);
        assert_eq!(parsed.remaining_repl_size, 2048);
        assert!(parsed.is_active(), "snapshotting is not a terminal state");
        assert!(parsed.is_export());
    }

    /// What `CLUSTER MIGRATION STATUS ALL` answered on Redis 8.10.2, seen
    /// from the source (`migrate`) — read as Valkey's `EXPORT`, with the
    /// millisecond times in seconds.
    #[test]
    fn a_redis_status_row_is_read_in_valkey_s_words() {
        let row = Value::Array(vec![
            bulk("id"),
            bulk("7003cd8d76840f0b4ee72ee7c7eb2a11aa79e225"),
            bulk("slots"),
            bulk("0-100"),
            bulk("source"),
            bulk("a6f810b8"),
            bulk("dest"),
            bulk("7c5ab260"),
            bulk("operation"),
            bulk("migrate"),
            bulk("state"),
            bulk("completed"),
            bulk("last_error"),
            bulk(""),
            bulk("retries"),
            Value::Int(0),
            bulk("create_time"),
            Value::Int(1_790_405_105_443),
            bulk("start_time"),
            Value::Int(1_790_405_105_443),
            bulk("end_time"),
            Value::Int(1_790_405_105_445),
            bulk("write_pause_ms"),
            Value::Int(2),
        ]);
        let parsed = parse_redis_migration(&row).expect("row parses");
        assert_eq!(parsed.name, "7003cd8d76840f0b4ee72ee7c7eb2a11aa79e225");
        assert_eq!(parsed.slot_ranges, "0-100");
        assert_eq!(parsed.source_node, "a6f810b8");
        assert_eq!(parsed.target_node, "7c5ab260");
        assert!(parsed.is_export(), "migrate is the source's side");
        assert!(!parsed.is_active(), "completed is done");
        assert_eq!(parsed.create_time, 1_790_405_105);
        assert_eq!(parsed.last_update_time, 1_790_405_105);

        let importing = Value::Array(vec![
            bulk("operation"),
            bulk("import"),
            bulk("state"),
            bulk("wait-stream-eof"),
        ]);
        let parsed = parse_redis_migration(&importing).expect("row parses");
        assert_eq!(parsed.operation, "IMPORT");
        assert!(parsed.is_active(), "a state on the way is still running");
    }

    #[test]
    fn only_the_documented_states_are_terminal() {
        let finished = |state: &str| AtomicSlotMigration {
            state: state.to_string(),
            ..Default::default()
        };
        for state in ["success", "completed", "failed", "cancelled", "canceled"] {
            assert!(!finished(state).is_active(), "{state}");
        }
        // A state a later server might add reads as still running, which
        // keeps it on screen instead of silently dropping it.
        for state in ["snapshotting", "streaming", "wait-stream-eof", "paused", ""] {
            assert!(finished(state).is_active(), "{state}");
        }
    }

    #[test]
    fn each_dialect_names_its_commands_and_the_end_it_starts_on() {
        assert_eq!(SlotMigrationDialect::for_flavor(true), SlotMigrationDialect::Valkey);
        assert_eq!(SlotMigrationDialect::for_flavor(false), SlotMigrationDialect::Redis);
        assert!(
            !SlotMigrationDialect::Valkey.starts_on_target(),
            "Valkey's source pushes"
        );
        assert!(SlotMigrationDialect::Redis.starts_on_target(), "Redis's target pulls");
        assert_eq!(SlotMigrationDialect::Redis.start_command(), "CLUSTER MIGRATION IMPORT");
        assert_eq!(
            SlotMigrationDialect::Valkey.cancel_command(),
            "CLUSTER CANCELSLOTMIGRATIONS"
        );
    }
}
