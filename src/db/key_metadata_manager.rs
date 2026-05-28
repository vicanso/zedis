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

//! Per-server client-side key tags + free-form notes.
//!
//! Data lives entirely in the local redb file — Redis itself stays
//! untouched (zero storage cost, zero network round-trips). Persistence
//! shape is a versioned JSON envelope per server:
//!
//! ```json
//! {"v": 1, "entries": {"user:42": {"tag": "red", "note": "…"}}}
//! ```
//!
//! Versioning at the document level so future shape changes (multi-tag,
//! per-tag notes, …) can migrate in place without breaking older clients
//! reading the same on-disk file.
//!
//! Caching strategy mirrors `HistoryManager`: a `DashMap<server_id,
//! HashMap<key, KeyMetadata>>` fronts every read. Manual annotations
//! are typically dozens-to-hundreds per instance, so loading a whole
//! server's metadata in one go is comfortably bounded.

use super::{KEY_METADATA_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use redb::ReadableDatabase;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Pre-set tag colour. Six options keep the chip row compact and avoid
/// a colour-picker rabbit hole. Each variant maps onto the active theme
/// at render time so dark/light themes look intentional rather than
/// blasting saturated CSS colours through both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl TagColor {
    /// All variants in display order. Used to build the swatch row and
    /// the filter dropdown without hand-listing them at every site.
    pub const ALL: [TagColor; 6] = [
        TagColor::Red,
        TagColor::Orange,
        TagColor::Yellow,
        TagColor::Green,
        TagColor::Blue,
        TagColor::Purple,
    ];

    /// Stable lower-case identifier — matches the on-disk JSON value
    /// and the i18n key suffix (e.g. `key_tag.color_red`).
    pub fn as_str(self) -> &'static str {
        match self {
            TagColor::Red => "red",
            TagColor::Orange => "orange",
            TagColor::Yellow => "yellow",
            TagColor::Green => "green",
            TagColor::Blue => "blue",
            TagColor::Purple => "purple",
        }
    }

    /// Round-trip helper used by tests + filter chip restoration.
    pub fn from_str(s: &str) -> Option<TagColor> {
        match s {
            "red" => Some(TagColor::Red),
            "orange" => Some(TagColor::Orange),
            "yellow" => Some(TagColor::Yellow),
            "green" => Some(TagColor::Green),
            "blue" => Some(TagColor::Blue),
            "purple" => Some(TagColor::Purple),
            _ => None,
        }
    }
}

/// One stored annotation. Both fields are optional from the user's POV
/// (tag-only, note-only, and both are all valid states); the manager
/// treats an entry with `tag == None && note.is_empty()` as equivalent
/// to "no record" and deletes it on save.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeyMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<TagColor>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl KeyMetadata {
    pub fn is_empty(&self) -> bool {
        self.tag.is_none() && self.note.is_empty()
    }
}

/// On-disk envelope. Versioned to keep future schema migrations local
/// — we can bump `v` and add a new shape without changing the table
/// definition or stranding existing users on an unreadable file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEnvelope {
    #[serde(default = "envelope_default_version")]
    v: u32,
    #[serde(default)]
    entries: HashMap<String, KeyMetadata>,
}

fn envelope_default_version() -> u32 {
    1
}

const ENVELOPE_VERSION: u32 = 1;

pub struct KeyMetadataManager {
    /// Per-server cache: `server_id → (key → metadata)`. Holds the
    /// authoritative in-memory copy after the first read; subsequent
    /// `set`/`clear` calls mutate it directly and persist the same way
    /// the favorites manager does.
    cache: DashMap<String, HashMap<String, KeyMetadata>>,
}

static KEY_METADATA_MANAGER: LazyLock<KeyMetadataManager> = LazyLock::new(KeyMetadataManager::new);

pub fn get_key_metadata_manager() -> &'static KeyMetadataManager {
    &KEY_METADATA_MANAGER
}

impl KeyMetadataManager {
    fn new() -> Self {
        Self { cache: DashMap::new() }
    }

