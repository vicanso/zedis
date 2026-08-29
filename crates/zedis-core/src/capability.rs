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

//! Connection capability matrix for read-only access modes.
//!
//! UI and server-state gates used to scatter `if !readonly` checks. That
//! made it easy for pure reads (folder refresh, bulk export, entry
//! preview) to disappear when a connection was locked. This module is
//! the single source of truth:
//!
//! - Every gated affordance maps to a [`Capability`] variant.
//! - [`Capability::requires_write`] classifies mutators vs. reads/local.
//! - [`allows`] / [`Capability::allowed`] answer "is this OK right now?".
//! - The table-driven tests lock the full matrix so a new variant without
//!   an expected row fails CI, and so regressions like "refresh vanished
//!   from the folder context menu" surface immediately.
//!
//! When adding a new button / menu item / server op:
//! 1. Add a variant (or reuse an existing one).
//! 2. Place it in the `requires_write` match.
//! 3. Extend `Capability::ALL` and the explicit matrix test.
//!
//! Not every variant is wired into a call site yet — the matrix is the
//! audit surface. That's fine lint-wise: this is a library crate, so the
//! pub matrix is public API and `dead_code` never fires on it.

use crate::features::{CommandStatus, ServerCommand, ServerFeatures};

/// UI / server operation that may be gated by connection access mode.
///
/// Grouped into *always-allowed-when-readonly* (reads, local metadata,
/// pure UI) and *denied-when-readonly* (anything that mutates Redis or
/// server-side state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    // ── Allowed in read-only (reads / local / pure UI) ───────────────
    /// Full key-tree re-scan (`SCAN` with current filter).
    RefreshKeys,
    /// Re-scan a single folder prefix.
    RefreshFolder,
    /// DUMP / framed binary export to a local file (Redis read + local write).
    ExportKeys,
    /// Export a single string value's bytes to a local file.
    ExportValue,
    /// Export the visible key list (name / type / TTL) as CSV.
    ExportCsv,
    /// Open / preview a container entry without mutating.
    ViewEntry,
    /// Side-by-side value diffs (history or cross-server) — pure reads.
    DiffValues,
    /// Load a history snapshot into the editor (still needs Save to push).
    LoadHistory,
    /// Local redb tag / note metadata (never touches Redis).
    EditLocalMetadata,
    /// Multi-select is a UI mode (enables bulk export among other things).
    ToggleMultiSelect,
    /// Collapse all expanded folders in the key tree.
    CollapseTree,
    /// Auto-refresh interval for the key tree.
    AutoRefresh,
    /// Search / type / tag filters on the key tree.
    SearchFilter,
    /// Copy key name / path / cell text to the system clipboard.
    CopyToClipboard,
    /// Observational panels: metrics, monitor, slowlog, config GET, ACL list,
    /// topology view, keyspace notifications, pub/sub subscribe, etc.
    Observe,
    /// Reload the currently selected key's value from Redis.
    ReloadValue,

    // ── Denied in read-only (mutate Redis / server state) ────────────
    /// Create a new key (`SET` / type-specific add).
    CreateKey,
    /// Delete one key (`DEL` / `UNLINK`).
    DeleteKey,
    /// Delete many selected keys.
    DeleteKeys,
    /// Delete every key under a folder prefix.
    DeleteFolder,
    /// `FLUSHDB` / `FLUSHALL` — wipe the current database or the whole
    /// instance. The bluntest write there is; never available read-only.
    FlushDatabase,
    /// Set / update TTL (`EXPIRE` / `PEXPIRE` / …).
    SetTtl,
    /// Remove TTL (`PERSIST`).
    PersistTtl,
    /// Persist an edited value to Redis (`SET` / `HSET` / `JSON.SET` / …).
    SaveValue,
    /// Import keys from a dump file (`RESTORE`).
    ImportKeys,
    /// Overwrite a string value from a local file.
    ImportValue,
    /// Rename a key (`RENAME` / `RENAMENX`).
    RenameKey,
    /// Cross-server copy (`DUMP` source + `RESTORE` target) — mutates target.
    CopyKeyToServer,
    /// Mutate a container field (hash / list / set / zset / stream / bitmap bit).
    MutateContainer,
    /// `CLIENT KILL`.
    KillClient,
    /// ACL create / edit / delete users.
    AclWrite,
    /// `CONFIG SET` / rewrite / resetstat.
    ConfigWrite,
    /// Cluster failover / forget / meet / replicate.
    ClusterWrite,
    /// Sentinel failover / reset / remove.
    SentinelWrite,
    /// `BGSAVE` / `BGREWRITEAOF`.
    PersistenceWrite,
    /// `PUBLISH` a pub/sub message.
    PublishMessage,
    /// `FUNCTION LOAD` / `FUNCTION DELETE`.
    FunctionWrite,
    /// Run a Lua script / EVAL (may have side effects — treated as write).
    EvalScript,
}

