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

//! Atomic slot migration (Valkey 9.0): `CLUSTER MIGRATESLOTS`,
//! `CLUSTER GETSLOTMIGRATIONS` and `CLUSTER CANCELSLOTMIGRATIONS`.
//!
//! The legacy reshard the app drives itself — `SETSLOT MIGRATING` /
//! `IMPORTING`, a `GETKEYSINSLOT` + `MIGRATE` loop per slot, then
//! `SETSLOT NODE` fanned out — moves keys one batch at a time and leaves a
//! slot marked on both ends if anything interrupts it. Valkey 9 moves whole
//! slots server-side in the AOF format instead: one command starts the job,
//! the server owns it, and closing the app cannot strand a slot.
//!
//! All three commands are sent to the **source** node. The legacy path is
//! still there on Valkey 9, so this is a second route gated by
//! [`floors::ATOMIC_SLOT_MIGRATION`](crate::floors::ATOMIC_SLOT_MIGRATION),
//! not a replacement.

use crate::error::Error;
use redis::aio::ConnectionLike;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One row of `CLUSTER GETSLOTMIGRATIONS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtomicSlotMigration {
    /// The migration's 40-byte name, and what `CANCELSLOTMIGRATIONS`
    /// reports against.
    pub name: String,
    /// `EXPORT` on the source node, `IMPORT` on the target — the same job
    /// seen from either end.
    pub operation: String,
    /// The slot ranges as the server prints them (`"0-10 20-30"`).
    pub slot_ranges: String,
    pub target_node: String,
    pub source_node: String,
    /// Unix seconds.
    pub create_time: i64,
    pub last_update_time: i64,
    pub last_ack_time: i64,
    /// `success`, `failed` and `cancelled` are terminal; anything else is
    /// a migration still running.
    pub state: String,
    /// The server's explanation when a migration failed.
    pub message: String,
    /// Copy-on-write memory the fork is holding, in bytes.
    pub cow_size: u64,
    /// Bytes still to send.
    pub remaining_repl_size: u64,
}

impl AtomicSlotMigration {
    /// Still running. The terminal states are named in the command's
    /// documentation; treating an unknown state as active is deliberate,
    /// so a state added later shows up as in-flight rather than finished.
    pub fn is_active(&self) -> bool {
        !matches!(self.state.as_str(), "success" | "failed" | "cancelled")
    }

    /// The source's side of the job — the end that can cancel it.
    pub fn is_export(&self) -> bool {
        self.operation.eq_ignore_ascii_case("EXPORT")
    }
}

/// `CLUSTER MIGRATESLOTS SLOTSRANGE <start> <end> … NODE <target-id>` —
/// hand `ranges` to `target_id`. Sent to the **source** node, which owns
/// the job from then on. Returns as soon as the server accepted it; the
/// migration itself is watched through [`cluster_get_slot_migrations`].
pub async fn cluster_migrate_slots<C: ConnectionLike + Send>(
    conn: &mut C,
    ranges: &[(u16, u16)],
    target_id: &str,
) -> Result<()> {
    if ranges.is_empty() {
        return Err(Error::Invalid {
            message: "no slot ranges to migrate".to_string(),
        });
    }
    let mut c = cmd("CLUSTER");
    c.arg("MIGRATESLOTS").arg("SLOTSRANGE");
    for (start, end) in ranges {
        c.arg(*start).arg(*end);
    }
    c.arg("NODE").arg(target_id);
    let _: String = c.query_async(conn).await?;
    Ok(())
}

/// `CLUSTER GETSLOTMIGRATIONS` — every in-flight job on this node plus the
/// recently finished ones the server still remembers.
pub async fn cluster_get_slot_migrations<C: ConnectionLike + Send>(conn: &mut C) -> Result<Vec<AtomicSlotMigration>> {
    let value: Value = cmd("CLUSTER").arg("GETSLOTMIGRATIONS").query_async(conn).await?;
    let Value::Array(items) = value else {
        return Ok(Vec::new());
    };
    // A row this build cannot read is skipped, never fatal: the list is
    // diagnostic and one odd entry must not hide the others.
    Ok(items.iter().filter_map(parse_migration).collect())
}

/// `CLUSTER CANCELSLOTMIGRATIONS` — abort every migration this node
/// started. Only the source can cancel; the target has to be told through
/// its own source.
pub async fn cluster_cancel_slot_migrations<C: ConnectionLike + Send>(conn: &mut C) -> Result<()> {
    let _: String = cmd("CLUSTER").arg("CANCELSLOTMIGRATIONS").query_async(conn).await?;
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

    #[test]
    fn only_the_documented_states_are_terminal() {
        let finished = |state: &str| AtomicSlotMigration {
            state: state.to_string(),
            ..Default::default()
        };
        for state in ["success", "failed", "cancelled"] {
            assert!(!finished(state).is_active(), "{state}");
        }
        // A state a later Valkey might add reads as still running, which
        // keeps it on screen instead of silently dropping it.
        for state in ["snapshotting", "streaming", "paused", ""] {
            assert!(finished(state).is_active(), "{state}");
        }
    }
}