    /// Materialise the full record map for a server, hitting the cache
    /// on subsequent calls. Returns an owned `HashMap` so callers don't
    /// hold a DashMap guard across UI render work (which can re-enter
    /// the manager via the editor / tree event loop).
    pub fn records(&self, server_id: &str) -> Result<HashMap<String, KeyMetadata>> {
        if let Some(cached) = self.cache.get(server_id) {
            return Ok(cached.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(KEY_METADATA_TABLE)?;
        let Some(v) = table.get(server_id)? else {
            self.cache.insert(server_id.to_string(), HashMap::new());
            return Ok(HashMap::new());
        };
        let envelope: StoredEnvelope = serde_json::from_str(v.value())?;
        // Older shapes would bump v and run a migration here; for now
        // we only know v=1 and fall back to an empty map otherwise.
        let entries = if envelope.v == ENVELOPE_VERSION {
            envelope.entries
        } else {
            HashMap::new()
        };
        self.cache.insert(server_id.to_string(), entries.clone());
        Ok(entries)
    }

    /// Fetch one key's annotation. `None` means "no record" — distinct
    /// from `Some(KeyMetadata::default())` which would be a record that
    /// just happens to carry no tag and no note (which we don't persist;
    /// see `set`).
    pub fn get(&self, server_id: &str, key: &str) -> Result<Option<KeyMetadata>> {
        let records = self.records(server_id)?;
        Ok(records.get(key).cloned())
    }

    /// Persist a metadata record. Storing an empty record (no tag, no
    /// note) is treated as a delete so the table doesn't accumulate
    /// dead entries when the user clears both fields in the dialog.
    pub fn set(&self, server_id: &str, key: &str, metadata: KeyMetadata) -> Result<()> {
        if metadata.is_empty() {
            return self.clear(server_id, key);
        }
        // Hydrate the cache first so the write transaction below sees
        // the full set, not just the delta we're about to apply.
        let _ = self.records(server_id)?;

        let db = get_database()?;
        let write_txn = db.begin_write()?;
        let entries_to_persist = {
            let mut entries = self.cache.entry(server_id.to_string()).or_default();
            entries.insert(key.to_string(), metadata);
            entries.clone()
        };
        {
            let mut table = write_txn.open_table(KEY_METADATA_TABLE)?;
            let envelope = StoredEnvelope {
                v: ENVELOPE_VERSION,
                entries: entries_to_persist,
            };
            let json_val = serde_json::to_string(&envelope)?;
            table.insert(server_id, json_val.as_str())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete a single key's record. No-op if there was nothing
    /// recorded — callers can always invoke this defensively.
    pub fn clear(&self, server_id: &str, key: &str) -> Result<()> {
        // Hydrate first so we can detect "nothing to do" without
        // racing the disk.
        let _ = self.records(server_id)?;
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        let entries_to_persist = {
            let mut entries = self.cache.entry(server_id.to_string()).or_default();
            if entries.remove(key).is_none() {
                return Ok(());
            }
            entries.clone()
        };
        {
            let mut table = write_txn.open_table(KEY_METADATA_TABLE)?;
            if entries_to_persist.is_empty() {
                // Empty envelope is wasteful — drop the row entirely so
                // future reads short-circuit at the table lookup.
                table.remove(server_id)?;
            } else {
                let envelope = StoredEnvelope {
                    v: ENVELOPE_VERSION,
                    entries: entries_to_persist,
                };
                let json_val = serde_json::to_string(&envelope)?;
                table.insert(server_id, json_val.as_str())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// `true` when the server has at least one record — drives the
    /// "show filter chip row" decision so we don't litter the toolbar
    /// when nothing is tagged yet.
    pub fn has_any_records(&self, server_id: &str) -> bool {
        self.records(server_id).map(|m| !m.is_empty()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyMetadata, StoredEnvelope, TagColor};

    #[test]
    fn tag_color_round_trips_through_string() {
        for c in TagColor::ALL {
            assert_eq!(TagColor::from_str(c.as_str()), Some(c));
        }
        assert_eq!(TagColor::from_str("turquoise"), None);
    }

    #[test]
    fn key_metadata_empty_detects_both_blanks() {
        assert!(KeyMetadata::default().is_empty());
        assert!(
            !KeyMetadata {
                tag: Some(TagColor::Red),
                note: String::new(),
            }
            .is_empty()
        );
        assert!(
            !KeyMetadata {
                tag: None,
                note: "x".into(),
            }
            .is_empty()
        );
    }

    #[test]
    fn envelope_serde_keeps_version_and_entries() {
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "user:42".to_string(),
            KeyMetadata {
                tag: Some(TagColor::Red),
                note: "hot cache key".into(),
            },
        );
        let envelope = StoredEnvelope { v: 1, entries };
        let json = serde_json::to_string(&envelope).expect("serialize");
        // Spot-check the on-disk shape so a future serde-derive tweak
        // doesn't silently bump the schema without a migration bump.
        assert!(json.contains("\"v\":1"));
        assert!(json.contains("\"tag\":\"red\""));
        assert!(json.contains("\"note\":\"hot cache key\""));

        let parsed: StoredEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.v, 1);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries.get("user:42").and_then(|m| m.tag), Some(TagColor::Red));
    }

    #[test]
    fn envelope_serde_drops_default_fields() {
        // tag=None and note="" should not appear in the serialized JSON,
        // so a record that's been "cleared down to nothing" round-trips
        // through serde as `{}` — flagged by `is_empty` to drop the row.
        let envelope = StoredEnvelope {
            v: 1,
            entries: std::collections::HashMap::from([("k".to_string(), KeyMetadata::default())]),
        };
        let json = serde_json::to_string(&envelope).expect("serialize");
        assert!(!json.contains("\"tag\""));
        assert!(!json.contains("\"note\""));
    }
}