impl Capability {
    /// Every variant — must stay in sync with the enum. The matrix test
    /// asserts this list is exhaustive by checking length + uniqueness
    /// and by requiring an explicit `(cap, allowed_in_ro)` row for each.
    pub const ALL: &'static [Capability] = &[
        // reads / local
        Capability::RefreshKeys,
        Capability::RefreshFolder,
        Capability::ExportKeys,
        Capability::ExportValue,
        Capability::ExportCsv,
        Capability::ViewEntry,
        Capability::DiffValues,
        Capability::LoadHistory,
        Capability::EditLocalMetadata,
        Capability::ToggleMultiSelect,
        Capability::CollapseTree,
        Capability::AutoRefresh,
        Capability::SearchFilter,
        Capability::CopyToClipboard,
        Capability::Observe,
        Capability::ReloadValue,
        // writes
        Capability::CreateKey,
        Capability::DeleteKey,
        Capability::DeleteKeys,
        Capability::DeleteFolder,
        Capability::FlushDatabase,
        Capability::SetTtl,
        Capability::PersistTtl,
        Capability::SaveValue,
        Capability::ImportKeys,
        Capability::ImportValue,
        Capability::RenameKey,
        Capability::CopyKeyToServer,
        Capability::MutateContainer,
        Capability::KillClient,
        Capability::AclWrite,
        Capability::ConfigWrite,
        Capability::ClusterWrite,
        Capability::SentinelWrite,
        Capability::PersistenceWrite,
        Capability::PublishMessage,
        Capability::FunctionWrite,
        Capability::EvalScript,
    ];

    /// Whether this capability mutates Redis or server-side state.
    ///
    /// Local-only ops (tags, clipboard, multi-select) and pure reads
    /// (refresh, export, diff, observe) return `false`.
    pub const fn requires_write(self) -> bool {
        matches!(
            self,
            Capability::CreateKey
                | Capability::DeleteKey
                | Capability::DeleteKeys
                | Capability::DeleteFolder
                | Capability::FlushDatabase
                | Capability::SetTtl
                | Capability::PersistTtl
                | Capability::SaveValue
                | Capability::ImportKeys
                | Capability::ImportValue
                | Capability::RenameKey
                | Capability::CopyKeyToServer
                | Capability::MutateContainer
                | Capability::KillClient
                | Capability::AclWrite
                | Capability::ConfigWrite
                | Capability::ClusterWrite
                | Capability::SentinelWrite
                | Capability::PersistenceWrite
                | Capability::PublishMessage
                | Capability::FunctionWrite
                | Capability::EvalScript
        )
    }

    /// Allowed under the given access mode (`readonly == true` means
    /// SafeMode or StrictReadOnly).
    pub const fn allowed(self, readonly: bool) -> bool {
        !readonly || !self.requires_write()
    }

    /// The server commands this capability cannot work without — the second
    /// axis of the matrix (the first is read/write). Empty for capabilities
    /// built on commands every Redis-compatible server has (`GET`, `DEL`,
    /// `EXPIRE`, …) and for local-only ones. Keep this to the commands the
    /// probe actually checks (`ServerCommand`).
    pub const fn required_commands(self) -> &'static [ServerCommand] {
        match self {
            Capability::RefreshKeys
            | Capability::RefreshFolder
            | Capability::AutoRefresh
            | Capability::SearchFilter => &[ServerCommand::Scan],
            Capability::ExportKeys | Capability::CopyKeyToServer => &[ServerCommand::Dump],
            Capability::ImportKeys => &[ServerCommand::Restore],
            Capability::FlushDatabase => &[ServerCommand::FlushDb],
            Capability::KillClient => &[ServerCommand::ClientKill],
            Capability::AclWrite => &[ServerCommand::AclSetUser],
            Capability::ConfigWrite => &[ServerCommand::ConfigSet],
            Capability::PersistenceWrite => &[ServerCommand::Bgsave],
            Capability::PublishMessage => &[ServerCommand::Publish],
            Capability::FunctionWrite => &[ServerCommand::FunctionLoad],
            Capability::EvalScript => &[ServerCommand::Eval],
            _ => &[],
        }
    }

    /// The first required command the probe found unusable, with why —
    /// `None` when the server side is fine (or not probed yet).
    pub fn blocked_by(self, features: &ServerFeatures) -> Option<(ServerCommand, CommandStatus)> {
        features.first_unusable(self.required_commands())
    }

    /// Both axes at once: allowed under the access mode *and* every command
    /// it needs is usable on this server.
    pub fn available(self, readonly: bool, features: &ServerFeatures) -> bool {
        self.allowed(readonly) && self.blocked_by(features).is_none()
    }
}

/// Convenience wrapper: is `cap` allowed when the connection is/isn't readonly?
#[inline]
pub const fn allows(readonly: bool, cap: Capability) -> bool {
    cap.allowed(readonly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Explicit matrix: every capability + whether it must remain available
    /// when the connection is read-only. This is the regression net for
    /// "folder refresh disappeared in RO" style bugs.
    const MATRIX: &[(Capability, bool)] = &[
        // ── reads / local: true ──────────────────────────────────────
        (Capability::RefreshKeys, true),
        (Capability::RefreshFolder, true),
        (Capability::ExportKeys, true),
        (Capability::ExportValue, true),
        (Capability::ExportCsv, true),
        (Capability::ViewEntry, true),
        (Capability::DiffValues, true),
        (Capability::LoadHistory, true),
        (Capability::EditLocalMetadata, true),
        (Capability::ToggleMultiSelect, true),
        (Capability::CollapseTree, true),
        (Capability::AutoRefresh, true),
        (Capability::SearchFilter, true),
        (Capability::CopyToClipboard, true),
        (Capability::Observe, true),
        (Capability::ReloadValue, true),
        // ── writes: false ────────────────────────────────────────────
        (Capability::CreateKey, false),
        (Capability::DeleteKey, false),
        (Capability::DeleteKeys, false),
        (Capability::DeleteFolder, false),
        (Capability::FlushDatabase, false),
        (Capability::SetTtl, false),
        (Capability::PersistTtl, false),
        (Capability::SaveValue, false),
        (Capability::ImportKeys, false),
        (Capability::ImportValue, false),
        (Capability::RenameKey, false),
        (Capability::CopyKeyToServer, false),
        (Capability::MutateContainer, false),
        (Capability::KillClient, false),
        (Capability::AclWrite, false),
        (Capability::ConfigWrite, false),
        (Capability::ClusterWrite, false),
        (Capability::SentinelWrite, false),
        (Capability::PersistenceWrite, false),
        (Capability::PublishMessage, false),
        (Capability::FunctionWrite, false),
        (Capability::EvalScript, false),
    ];

    #[test]
    fn all_list_matches_matrix_and_is_unique() {
        assert_eq!(
            Capability::ALL.len(),
            MATRIX.len(),
            "Capability::ALL and MATRIX must cover the same set — update both when adding a variant"
        );
        let mut seen = HashSet::new();
        for cap in Capability::ALL {
            assert!(seen.insert(*cap), "duplicate in Capability::ALL: {cap:?}");
        }
        let mut matrix_seen = HashSet::new();
        for (cap, _) in MATRIX {
            assert!(matrix_seen.insert(*cap), "duplicate in MATRIX: {cap:?}");
            assert!(
                Capability::ALL.contains(cap),
                "{cap:?} is in MATRIX but missing from Capability::ALL"
            );
        }
        for cap in Capability::ALL {
            assert!(
                matrix_seen.contains(cap),
                "{cap:?} is in Capability::ALL but missing from MATRIX"
            );
        }
    }

    #[test]
    fn readonly_matrix_matches_allows() {
        for &(cap, allowed_in_ro) in MATRIX {
            assert_eq!(
                allows(true, cap),
                allowed_in_ro,
                "{cap:?}: expected allowed_in_readonly={allowed_in_ro}"
            );
            assert!(allows(false, cap), "{cap:?}: must always be allowed when not readonly");
            assert_eq!(
                cap.requires_write(),
                !allowed_in_ro,
                "{cap:?}: requires_write must be the inverse of allowed_in_readonly"
            );
        }
    }

    #[test]
    fn required_commands_only_name_probed_commands_and_gate_availability() {
        // Every required command must be one the probe checks.
        for cap in Capability::ALL {
            for c in cap.required_commands() {
                assert!(ServerCommand::ALL.contains(c), "{cap:?} requires unprobed {c:?}");
            }
        }
        // Un-probed: nothing is blocked, the read/write axis alone decides.
        let fresh = ServerFeatures::default();
        for cap in Capability::ALL {
            assert!(cap.available(false, &fresh), "{cap:?}");
            assert_eq!(cap.available(true, &fresh), cap.allowed(true), "{cap:?}");
        }
        // A denied CONFIG SET blocks config writes and nothing else.
        let mut features = ServerFeatures::probed_empty();
        features.set(ServerCommand::ConfigSet, CommandStatus::Denied);
        assert_eq!(
            Capability::ConfigWrite.blocked_by(&features),
            Some((ServerCommand::ConfigSet, CommandStatus::Denied))
        );
        assert!(!Capability::ConfigWrite.available(false, &features));
        assert!(Capability::KillClient.available(false, &features));
        // A missing SCAN takes the key-tree refreshes with it, even read-only.
        features.set(ServerCommand::Scan, CommandStatus::Missing);
        assert!(!Capability::RefreshKeys.available(true, &features));
        assert!(!Capability::RefreshFolder.available(false, &features));
        assert!(Capability::ReloadValue.available(true, &features));
    }

    #[test]
    fn folder_refresh_stays_available_in_readonly() {
        // Named regression for the key-tree folder context-menu bug.
        assert!(allows(true, Capability::RefreshFolder));
        assert!(allows(true, Capability::RefreshKeys));
        assert!(allows(true, Capability::ExportKeys));
        assert!(!allows(true, Capability::DeleteFolder));
        assert!(!allows(true, Capability::SetTtl));
        assert!(!allows(true, Capability::PersistTtl));
        assert!(!allows(true, Capability::ImportKeys));
    }

    #[test]
    fn multi_select_and_local_metadata_allowed_in_readonly() {
        // Multi-select enables bulk export; tags live in local redb.
        assert!(allows(true, Capability::ToggleMultiSelect));
        assert!(allows(true, Capability::EditLocalMetadata));
        assert!(allows(true, Capability::ViewEntry));
        assert!(allows(true, Capability::DiffValues));
    }

    #[test]
    fn write_ops_blocked_only_when_readonly() {
        let writes = [
            Capability::CreateKey,
            Capability::DeleteKey,
            Capability::SaveValue,
            Capability::ClusterWrite,
            Capability::PersistenceWrite,
            Capability::EvalScript,
        ];
        for cap in writes {
            assert!(!allows(true, cap), "{cap:?}");
            assert!(allows(false, cap), "{cap:?}");
        }
    }
}
