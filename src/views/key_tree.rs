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

use crate::{
    assets::CustomIconName,
    components::KeyTypeBadge,
    connection::get_server,
    constants::KEY_TREE_KEYWORD_INPUT_HEIGHT,
    db::{KeyMetadata, TagColor, get_favorites_manager, get_key_metadata_manager, get_search_history_manager},
    helpers::{
        EditorAction, build_csv, format_ttl_chip, get_mono_font_family, humanize_keystroke, parse_duration,
        theme_color_for_tag, ttl_chip_kind, validate_long_string, validate_ttl,
    },
    states::{
        KeyType, KeyTypeFilter, QueryMode, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props,
        escalate_dangerous_body, get_session_option, i18n_common, i18n_key_tag, i18n_key_tree, save_session_option,
    },
    views::{
        OnTagDialogDone, export_to_file, open_key_tag_dialog, open_migration_export_window,
        open_migration_import_window,
    },
};
use ahash::{AHashMap, AHashSet};
use gpui::{
    Action, Anchor, App, AppContext, Entity, FocusHandle, Focusable, Hsla, ScrollStrategy, SharedString, Subscription,
    Task, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::ContextMenuExt,
    spinner::Spinner,
    v_flex,
};
use gpui_component::{
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::DropdownMenu,
};
use rust_i18n::t;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::{str::FromStr, sync::Arc, time::Duration};
use tracing::info;
use zedis_ui::{ZedisDialog, ZedisFormField, ZedisFormFieldType, ZedisFormOptions, ZedisSkeletonLoading};

// Constants for tree layout and behavior
const TREE_INDENT_BASE: f32 = 16.0; // Base indentation per level in pixels
const TREE_INDENT_OFFSET: f32 = 8.0; // Additional offset for all items
const EXPANDED_ITEMS_INITIAL_CAPACITY: usize = 10;
/// Fixed width of the TTL chip, in pixels. Sized to fit the two-digit cap
/// of `format_ttl_chip` (`59s` / `59m`) at 10px font with 1px borders.
const TTL_CHIP_WIDTH: f32 = 34.0;
/// Width of the hover-only inline delete button slot, in pixels. The button is
/// absolutely positioned in this fixed slot at the row's right edge (out of
/// flow → no reserved width); the TTL chip hides on hover so they don't overlap.
const INLINE_DELETE_WIDTH: f32 = 28.0;
/// Fixed width of the leaf type-badge column, in pixels. Holds the compact
/// type codes (`STR` / `STRM` / `ZSET`, max 4 chars at 10px) so the key names
/// line up in a column regardless of their type (matches the design).
const TYPE_BADGE_COL_WIDTH: f32 = 36.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
enum KeyTreeAction {
    Search(SharedString),
    Clear,
    DeleteMultipleKeys,
    DeleteKey(SharedString),
    DeleteFolder(SharedString),
    /// Batch TTL on the multi-selection / a folder prefix. `SetTtl*` open a
    /// TTL-input dialog (`EXPIRE`); `Persist*` confirm then `PERSIST`.
    SetTtlMultipleKeys,
    PersistMultipleKeys,
    SetTtlFolder(SharedString),
    PersistFolder(SharedString),
    RefreshFolder(SharedString),
    CollapseAllKeys,
    /// Export the current key list (name / type / TTL) to a CSV file.
    ExportCsv,
    ToggleMultiSelectMode,
    ChangeChannelMode,
    AutoRefresh(u32),
    SelectFavoriteKey(SharedString),
    ClearFavorites,
    ExportSelectedKeys,
    ExportFolder(SharedString),
    ExportKey(SharedString),
    ImportFromFile,
    /// Manual full refresh: re-scan with the current keyword/mode.
    RefreshAll,
    /// Open the key tag & note dialog for the given key (carried in
    /// the variant payload because the right-click site dispatches
    /// against the row's key, not the currently-selected one in the
    /// editor — they can differ when the user right-clicks a row
    /// other than the active selection).
    EditKeyTag(SharedString),
    /// Set (or clear) the tag colour filter applied to the visible
    /// tree. Empty string means "clear" — distinct from any TagColor
    /// variant so the dispatch arm has a single match path. The
    /// payload is a `SharedString` not a `TagColor` because gpui
    /// actions need `JsonSchema` and the colour enum sits in the
    /// `db` layer, so we keep the wire format string-based.
    SetTagFilter(SharedString),
}

#[derive(Default)]
struct KeyTreeState {
    /// current keyword
    keyword: SharedString,
    server_id: SharedString,
    /// Unique ID for the current key tree (changes when keys are reloaded)
    key_tree_id: SharedString,
    /// Cached key tree ID — tracks which key_tree_id the cached_keys and
    /// cached_key_ttls correspond to.
    cached_key_tree_id: SharedString,
    /// Cached sorted keys snapshot to avoid re-cloning from server state on every rebuild
    cached_keys: Arc<Vec<(SharedString, KeyType)>>,
    /// Snapshot of `ZedisServerState::key_ttls`, refreshed in lockstep with
    /// `cached_keys`. Used to color leaf rows in the tree.
    cached_key_ttls: Arc<AHashMap<SharedString, i64>>,
    /// Whether the tree is empty (no keys found)
    is_empty: bool,
    /// Current query mode (All/Prefix/Exact)
    query_mode: QueryMode,
    /// Error message to display if key loading fails
    error: Option<SharedString>,
    /// Set of expanded folder paths (persisted during tree rebuilds)
    expanded_items: AHashSet<SharedString>,
    /// Folders the user explicitly collapsed. The single-child
    /// auto-expand (`single_child_expanded_set`) skips these so a
    /// collapse sticks instead of being reopened on the next rebuild.
    /// Cleared for a folder when it's expanded again, and on
    /// collapse-all / a new scan.
    suppressed_auto_expand: AHashSet<SharedString>,
    /// Index path to scroll to when the tree is updated
    scroll_to_index: Option<IndexPath>,
    /// Whether to clear the list selection on the next render (requires window)
    clear_selection: bool,
    /// Refresh interval in seconds
    refresh_interval_sec: u32,
    /// (keyword, query_mode) the displayed tree was last scanned for.
    /// Used to tell a refresh (same target) apart from a new query.
    last_scan: Option<(SharedString, QueryMode)>,
    /// Set by `handle_filter` for the duration of a same-target
    /// refresh. Read (not consumed) by both collapse paths — the
    /// `KeyScanReset` guard and the transient-empty branch in
    /// `update_key_tree` — and cleared on `KeyScanFinished`.
    preserve_expand_on_scan: bool,
    /// Single-colour tag filter applied to the visible tree. `None`
    /// shows all rows; `Some(color)` keeps only leaves with that tag
    /// (folders stay so the path remains navigable even when the
    /// folder's own descendants are filtered out).
    selected_tag_filter: Option<TagColor>,
}

#[derive(Default, Debug, Clone)]
struct KeyTreeItem {
    id: SharedString,
    label: SharedString,
    depth: usize,
    key_type: KeyType,
    expanded: bool,
    children_count: usize,
    is_folder: bool,
    /// Zebra stripe flag for leaf rows: true on every second leaf within a
    /// parent folder (2nd, 4th, …) so the key list reads as banded rows. The
    /// count restarts under each folder; always false for folders / synthetic
    /// rows.
    stripe: bool,
    /// Remaining TTL in seconds for leaf items (`-1` = no expiry, `-2`
    /// = unknown/missing). `None` for folder nodes — folders don't have
    /// a meaningful aggregate TTL at the tree level.
    ttl_secs: Option<i64>,
    /// Client-side tag colour, pre-resolved from
    /// `KeyMetadataManager::records` at tree-build time so render_item
    /// stays O(1) per row. Folders never carry a tag (aggregate tags
    /// would be a separate v2 feature).
    tag: Option<TagColor>,
    /// Free-form note. Empty when there's no annotation. Rendered as a
    /// hover tooltip on the row label so it doesn't steal layout
    /// space from the type badge / TTL chip.
    note: SharedString,
    /// True while this folder's lazy `scan_prefix` is still running, so
    /// `render_item` can show an inline spinner. Always false for leaves.
    is_scanning: bool,
    /// When set, this row is a synthetic "Load more" affordance for the
    /// given (incomplete) folder prefix — clicking it resumes the scan.
    /// `None` for real key / folder rows.
    load_more_prefix: Option<SharedString>,
}

/// When a tag-colour filter is active, derive the input key list
/// **directly from local metadata** rather than from the SCAN snapshot.
/// SCAN is paginated and bounded — until it completes, the snapshot is
/// a strict subset of the server's keyspace, so filtering after the
/// scan would silently hide tagged keys that haven't been scanned yet.
///
/// For each tagged key we try to recover its `KeyType` from the SCAN
/// snapshot (constant-time lookup via a `name → type` index built
/// here). Keys outside the snapshot fall back to `KeyType::Unknown` —
/// the row still renders (with a neutral icon) and a click opens the
/// editor, which resolves the type lazily. Keys that have been deleted
/// on the server but still carry local metadata also show up this way;
/// that's intentional, since the loud "this key is gone" feedback
/// helps the user spot dangling annotations.
fn build_tagged_keys_list(
    color: TagColor,
    snapshot_keys: &[(SharedString, KeyType)],
    metadata: &std::collections::HashMap<String, KeyMetadata>,
) -> Vec<(SharedString, KeyType)> {
    // O(1) type lookup — `metadata` may contain hundreds of entries on
    // a heavily-annotated server, so a linear scan per entry would be
    // wasteful even if the snapshot is small.
    let type_by_key: std::collections::HashMap<&str, KeyType> =
        snapshot_keys.iter().map(|(k, t)| (k.as_ref(), *t)).collect();
    metadata
        .iter()
        .filter(|(_, m)| m.tag == Some(color))
        .map(|(key, _)| {
            let key_type = type_by_key.get(key.as_str()).copied().unwrap_or(KeyType::Unknown);
            (SharedString::from(key.clone()), key_type)
        })
        .collect()
}

/// Expands the user's `expanded_items` through single-child folder chains:
/// while an expanded folder's only child is itself a folder, that child is
/// treated as expanded too. Lets a deep single-child namespace
/// (`app:user` → `profile` → leaves) open in one click instead of one click
/// per level. Returns the augmented set (owned, so the caller can borrow
/// `&str` views into it). Recomputed every rebuild, so a streaming scan that
/// later reveals a second child stops the auto-expand at that level on the
/// next pass. No-op (skips the child-map pass) when nothing is expanded.
fn single_child_expanded_set(
    keys: &[(SharedString, KeyType)],
    expanded_items: &AHashSet<SharedString>,
    suppressed: &AHashSet<SharedString>,
    keyword: &str,
    separator: &str,
    max_depth: usize,
) -> AHashSet<String> {
    let mut effective: AHashSet<String> = expanded_items.iter().map(|s| s.to_string()).collect();
    if effective.is_empty() {
        return effective;
    }
    // For each folder prefix: (sole-child id, whether that child is itself a
    // folder, whether more than one distinct child was seen). Tracking just
    // the first child plus a "multiple" flag avoids a per-folder child set.
    let mut child_info: AHashMap<String, (String, bool, bool)> = AHashMap::new();
    for (key, _) in keys {
        if !keyword.is_empty() && !key.contains(keyword) {
            continue;
        }
        if !key.contains(separator) {
            continue;
        }
        let segs: Vec<&str> = key.splitn(max_depth, separator).collect();
        let mut dir = String::new();
        for (i, seg) in segs.iter().enumerate() {
            let parent = dir.clone();
            if i > 0 {
                dir.push_str(separator);
            }
            dir.push_str(seg);
            let child_is_folder = i + 1 < segs.len();
            match child_info.entry(parent) {
                Vacant(e) => {
                    e.insert((dir.clone(), child_is_folder, false));
                }
                Occupied(mut e) => {
                    let info = e.get_mut();
                    if info.0 == dir {
                        info.1 |= child_is_folder;
                    } else {
                        info.2 = true;
                    }
                }
            }
        }
    }
    // Follow single-folder-child links transitively from each expanded folder,
    // but never auto-open a folder the user explicitly collapsed.
    let suppressed_set: AHashSet<String> = suppressed.iter().map(|s| s.to_string()).collect();
    let mut stack: Vec<String> = effective.iter().cloned().collect();
    while let Some(dir) = stack.pop() {
        let Some((child, child_is_folder, multiple)) = child_info.get(dir.as_str()) else {
            continue;
        };
        if *multiple || !*child_is_folder || suppressed_set.contains(child) {
            continue;
        }
        if effective.insert(child.clone()) {
            stack.push(child.clone());
        }
    }
    effective
}

// Eight distinct concerns: input keys, keyword filter, expansion state,
// separator, depth cap, TTL map, metadata map, and tag filter. Bundling
// them into a struct would add more boilerplate than clarity since each
// caller already constructs them in different scopes.
#[allow(clippy::too_many_arguments)]
fn new_key_tree_items(
    mut keys: Vec<(SharedString, KeyType)>,
    keyword: SharedString,
    expanded_items: AHashSet<SharedString>,
    suppressed: AHashSet<SharedString>,
    separator: &str,
    max_key_tree_depth: usize,
    key_ttls: &AHashMap<SharedString, i64>,
    // Pre-loaded client-side annotations for the current server.
    // Looked up by exact key name when building leaf items so each
    // row carries its own tag/note copy and `render_item` doesn't
    // have to touch the manager per frame. Empty map is fine — no
    // metadata simply means no badges.
    metadata: &std::collections::HashMap<String, KeyMetadata>,
    // Optional single-colour filter — when set, keys not carrying
    // that exact tag are excluded from the resulting tree.
    tag_filter: Option<TagColor>,
) -> Vec<KeyTreeItem> {
    keys.sort_unstable_by_key(|(k, _)| k.clone());
    // Effective expansion = the user-expanded folders plus any single-child
    // folder chains hanging off them, so drilling into a deep single-child
    // namespace (`app:user` → `profile` → leaves) opens straight through in
    // one click instead of one click per level.
    let effective_expanded = single_child_expanded_set(
        &keys,
        &expanded_items,
        &suppressed,
        &keyword,
        separator,
        max_key_tree_depth,
    );
    let expanded_items_set = effective_expanded
        .iter()
        .map(|s| s.as_str())
        .collect::<AHashSet<&str>>();
    let mut items: AHashMap<SharedString, KeyTreeItem> = AHashMap::with_capacity(100);
    // Tracks standalone keys whose HashMap slot was taken over by a folder
    // with the same name (e.g. key "test" exists alongside "test:key1").
    // These are re-inserted as **siblings** of the folder so both remain
    // visible at the same tree level.
    let mut promoted_leaves: Vec<(SharedString, KeyType, SharedString, usize)> = Vec::new();

    for (key, key_type) in keys {
        if !keyword.is_empty() && !key.contains(keyword.as_str()) {
            continue;
        }
        let ttl_for_leaf = key_ttls.get(&key).copied();
        let (tag_for_leaf, note_for_leaf) = match metadata.get(key.as_ref()) {
            Some(m) => (m.tag, SharedString::from(m.note.clone())),
            None => (None, SharedString::default()),
        };
        // Single-color tag filter — drop the key entirely (along with
        // any synthesised parent folders) when it doesn't carry the
        // selected colour. Folders that end up empty just disappear
        // along with their leaves, which is correct: no need to keep
        // a heading with zero matching descendants.
        if let Some(filter) = tag_filter
            && tag_for_leaf != Some(filter)
        {
            continue;
        }
        if !key.contains(separator) {
            items.insert(
                key.clone(),
                KeyTreeItem {
                    id: key.clone(),
                    label: key.clone(),
                    key_type,
                    ttl_secs: ttl_for_leaf,
                    tag: tag_for_leaf,
                    note: note_for_leaf,
                    ..Default::default()
                },
            );
            continue;
        }

        let mut dir = String::with_capacity(50);
        let mut key_tree_item: Option<KeyTreeItem> = None;
        for (index, k) in key.splitn(max_key_tree_depth, separator).enumerate() {
            let expanded = index == 0 || expanded_items_set.contains(dir.as_str());
            if let Some(pending) = key_tree_item.take() {
                match items.entry(pending.id.clone()) {
                    Occupied(mut e) => {
                        let existing = e.get_mut();
                        if !existing.is_folder {
                            promoted_leaves.push((
                                existing.id.clone(),
                                existing.key_type,
                                existing.label.clone(),
                                existing.depth,
                            ));
                        }
                        existing.is_folder = true;
                        existing.children_count += 1;
                        existing.expanded = expanded;
                    }
                    Vacant(e) => {
                        let mut item = pending;
                        item.is_folder = true;
                        item.children_count = 1;
                        item.expanded = expanded;
                        e.insert(item);
                    }
                }
            }

            if !expanded {
                break;
            }
            let name: SharedString = k.to_string().into();
            if index != 0 {
                dir.push_str(separator);
            };
            dir.push_str(k);

            key_tree_item = Some(KeyTreeItem {
                id: dir.clone().into(),
                label: name.clone(),
                key_type,
                depth: index,
                expanded,
                ..Default::default()
            });
        }
        if let Some(mut key_tree_item) = key_tree_item.take() {
            // This is the deepest level for this key — guaranteed a leaf
            // since no further segment was promoted. Stamp the live TTL
            // and the client-side annotation (if any).
            key_tree_item.ttl_secs = ttl_for_leaf;
            key_tree_item.tag = tag_for_leaf;
            key_tree_item.note = note_for_leaf.clone();
            items.insert(key_tree_item.id.clone(), key_tree_item);
        }
    }

    let mut children_map: AHashMap<String, Vec<KeyTreeItem>> = AHashMap::new();

    for item in items.into_values() {
        let size = item.id.len() - item.label.len();
        let parent_id = if size == 0 { "" } else { &item.id[..(size - 1)] };
        children_map.entry(parent_id.to_string()).or_default().push(item);
    }

    for (key_id, key_type, label, depth) in promoted_leaves {
        let size = key_id.len() - label.len();
        let parent_id = if size == 0 { "" } else { &key_id[..(size - 1)] };
        let ttl_secs = key_ttls.get(&key_id).copied();
        // Same lookup as the main leaf path; promoted leaves are
        // standalone keys displaced from their slot by a same-named
        // folder, so they still want their own annotation.
        let (tag, note) = match metadata.get(key_id.as_ref()) {
            Some(m) => (m.tag, SharedString::from(m.note.clone())),
            None => (None, SharedString::default()),
        };
        children_map
            .entry(parent_id.to_string())
            .or_default()
            .push(KeyTreeItem {
                id: key_id,
                label,
                depth,
                key_type,
                ttl_secs,
                tag,
                note,
                ..Default::default()
            });
    }

    let mut result = Vec::with_capacity(children_map.values().map(|v| v.len()).sum());

    fn build_sorted_list(parent_id: &str, map: &mut AHashMap<String, Vec<KeyTreeItem>>, result: &mut Vec<KeyTreeItem>) {
        if let Some(mut children) = map.remove(parent_id) {
            children.sort_unstable_by(|a, b| b.is_folder.cmp(&a.is_folder).then_with(|| a.label.cmp(&b.label)));

            // Zebra index restarts under each parent: among this parent's leaf
            // (non-folder) children, every second one (2nd, 4th, …) is striped.
            // Folders are skipped and don't advance the count.
            let mut leaf_ix = 0usize;
            for mut child in children {
                if !child.is_folder {
                    child.stripe = leaf_ix % 2 == 1;
                    leaf_ix += 1;
                }
                let child_id = child.id.to_string();
                result.push(child);
                build_sorted_list(&child_id, map, result);
            }
        }
    }

    build_sorted_list("", &mut children_map, &mut result);

    result
}

/// Appends a synthetic "Load more" row after the visible children of any
/// expanded folder whose prefix scan stopped at the page cap (tracked in
/// `incomplete`). Clicking that row resumes the scan. No-op when nothing is
/// incomplete. `incomplete` holds prefixes in `"{folder_id}:"` form — the same
/// shape `scan_prefix` receives.
fn append_load_more_rows(
    items: Vec<KeyTreeItem>,
    incomplete: &AHashSet<SharedString>,
    label: &SharedString,
) -> Vec<KeyTreeItem> {
    if incomplete.is_empty() {
        return items;
    }
    let len = items.len();
    // For each incomplete + expanded folder: the index just past its last
    // descendant (where the row belongs) and the row to insert there.
    let mut inserts: Vec<(usize, KeyTreeItem)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !(item.is_folder && item.expanded) {
            continue;
        }
        let prefix = SharedString::from(format!("{}:", item.id));
        if !incomplete.contains(&prefix) {
            continue;
        }
        // A folder's subtree is contiguous and strictly deeper; it ends at the
        // first later row whose depth is not greater (or the end of the list).
        let mut end = i + 1;
        while end < len && items[end].depth > item.depth {
            end += 1;
        }
        let row = KeyTreeItem {
            id: SharedString::from(format!("{prefix}\u{1}load_more")),
            label: label.clone(),
            depth: item.depth + 1,
            load_more_prefix: Some(prefix),
            ..Default::default()
        };
        inserts.push((end, row));
    }
    if inserts.is_empty() {
        return items;
    }
    let mut result: Vec<KeyTreeItem> = Vec::with_capacity(len + inserts.len());
    for (i, item) in items.into_iter().enumerate() {
        for (end, row) in inserts.iter() {
            if *end == i {
                result.push(row.clone());
            }
        }
        result.push(item);
    }
    // Folders whose subtree runs to the very end of the list.
    for (end, row) in inserts {
        if end == len {
            result.push(row);
        }
    }
    result
}

struct KeyTreeDelegate {
    items: Vec<KeyTreeItem>,
    enabled_multiple_selection: bool,
    selected_items: AHashSet<SharedString>,
    readonly: bool,
    /// Read in `render_item` to highlight the row whose key is the editor's
    /// active key. Keyed off the persistent `ZedisServerState::key()` instead
    /// of the list's transient selected index (reset on every tree rebuild —
    /// which made the highlight vanish a moment after selecting).
    server_state: Entity<ZedisServerState>,
}

impl KeyTreeDelegate {
    fn toggle_multiple_selection(&mut self, cx: &mut Context<ListState<Self>>) {
        self.enabled_multiple_selection = !self.enabled_multiple_selection;
        if self.enabled_multiple_selection {
            self.selected_items.clear();
        }
        cx.notify();
    }
}

impl ListDelegate for KeyTreeDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        // Folder icons use a calm neutral instead of a loud yellow — the folder
        // name (brighter) already carries the emphasis and the design keeps the
        // tree quiet.
        let folder_icon_color = cx.theme().foreground.alpha(0.7);
        let entry = self.items.get(ix.row)?;
        // Synthetic "Load more" row for an incomplete folder scan — a simple
        // clickable line indented to the folder's child level. The list's
        // Select event routes the click to `load_more_prefix` (see
        // `select_item_by_index`).
        if entry.load_more_prefix.is_some() {
            let primary = cx.theme().primary;
            let label = entry.label.clone();
            return Some(
                ListItem::new(ix).w_full().py_2().px_2().child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .justify_center()
                        .text_color(primary)
                        .child(Icon::new(CustomIconName::ChevronsDown))
                        .child(Label::new(label).text_sm().text_color(primary)),
                ),
            );
        }
        let icon = if !entry.is_folder {
            // Key item: plain type label in a fixed-width column so the key
            // names line up regardless of type (design). The left margin opens
            // a gap between the folder guide line and the type label without
            // touching the per-level indent or the type→name spacing.
            div()
                .flex_none()
                .ml(px(8.))
                .w(px(TYPE_BADGE_COL_WIDTH))
                .child(KeyTypeBadge::new(entry.key_type).plain(true))
                .into_any_element()
        } else if entry.expanded {
            // Expanded folder: Show open folder icon
            Icon::new(IconName::FolderOpen)
                .text_color(folder_icon_color)
                .into_any_element()
        } else {
            // Collapsed folder: Show closed folder icon
            Icon::new(IconName::Folder)
                .text_color(folder_icon_color)
                .into_any_element()
        };

        let is_dark = cx.theme().is_dark();
        let is_folder = entry.is_folder;
        let is_scanning = entry.is_scanning;

        // Selection — highlight the row whose key is the editor's active key,
        // keyed off the persistent server-state key so it survives tree
        // rebuilds (the list's selected index is cleared on every rebuild,
        // which made the accent vanish moments after selecting). Gated to leaf
        // keys: folders toggle expand/collapse on click, never "select".
        let is_selected_row = !is_folder && self.server_state.read(cx).key().as_ref() == Some(&entry.id);
        // Theme-neutral selection fill (~10% foreground), the same recipe the
        // sidebar uses so the highlight reads on any theme.
        let selected_row_bg = cx.theme().foreground.alpha(0.1);
        // Faint zebra stripe fill for banded leaf rows. Dark needs a higher
        // alpha than light — white-on-dark reads weaker than black-on-light at
        // the same opacity.
        let stripe_bg = if is_dark {
            Hsla::white().alpha(0.06)
        } else {
            Hsla::black().alpha(0.03)
        };
        // Selection accent (#6b95c4, both themes) for the left bar.
        let accent_color: Hsla = rgb(0x6b95c4).into();

        let label_color = if is_folder {
            cx.theme().foreground.alpha(0.85)
        } else {
            cx.theme().foreground
        };
        // Row label — built up front so the builder chain below just drops it
        // in. No weight change on selection (a font-weight swap re-rasterizes
        // the glyphs and reads as a flicker); the accent bar + fill mark it.
        let row_label = Label::new(entry.label.clone()).text_color(label_color).text_ellipsis();
        // TTL text — rendered on every leaf row that has a known TTL value.
        // `< 1h` ⇒ warm amber accent, everything else (comfortably live, or
        // perm `-1` ∞) ⇒ muted gray. Missing (`-2`, race between SCAN and TTL)
        // renders nothing. Gated by the user setting; when off the SCAN loop
        // also skipped the TTL command (see `RedisClient::scan`).
        let show_ttl = cx.global::<ZedisGlobalStore>().read(cx).show_key_tree_ttl();
        let ttl_chip: Option<(SharedString, Hsla)> = if is_folder || !show_ttl {
            None
        } else {
            entry.ttl_secs.and_then(|secs| {
                // `ttl_chip_kind` is the "render a chip?" gate (None ⇒ the -2
                // SCAN/TTL race ⇒ no chip); the colour below is keyed off the
                // raw seconds so the warning window is exactly "< 1h".
                ttl_chip_kind(secs)?;
                let label = format_ttl_chip(secs)?;
                // Three-tier TTL colour (design): seconds left (< 1m) ⇒ red
                // (about to vanish), minutes left (< 1h) ⇒ warm amber (#cba26a),
                // anything calmer (≥ 1h) or perm ∞ (secs == -1) ⇒ muted.
                let amber: Hsla = rgb(0xcba26a).into();
                let color = if (0..60).contains(&secs) {
                    cx.theme().red
                } else if (60..3600).contains(&secs) {
                    amber
                } else {
                    cx.theme().muted_foreground
                };
                Some((label, color))
            })
        };

        // Full-bleed hover fill applied on the content div below. In dark a grey
        // tint blends into the row, so use a saturated blue that stands out; in
        // light the native `list_hover` already reads fine — leave it there.
        let hover_bg = if is_dark {
            cx.theme().blue.alpha(0.3)
        } else {
            cx.theme().blue.alpha(0.1)
        };
        let show_check_icon = self.enabled_multiple_selection && !is_folder;
        let selected = if show_check_icon && let Some(item) = self.items.get(ix.row) {
            let id = &item.id;
            self.selected_items.contains(id)
        } else {
            false
        };
        let selected_items_count = self.selected_items.len();
        let id = entry.id.clone();
        let readonly = self.readonly;
        // Pre-resolve client-side annotation visuals before the
        // ListItem builder chain — we need both the colour (for the
        // left-edge bar) and the note (for hover tooltip) below, and
        // theming reads can't borrow `cx` across the chain.
        let tag_color = entry.tag.map(|c| theme_color_for_tag(c, cx));
        // Left bar: the selection accent wins on the active row; otherwise it
        // carries the tag colour (transparent when untagged → identical height).
        let row_border = if is_selected_row {
            accent_color
        } else {
            tag_color.unwrap_or(gpui::transparent_black())
        };
        let note = entry.note.clone();
        let has_note = !note.is_empty();
        let edit_tag_id = entry.id.clone();
        let del_id = entry.id.clone();
        // Inline delete only on leaf keys (folders use their context-menu
        // delete) and only when writes are allowed.
        let show_inline_delete = !is_folder && !readonly;
        // When the row shows a TTL chip, scope the inline delete to that chip
        // slot (hover the chip to swap it for the delete) instead of revealing
        // on hover of the whole row. Rows without a chip fall back to row-hover.
        let has_ttl_chip = ttl_chip.is_some();
        let delete_tooltip = i18n_key_tree(cx, "delete_key_tooltip");
        // Dashed tree connectors: one vertical guide per ancestor level so the
        // children under an expanded folder read as a connected group. Each
        // segment bridges the row's `mb_1` gap (bottom −4px) to meet the row
        // below into a continuous dashed line. Top-level rows (depth 0) have
        // none.
        let guide_color = cx.theme().muted_foreground.alpha(0.5);
        let guides: Vec<gpui::AnyElement> = (1..=entry.depth)
            .map(|level| {
                div()
                    .absolute()
                    .top_0()
                    .bottom(px(-4.))
                    // Centered on the ancestor folder's icon (one indent level
                    // left of the child content) so the line drops from the
                    // folder; the wider TREE_INDENT_BASE leaves the gap before
                    // the child's type label.
                    .left(px((level - 1) as f32 * TREE_INDENT_BASE + TREE_INDENT_OFFSET + 8.0))
                    .w_0()
                    .border_l_1()
                    .border_dashed()
                    .border_color(guide_color)
                    .into_any_element()
            })
            .collect();
        Some(
            ListItem::new(ix)
                .font_family(get_mono_font_family())
                .w_full()
                // Padding/background/hover live on the content div below so the
                // hover fill is full-bleed and high-contrast; zero ListItem's
                // own padding here.
                .py_0()
                .px_0()
                .mb_1()
                .child(
                    div()
                        // Hover group so the inline delete button (below)
                        // can reveal itself only while this row is hovered.
                        .group("ktree-row")
                        // Positioning context for the absolute dashed guides.
                        .relative()
                        .w_full()
                        .py_2()
                        .px_2()
                        .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
                        // Extra right padding so the floating scrollbar (16px
                        // track) doesn't cover the right-aligned TTL / inline
                        // delete button.
                        .pr(px(14.))
                        // 3px left bar carries the selection accent (or the tag
                        // colour); transparent when neither, so row height never
                        // jitters.
                        .border_l_3()
                        .border_color(row_border)
                        // Zebra: every second leaf under a folder gets a faint
                        // stripe; the selected row's fill overrides it, and
                        // hover overrides both.
                        .when(entry.stripe, |this| this.bg(stripe_bg))
                        .when(is_selected_row, |this| this.bg(selected_row_bg))
                        .hover(|s| s.bg(hover_bg))
                        .children(guides)
                        .context_menu(move |mut menu, _window, cx| {
                            let id = id.clone();
                            let multi_selection_count = if selected { selected_items_count } else { 0 };
                            if !readonly {
                                if selected && selected_items_count > 1 {
                                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                                    let text = t!(
                                        "key_tree.delete_keys_tooltip",
                                        count = selected_items_count,
                                        locale = locale
                                    );
                                    menu = menu
                                        .menu_element_with_icon(
                                            CustomIconName::ListX,
                                            Box::new(KeyTreeAction::DeleteMultipleKeys),
                                            move |_, _cx| Label::new(text.clone()),
                                        )
                                        .menu_element_with_icon(
                                            CustomIconName::Clock3,
                                            Box::new(KeyTreeAction::SetTtlMultipleKeys),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                                        )
                                        .menu_element_with_icon(
                                            CustomIconName::Clock3,
                                            Box::new(KeyTreeAction::PersistMultipleKeys),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                                        );
                                } else {
                                    menu = if is_folder {
                                        menu.menu_element_with_icon(
                                            CustomIconName::RotateCw,
                                            Box::new(KeyTreeAction::RefreshFolder(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "refresh_folder_tooltip")),
                                        )
                                        .menu_element_with_icon(
                                            CustomIconName::X,
                                            Box::new(KeyTreeAction::DeleteFolder(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "delete_folder_tooltip")),
                                        )
                                        .menu_element_with_icon(
                                            CustomIconName::Clock3,
                                            Box::new(KeyTreeAction::SetTtlFolder(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                                        )
                                        .menu_element_with_icon(
                                            CustomIconName::Clock3,
                                            Box::new(KeyTreeAction::PersistFolder(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                                        )
                                    } else {
                                        menu.menu_element_with_icon(
                                            CustomIconName::X,
                                            Box::new(KeyTreeAction::DeleteKey(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "delete_key_tooltip")),
                                        )
                                    };
                                }
                            }
                            if multi_selection_count > 0 {
                                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                                let text = t!(
                                    "key_tree.export_selected_tooltip",
                                    count = multi_selection_count,
                                    locale = locale
                                );
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::Save,
                                    Box::new(KeyTreeAction::ExportSelectedKeys),
                                    move |_, _cx| Label::new(text.clone()),
                                );
                            } else if is_folder {
                                let folder_id = id.clone();
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::Save,
                                    Box::new(KeyTreeAction::ExportFolder(folder_id)),
                                    move |_, cx| Label::new(i18n_key_tree(cx, "export_folder_tooltip")),
                                );
                            } else {
                                let key_id = id.clone();
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::Save,
                                    Box::new(KeyTreeAction::ExportKey(key_id)),
                                    move |_, cx| Label::new(i18n_key_tree(cx, "export_key_tooltip")),
                                );
                            }
                            if !readonly {
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::HardDrive,
                                    Box::new(KeyTreeAction::ImportFromFile),
                                    move |_, cx| Label::new(i18n_key_tree(cx, "import_from_file_tooltip")),
                                );
                            }
                            // Tag & note editor — only meaningful on a
                            // single leaf key. Folders aggregate tags
                            // as a v2 concern (currently no schema for
                            // that), and bulk-tagging a multi-select
                            // wants its own dialog UX, so we hide both
                            // and present a single-key flow here.
                            if !is_folder && multi_selection_count == 0 {
                                let tag_key = edit_tag_id.clone();
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::FilePenLine,
                                    Box::new(KeyTreeAction::EditKeyTag(tag_key)),
                                    move |_, cx| Label::new(i18n_key_tag(cx, "edit_menu_label")),
                                );
                            }
                            menu
                        })
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .flex_1()
                                .min_w_0()
                                // Positioning context for the absolute, hover-only
                                // delete button below (out of flow → reserves no
                                // width). The TTL chip hides on hover so the button
                                // takes its spot without overlapping it.
                                .relative()
                                .child(icon)
                                .child(
                                    div()
                                        .id(("ktree-label", ix.row))
                                        .flex_1()
                                        .min_w_0()
                                        // Note tooltip on hover — only attached when
                                        // there's actually a note, otherwise we'd
                                        // get a useless empty tooltip on every row.
                                        // The lambda owns its `note` clone so the
                                        // tooltip survives label re-layout.
                                        .when(has_note, |this| {
                                            let note = note.clone();
                                            this.tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(note.clone()).build(window, cx)
                                            })
                                        })
                                        .child(row_label),
                                )
                                .when(show_check_icon, |this| {
                                    let check_icon = if selected {
                                        CustomIconName::SquareCheck
                                    } else {
                                        CustomIconName::Square
                                    };
                                    this.child(Icon::new(check_icon))
                                })
                                .when_some(ttl_chip, |this, (chip_label, chip_color)| {
                                    this.child(
                                        div()
                                            // Hover scope limited to this chip slot
                                            // (its own group): hovering the chip — not
                                            // the whole row — swaps it for the delete
                                            // button, so the TTL stays visible when
                                            // hovering elsewhere on the row.
                                            .group("ktree-ttl")
                                            .relative()
                                            .flex_none()
                                            .child(
                                                div().group_hover("ktree-ttl", |s| s.invisible()).child(
                                                    // Plain TTL text (no chip chrome) —
                                                    // calm and right-aligned, matching
                                                    // the design.
                                                    Label::new(chip_label)
                                                        .text_size(px(10.))
                                                        .w(px(TTL_CHIP_WIDTH))
                                                        .text_right()
                                                        .text_color(chip_color)
                                                        .flex_none(),
                                                ),
                                            )
                                            .when(show_inline_delete, |this| {
                                                let del_id = del_id.clone();
                                                let tooltip = delete_tooltip.clone();
                                                this.child(
                                                    div()
                                                        .absolute()
                                                        .inset_0()
                                                        .flex()
                                                        .items_center()
                                                        // Right-align so the small X hugs the
                                                        // right edge of the (wider) TTL slot
                                                        // instead of floating centred with a
                                                        // big gap to its right.
                                                        .justify_end()
                                                        .invisible()
                                                        .group_hover("ktree-ttl", |s| s.visible())
                                                        .child(
                                                            Button::new(("ktree-del", ix.row))
                                                                .ghost()
                                                                .xsmall()
                                                                .icon(CustomIconName::X)
                                                                .tooltip(tooltip)
                                                                .on_click(move |_, window, cx| {
                                                                    cx.stop_propagation();
                                                                    window.dispatch_action(
                                                                        Box::new(KeyTreeAction::DeleteKey(
                                                                            del_id.clone(),
                                                                        )),
                                                                        cx,
                                                                    );
                                                                }),
                                                        ),
                                                )
                                            }),
                                    )
                                })
                                .when(is_folder && is_scanning, |this| {
                                    // Inline spinner while this folder's lazy
                                    // prefix-scan is still in flight.
                                    this.child(Spinner::new().with_size(px(14.)).color(cx.theme().muted_foreground))
                                })
                                .when(entry.is_folder, |this| {
                                    this.child(
                                        Label::new(entry.children_count.to_string())
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                })
                                // Row-hover inline delete — used only when the row has
                                // no TTL chip (chip rows scope the delete to the chip
                                // slot above). Absolutely positioned (out of flow → no
                                // reserved width); dispatches the same `DeleteKey`
                                // action as the context menu (confirm + PROD escalation).
                                .when(show_inline_delete && !has_ttl_chip, |this| {
                                    let del_id = del_id.clone();
                                    let tooltip = delete_tooltip.clone();
                                    this.child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            .top_0()
                                            .bottom_0()
                                            .w(px(INLINE_DELETE_WIDTH))
                                            .flex()
                                            .items_center()
                                            // Right-align the X to the slot's right edge so it
                                            // doesn't sit centred with a big gap to its right.
                                            .justify_end()
                                            .invisible()
                                            .group_hover("ktree-row", |s| s.visible())
                                            .child(
                                                Button::new(("ktree-del", ix.row))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(CustomIconName::X)
                                                    .tooltip(tooltip)
                                                    .on_click(move |_, window, cx| {
                                                        cx.stop_propagation();
                                                        window.dispatch_action(
                                                            Box::new(KeyTreeAction::DeleteKey(del_id.clone())),
                                                            cx,
                                                        );
                                                    }),
                                            ),
                                    )
                                }),
                        ),
                ),
        )
    }

    fn set_selected_index(&mut self, ix: Option<IndexPath>, _window: &mut Window, _cx: &mut Context<ListState<Self>>) {
        if self.enabled_multiple_selection
            && let Some(ix) = ix
            && let Some(item) = self.items.get(ix.row)
        {
            let id = &item.id;
            if self.selected_items.contains(id) {
                self.selected_items.remove(id);
            } else {
                self.selected_items.insert(id.clone());
            }
        }
    }
}

/// Key tree view component for browsing and filtering Redis keys
///
/// Displays Redis keys in a hierarchical tree structure with:
/// - Folder navigation for key namespaces (using colon separators)
/// - Key type indicators (String, List, etc.) with color-coded badges
/// - Multiple query modes (All, Prefix, Exact)
/// - Real-time filtering and search
/// - Expandable/collapsible folders
/// - Visual feedback for selected keys
pub struct ZedisKeyTree {
    focus_handle: FocusHandle,

    auto_refresh_task: Option<Task<()>>,

    state: KeyTreeState,

    current_keyword: Entity<SharedString>,

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Delegate for the key tree list
    // key_tree_delegate: Entity<KeyTreeDelegate>,

    /// State for the key tree list
    key_tree_list_state: Entity<ListState<KeyTreeDelegate>>,

    /// Input field state for keyword filtering
    keyword_state: Entity<InputState>,

    /// Whether to enter add key mode
    should_enter_add_key_mode: Option<bool>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyTree {
    /// Create a new key tree view with event subscriptions
    ///
    /// Sets up reactive updates when server state changes and
    /// initializes UI components (tree, search input).
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::KeyCollapseAll => {
                    this.state.expanded_items.clear();
                    this.state.suppressed_auto_expand.clear();
                }
                ServerEvent::ServerSelected(_) => {
                    this.reset(cx);
                }
                ServerEvent::KeyTreeUpdated => {
                    this.update_key_tree(true, cx);
                }
                ServerEvent::ServerInfoUpdated => {
                    let readonly = server_state.read(cx).readonly();
                    this.key_tree_list_state.update(cx, |state, _cx| {
                        state.delegate_mut().readonly = readonly;
                    });
                }
                ServerEvent::EditionActionTriggered(action) if action == &EditorAction::Create => {
                    this.should_enter_add_key_mode = Some(true);
                    cx.notify();
                }
                ServerEvent::EditionActionTriggered(action) if action == &EditorAction::ReloadKeyTree => {
                    // Manual refresh (cmd-r / ⋯ menu): re-scan with the
                    // current keyword + query mode.
                    this.handle_filter(cx);
                }
                ServerEvent::KeySelected(key) => {
                    this.update_expand(key.clone(), cx);
                }
                ServerEvent::KeyScanReset if !this.state.preserve_expand_on_scan => {
                    // New query / server switch: collapse + scroll to
                    // top. A same-target refresh keeps the flag set
                    // (cleared on KeyScanFinished) so this arm is
                    // skipped and the expanded folders survive.
                    this.reset_expand(cx);
                }
                ServerEvent::KeyScanFinished => {
                    this.check_and_expand_keys(cx);
                    // Record what the freshly-built tree was scanned
                    // for, so a later same-target refresh is recognised
                    // — including the very first refresh after the
                    // initial load (which never calls `handle_filter`).
                    this.state.last_scan = Some((this.state.keyword.clone(), this.state.query_mode));
                    // Refresh complete: drop the preserve flag so the
                    // next genuinely-new query collapses as normal.
                    this.state.preserve_expand_on_scan = false;
                    cx.notify();
                }
                _ => {}
            }),
        );

        // Initialize keyword search input with placeholder
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });
        // initial focus
        keyword_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });

        let server_state_value = server_state.read(cx);
        let server_id = server_state_value.server_id().to_string();
        let mut query_mode = QueryMode::All;
        let mut refresh_interval_sec = 0;
        if let Ok(option) = get_session_option(&server_id) {
            query_mode = option
                .query_mode
                .as_deref()
                .and_then(|s| QueryMode::from_str(s).ok())
                .unwrap_or(QueryMode::All);
            refresh_interval_sec = option.refresh_interval_sec.unwrap_or_default();
        }
        let readonly = server_state_value.readonly();

        // Subscribe to search input events (Enter key triggers filter)
        subscriptions.push(cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
            if let InputEvent::PressEnter { .. } = &event {
                view.handle_filter(cx);
            }
        }));

        info!(server_id, "Creating new key tree view");

        let delegate = KeyTreeDelegate {
            items: Vec::new(),
            enabled_multiple_selection: false,
            selected_items: AHashSet::with_capacity(5),
            readonly,
            server_state: server_state.clone(),
        };
        let key_tree_list_state = cx.new(|cx| ListState::new(delegate, window, cx));
        subscriptions.push(cx.subscribe(&key_tree_list_state, |view, _, event, cx| match event {
            ListEvent::Select(ix) => {
                view.select_item_by_index(ix, false, cx);
            }
            ListEvent::Confirm(ix) => {
                view.select_item_by_index(ix, true, cx);
            }
            _ => {}
        }));

        let mut this = Self {
            focus_handle,
            state: KeyTreeState {
                query_mode,
                server_id: server_id.into(),
                refresh_interval_sec,
                expanded_items: AHashSet::with_capacity(EXPANDED_ITEMS_INITIAL_CAPACITY),
                ..Default::default()
            },
            current_keyword: cx.new(|_cx| SharedString::default()),
            key_tree_list_state,
            keyword_state,
            server_state,
            should_enter_add_key_mode: None,
            auto_refresh_task: None,
            _subscriptions: subscriptions,
        };

        // Initial tree build
        this.update_key_tree(true, cx);
        this.start_auto_refresh(cx);

        this
    }

    fn start_auto_refresh(&mut self, cx: &mut Context<Self>) {
        let auto_refresh_interval_sec = self.state.refresh_interval_sec;
        if auto_refresh_interval_sec == 0 {
            self.auto_refresh_task = None;
            return;
        }
        let server_state = self.server_state.clone();
        let current_keyword = self.current_keyword.clone();
        self.auto_refresh_task = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(auto_refresh_interval_sec as u64))
                    .await;
                let keyword = current_keyword.update(cx, |state, _cx| state.clone());
                info!(keyword = keyword.as_str(), "auto refresh");
                server_state.update(cx, move |handle, cx| {
                    handle.handle_auto_refresh(keyword, cx);
                });
            }
        }));
    }

    fn reset(&mut self, _cx: &mut Context<Self>) {
        self.state = KeyTreeState::default();
    }
    fn reset_expand(&mut self, _cx: &mut Context<Self>) {
        self.state.expanded_items.clear();
        self.state.suppressed_auto_expand.clear();
        self.state.scroll_to_index = Some(IndexPath::new(0));
    }
    fn update_expand(&mut self, selected_key: SharedString, cx: &mut Context<Self>) {
        let (separator, max_depth) = {
            let global_state = cx.global::<ZedisGlobalStore>().read(cx);
            (
                global_state.key_separator().to_string(),
                global_state.max_key_tree_depth(),
            )
        };
        if !selected_key.contains(separator.as_str()) {
            return;
        }
        let parts: Vec<&str> = selected_key.splitn(max_depth, separator.as_str()).collect();
        let mut inserted_count = 0;
        for i in 1..parts.len() {
            let prefix: SharedString = parts[..i].join(separator.as_str()).into();
            if self.state.expanded_items.insert(prefix) {
                inserted_count += 1;
            }
        }
        if inserted_count > 0 {
            self.check_and_expand_keys(cx);
            self.update_key_tree(true, cx);
        }
    }
    fn check_and_expand_keys(&mut self, cx: &mut Context<Self>) {
        let keys = self.server_state.read(cx).keys();
        let global_state = cx.global::<ZedisGlobalStore>().read(cx);
        if keys.len() < global_state.auto_expand_threshold() {
            let key_separator = global_state.key_separator();
            let mut expanded_items: AHashSet<SharedString> = AHashSet::new();
            keys.iter().for_each(|(key, _)| {
                if !key.contains(key_separator) {
                    return;
                }
                let parts: Vec<&str> = key.split(key_separator).collect();
                for i in 1..parts.len() {
                    let prefix = parts[..i].join(key_separator);
                    expanded_items.insert(prefix.into());
                }
            });
            self.state.expanded_items = expanded_items;
        }
    }

    /// Update the key tree structure when server state changes
    ///
    /// Rebuilds the tree only if the tree ID has changed (indicating new keys loaded).
    /// Preserves expanded folder state across rebuilds. Auto-expands all folders
    /// if the total key count is below the threshold.
    ///
    /// Uses a cached keys snapshot to avoid re-cloning all keys from server state
    /// when only expanded_items changed (e.g., folder expand/collapse).
    fn update_key_tree(&mut self, force_update: bool, cx: &mut Context<Self>) {
        // Resolve the "Load more" label up front, while `cx` is free (reading
        // `server_state` below borrows it for the rest of the function).
        let load_more_label = i18n_key_tree(cx, "load_more");
        let server_state = self.server_state.read(cx);
        let key_tree_id = server_state.key_tree_id();

        tracing::debug!(
            key_tree_server_id = server_state.server_id(),
            key_tree_id,
            "Server state updated"
        );

        self.state.query_mode = server_state.query_mode();

        // Skip rebuild if tree ID hasn't changed (same keys)
        if !force_update && self.state.key_tree_id == key_tree_id {
            return;
        }
        self.state.key_tree_id = key_tree_id.to_string().into();

        // Only re-clone keys from server state when key_tree_id actually changed
        // (keys added/removed/type changed). For expand/collapse, reuse cached snapshot.
        if self.state.cached_key_tree_id != key_tree_id {
            let keys_snapshot: Vec<(SharedString, KeyType)> =
                server_state.keys().iter().map(|(k, v)| (k.clone(), *v)).collect();
            self.state.cached_keys = Arc::new(keys_snapshot);
            self.state.cached_key_ttls = Arc::new(server_state.key_ttls().clone());
            self.state.cached_key_tree_id = key_tree_id.to_string().into();
        }

        let keys_snapshot = self.state.cached_keys.clone();
        let key_ttls_snapshot = self.state.cached_key_ttls.clone();
        let readonly = server_state.readonly();
        let scanning_prefixes = server_state.scanning_prefixes().clone();
        let incomplete_prefixes = server_state.incomplete_prefix_set();
        let expanded_items = self.state.expanded_items.clone();
        let suppressed = self.state.suppressed_auto_expand.clone();

        let view_handle = cx.entity().downgrade();
        let keyword = self.state.keyword.clone();

        // Snapshot client-side annotations for this server up-front so
        // the background tree-build task doesn't have to re-enter the
        // manager (which would also pull the DashMap guard onto the
        // worker thread). Empty map when the table is empty or load
        // fails — the tree still renders, just without colour bars.
        let metadata_snapshot = {
            let sid: &str = self.state.server_id.as_ref();
            if sid.is_empty() {
                std::collections::HashMap::new()
            } else {
                get_key_metadata_manager().records(sid).unwrap_or_default()
            }
        };
        let tag_filter_snapshot = self.state.selected_tag_filter;

        self.key_tree_list_state.update(cx, move |_state, cx| {
            let app_state = cx.global::<ZedisGlobalStore>().value(cx);
            let separator = app_state.key_separator().to_string();
            let max_key_tree_depth = app_state.max_key_tree_depth();
            cx.spawn(async move |handle, cx| {
                let task = cx.background_spawn(async move {
                    let start = std::time::Instant::now();
                    // Filter-source switch: when a colour filter is
                    // active, the input key list comes from local
                    // metadata (covers every tagged key regardless of
                    // SCAN progress); otherwise the SCAN snapshot is
                    // the source as usual.
                    let raw_keys = (*keys_snapshot).clone();
                    let keys_input = match tag_filter_snapshot {
                        Some(color) => build_tagged_keys_list(color, &raw_keys, &metadata_snapshot),
                        None => raw_keys,
                    };
                    let mut items = new_key_tree_items(
                        keys_input,
                        keyword,
                        expanded_items,
                        suppressed,
                        &separator,
                        max_key_tree_depth,
                        &key_ttls_snapshot,
                        &metadata_snapshot,
                        // The internal tag-filter check is a no-op in
                        // the filtered branch (every input row already
                        // matches), and stays as the source of truth
                        // when no upstream filtering happened.
                        tag_filter_snapshot,
                    );
                    // Stamp the inline-spinner flag on folders whose lazy
                    // prefix-scan is still running. Skipped entirely when
                    // nothing is scanning (the common case).
                    if !scanning_prefixes.is_empty() {
                        for item in items.iter_mut() {
                            if item.is_folder
                                && scanning_prefixes.contains(&SharedString::from(format!("{}:", item.id)))
                            {
                                item.is_scanning = true;
                            }
                        }
                    }
                    let items = append_load_more_rows(items, &incomplete_prefixes, &load_more_label);
                    tracing::debug!("Key tree build time: {:?}", start.elapsed());
                    items
                });

                let result = task.await;
                let _ = view_handle.update(cx, |view: &mut ZedisKeyTree, cx| {
                    // `reset_scan` clears keys then emits KeyTreeUpdated,
                    // so a refresh transiently rebuilds an empty tree.
                    // Don't collapse for that — only a real "no results"
                    // (new query, flag not set) resets expansion.
                    if result.is_empty() && !view.state.preserve_expand_on_scan {
                        view.reset_expand(cx);
                    }
                    view.state.clear_selection = true;
                    cx.notify();
                });
                handle.update(cx, |this, cx| {
                    this.delegate_mut().selected_items.clear();
                    this.delegate_mut().items = result;
                    this.delegate_mut().readonly = readonly;
                    cx.notify();
                })
            })
            .detach();
        });
    }

    /// Handle filter/search action when user submits keyword
    ///
    /// Patch one row's `tag` + `note` from the latest manager snapshot,
    /// without rebuilding the whole tree. Falls back to a full rebuild
    /// when a tag filter is active because the row's *visibility* now
    /// depends on its tag — a colour change can require the row to
    /// appear or disappear, which the linear patch can't model.
    ///
    /// O(items) linear scan — `items` caps at a few thousand and the
    /// search is trivial, so we don't bother with an id→index map.
    fn refresh_metadata_for_key(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        if self.state.selected_tag_filter.is_some() {
            // Filter-aware path: visibility may flip → full rebuild.
            self.handle_filter(cx);
            return;
        }
        let server_id = self.state.server_id.clone();
        let fresh = get_key_metadata_manager()
            .get(server_id.as_ref(), key.as_ref())
            .unwrap_or_default()
            .unwrap_or_default();
        let target = key.clone();
        self.key_tree_list_state.update(cx, |state, cx| {
            let items = &mut state.delegate_mut().items;
            if let Some(item) = items.iter_mut().find(|i| i.id == target) {
                item.tag = fresh.tag;
                item.note = SharedString::from(fresh.note);
            }
            cx.notify();
        });
    }

    /// Delegates to server state to perform the actual filtering based on
    /// current query mode. Ignores if a scan is already in progress.
    /// Pick a file and write the prepared keys CSV to it, reporting via a
    /// notification. Split from the `ExportCsv` handler so the confirm dialog's
    /// OK can invoke it.
    fn export_keys_csv_to_file(&mut self, csv: String, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(cx, server_state, csv.into_bytes(), "keys.csv", success, error);
    }

    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        // Don't trigger filter while already scanning
        let server_state_clone = self.server_state.clone();
        let server_state = self.server_state.read(cx);
        if server_state.scanning() {
            return;
        }

        let keyword = self.keyword_state.read(cx).value();
        // Same keyword + query mode as the displayed tree ⇒ this is a
        // refresh, not a new query: keep the folder-expanded state
        // (the `KeyScanReset` handler consumes this flag).
        // `last_scan` is owned by the `KeyScanFinished` handler (so the
        // *initial* load — which never calls `handle_filter` — also
        // seeds it). Here we only read it to tell a refresh apart from
        // a new query.
        let scan_sig = (keyword.clone(), self.state.query_mode);
        self.state.preserve_expand_on_scan = self.state.last_scan.as_ref() == Some(&scan_sig);
        self.state.keyword = keyword.clone();

        let server_id_clone = server_state.server_id().to_string();
        let keyword_clone = keyword.clone();
        self.current_keyword
            .update(cx, |state, _cx| *state = keyword_clone.clone());
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move {
                    let search_history_manager = get_search_history_manager();
                    search_history_manager.add_record(server_id_clone.as_str(), keyword_clone.as_str())
                })
                .await;
            if let Ok(history) = result {
                server_state_clone.update(cx, |state, _cx| {
                    state.set_search_history(history);
                });
            }
        })
        .detach();
        self.server_state.update(cx, move |handle, cx| {
            handle.handle_filter(keyword, cx);
        });
    }
    fn handle_clear_history(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        self.server_state.update(cx, |state, cx| {
            state.clear_search_history(cx);
        });
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_spawn(async move {
                    let search_history_manager = get_search_history_manager();
                    let _ = search_history_manager.clear_history(server_id.as_str());
                })
                .await;
        })
        .detach();
    }

    fn handle_add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prefix: Option<SharedString> = if let Some(key) = self.server_state.read(cx).key()
            && let Some((prefix, _)) = key.rsplit_once(":")
        {
            Some(format!("{prefix}:").into())
        } else {
            None
        };
        let supports_rejson = self.server_state.read(cx).supports_rejson();
        let mut category_list = vec!["String", "List", "Set", "Zset", "Hash", "Stream"];
        if supports_rejson {
            category_list.push("Json");
        }
        // Category indices: String=0, List=1, Set=2, Zset=3, Hash=4, Stream=5, Json=6(optional)
        let json_index = category_list.iter().position(|&s| s == "Json");
        let fields = vec![
            ZedisFormField::new("category", i18n_key_tree(cx, "category"))
                .field_type(ZedisFormFieldType::RadioGroup)
                .options(category_list.iter().map(|s| s.to_string().into()).collect()),
            ZedisFormField::new("key", i18n_common(cx, "key"))
                .placeholder(i18n_common(cx, "key_placeholder"))
                .required()
                .when_some(prefix, |this, prefix| this.default_value(prefix))
                .focus()
                .validate(move |s| {
                    if validate_long_string(s) {
                        None
                    } else {
                        Some("Too long".into())
                    }
                }),
            ZedisFormField::new("ttl", i18n_common(cx, "ttl"))
                .placeholder(i18n_common(cx, "ttl_placeholder"))
                .validate(move |s| {
                    if validate_ttl(s) {
                        None
                    } else {
                        Some("Invalid TTL".into())
                    }
                }),
            // Value field for String, List, Set
            ZedisFormField::new("value", i18n_common(cx, "value"))
                .placeholder(i18n_common(cx, "value_placeholder"))
                .required()
                .visible_on("category", &[0, 1, 2]),
            // Score + Member fields for Zset
            ZedisFormField::new("score", i18n_common(cx, "score"))
                .placeholder(i18n_common(cx, "score_placeholder"))
                .visible_on("category", &[3]),
            ZedisFormField::new("member", i18n_common(cx, "member"))
                .placeholder(i18n_common(cx, "member_placeholder"))
                .required()
                .visible_on("category", &[3]),
            // Field + Value fields for Hash
            ZedisFormField::new("hash_field", i18n_common(cx, "field"))
                .placeholder(i18n_common(cx, "field_placeholder"))
                .required()
                .visible_on("category", &[4]),
            ZedisFormField::new("hash_value", i18n_common(cx, "value"))
                .placeholder(i18n_common(cx, "value_placeholder"))
                .required()
                .visible_on("category", &[4]),
            // Stream ID field for Stream only
            ZedisFormField::new("stream_id", i18n_common(cx, "stream_id"))
                .placeholder(i18n_common(cx, "stream_id_placeholder"))
                .visible_on("category", &[5]),
            // JSON value field (only when Json type is available)
            ZedisFormField::new("json_value", i18n_common(cx, "value"))
                .placeholder("{}")
                .default_value("{}")
                .required()
                .field_type(ZedisFormFieldType::Editor)
                .h(px(90.))
                .visible_on("category", &json_index.map_or(vec![], |i| vec![i])),
        ];
        let server_state = self.server_state.clone();

        ZedisFormOptions::new(fields)
            .title(i18n_key_tree(cx, "add_key_title"))
            .confirm_label(i18n_common(cx, "confirm"))
            .cancel_label(i18n_common(cx, "cancel"))
            .support_add_fields_on("category", &[5])
            .add_field_placeholder(i18n_common(cx, "field_placeholder"))
            .add_value_placeholder(i18n_common(cx, "value_placeholder"))
            .on_dialog_submit(move |values, _window, cx| {
                let category_index = values
                    .get("category")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let category = category_list.get(category_index).copied().unwrap_or_default();
                let key = values.get("key").cloned().unwrap_or_default();
                let ttl = values.get("ttl").cloned().unwrap_or_default();

                let key_type = KeyType::from(category.to_lowercase().as_str());
                let seed = key_type.seed_args();
                let get_or_seed = |name: &str, index: usize| -> SharedString {
                    values
                        .get(name)
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .unwrap_or_else(|| seed.get(index).unwrap_or(&"").to_string().into())
                };
                let args: Vec<SharedString> = match key_type {
                    KeyType::String | KeyType::List | KeyType::Set => {
                        vec![get_or_seed("value", 0)]
                    }
                    KeyType::Zset => {
                        vec![get_or_seed("score", 0), get_or_seed("member", 1)]
                    }
                    KeyType::Hash => {
                        vec![get_or_seed("hash_field", 0), get_or_seed("hash_value", 1)]
                    }
                    KeyType::Json => {
                        // JSON.SET key $ <json_value>
                        let json_value = get_or_seed("json_value", 1);
                        vec!["$".into(), json_value]
                    }
                    KeyType::Stream => {
                        // stream_id + dynamic field-value pairs from add_fields
                        let mut args = vec![get_or_seed("stream_id", 0)];
                        // Collect dynamic field-value pairs added by the user.
                        // They are stored as sequential entries in the values map
                        // after the static fields.
                        let static_keys = [
                            "category",
                            "key",
                            "ttl",
                            "value",
                            "score",
                            "member",
                            "hash_field",
                            "hash_value",
                            "stream_id",
                            "json_value",
                        ];
                        let mut has_dynamic = false;
                        for (k, v) in &values {
                            if !static_keys.contains(&k.as_ref()) {
                                args.push(k.clone());
                                args.push(v.clone());
                                has_dynamic = true;
                            }
                        }
                        if !has_dynamic {
                            // Fallback to seed_args field-value pair
                            args.push(seed.get(1).unwrap_or(&"field").to_string().into());
                            args.push(seed.get(2).unwrap_or(&"value").to_string().into());
                        }
                        args
                    }
                    _ => seed.into_iter().map(|s| s.to_string().into()).collect(),
                };

                server_state.update(cx, |this, cx| {
                    this.add_key(category.to_string().into(), key, ttl, args, cx);
                });
                true
            })
            .dialog_width(px(480.))
            .open_dialog(window, cx);

        let entity_id = cx.entity_id();
        cx.defer(move |cx| {
            cx.notify(entity_id);
        });
    }

    fn get_tree_status_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let server_state = self.server_state.read(cx);
        // if scanning, return None
        if server_state.scanning() {
            if self.key_tree_list_state.read(cx).delegate().items.is_empty() {
                return Some(
                    div()
                        .m_5()
                        .child(ZedisSkeletonLoading::new().text(i18n_common(cx, "loading")))
                        .into_any_element(),
                );
            }
            return None;
        }
        if !self.state.is_empty && self.state.error.is_none() {
            return None;
        }

        let mut text = SharedString::default();

        if self.state.query_mode == QueryMode::Exact {
            if let Some(value) = server_state.value()
                && value.is_expired()
            {
                text = i18n_key_tree(cx, "key_not_exists");
            }
        } else {
            text = self
                .state
                .error
                .clone()
                .unwrap_or_else(|| i18n_key_tree(cx, "no_keys_found"))
        }
        if text.is_empty() {
            return Some(h_flex().into_any_element());
        }
        Some(
            div()
                .h_flex()
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .pt_5()
                .px_2()
                .child(Icon::new(IconName::Info).text_sm())
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(text).text_sm().whitespace_normal()),
                )
                .into_any_element(),
        )
    }

    fn select_item_by_index(&mut self, ix: &IndexPath, toggle: bool, cx: &mut Context<Self>) {
        let Some((id, is_folder, load_more, is_expanded)) = self.key_tree_list_state.update(cx, |state, _cx| {
            let item = state.delegate().items.get(ix.row)?;
            Some((
                item.id.clone(),
                item.is_folder,
                item.load_more_prefix.clone(),
                item.expanded,
            ))
        }) else {
            return;
        };
        // Synthetic "Load more" row → resume the folder scan rather than
        // selecting/expanding a key.
        if let Some(prefix) = load_more {
            self.server_state.update(cx, |state, cx| {
                state.load_more_prefix(prefix, cx);
            });
            return;
        }
        self.select_item(id, is_folder, toggle, is_expanded, cx);
    }

    fn select_item(
        &mut self,
        item_id: SharedString,
        is_folder: bool,
        toggle: bool,
        is_expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if is_folder {
            if is_expanded {
                if !toggle {
                    return;
                }
                // Collapse a folder shown open — whether expanded explicitly
                // or auto-opened as a single-child chain. Suppress the
                // auto-expand so the collapse sticks instead of being reopened
                // on the next rebuild.
                self.state.expanded_items.remove(&item_id);
                self.state.suppressed_auto_expand.insert(item_id.clone());
            } else {
                // Expand a collapsed folder: clear any suppression and load it.
                self.state.expanded_items.insert(item_id.clone());
                self.state.suppressed_auto_expand.remove(&item_id);
                self.server_state.update(cx, |state, cx| {
                    state.scan_prefix(format!("{}:", item_id.as_str()).into(), cx);
                });
            }
            self.update_key_tree(true, cx);
        } else {
            let is_selected = self.server_state.read(cx).key().as_ref() == Some(&item_id);
            // Select Key
            if !is_selected {
                self.server_state.update(cx, |state, cx| {
                    state.select_key(item_id.clone(), cx);
                });
            }
        }
    }

    /// Render the tree view or empty state message
    ///
    /// Displays:
    /// - Tree structure with keys and folders (normal state)
    /// - "Key not exists" message (Exact mode with expired key)
    /// - Error or "no keys found" message (empty state)
    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(status_view) = self.get_tree_status_view(cx) {
            return status_view.into_any_element();
        }

        div()
            .p_1()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .h_full()
            .child(List::new(&self.key_tree_list_state))
            .into_any_element()
    }
    /// Render the search/filter input bar with query mode selector
    ///
    /// Features:
    /// - Query mode dropdown (All/Prefix/Exact) with visual indicators
    /// - Search input field with placeholder
    /// - Search button (with loading state during scan)
    /// - Clearable input (X button appears when text entered)
    fn render_keyword_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state_clone = self.server_state.clone();
        let server_state = self.server_state.read(cx);
        let scanning = server_state.scanning();
        let readonly = server_state.readonly();
        let server_id = server_state.server_id().to_string();
        let server_id_changed = server_id.as_str() != self.state.server_id.as_str();
        let _ = server_state;
        if server_id_changed {
            self.state.server_id = server_id.clone().into();
            self.keyword_state.update(cx, |state, cx| {
                state.set_value(SharedString::default(), window, cx);
            });
        }
        let query_mode = self.state.query_mode;
        let type_filter = self.server_state.read(cx).type_filter();

        // Select icon based on query mode
        let icon = match query_mode {
            QueryMode::All => Icon::new(IconName::Asterisk), // * for all keys
            QueryMode::Prefix => Icon::new(CustomIconName::ChevronUp), // ~ for prefix
            QueryMode::Exact => Icon::new(CustomIconName::Equal), // = for exact match
        };
        let server_id_for_favorites: SharedString = server_id.clone().into();
        let query_mode_dropdown = DropdownButton::new("dropdown")
            .button(Button::new("key-tree-query-mode-btn").ghost().px_2().icon(icon))
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, cx| {
                let favorites = get_favorites_manager()
                    .records(server_id_for_favorites.as_ref())
                    .unwrap_or_default();
                let server_state_for_history = server_state_clone.clone();
                menu.submenu_with_icon(
                    Some(Icon::new(CustomIconName::Clock3)),
                    i18n_key_tree(cx, "search_history"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        let keywords = server_state_for_history.read(cx).search_history();
                        if keywords.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_search_history"));
                        } else {
                            for keyword in keywords {
                                submenu = submenu
                                    .menu_element(Box::new(KeyTreeAction::Search(keyword.clone())), move |_, _cx| {
                                        Label::new(keyword.clone())
                                    });
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::Clear),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_history")),
                            );
                        }
                        submenu
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(IconName::Star)),
                    i18n_key_tree(cx, "favorite_keys"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        if favorites.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_favorite_keys"));
                        } else {
                            for key in &favorites {
                                let key_clone = key.clone();
                                submenu = submenu.menu_element(
                                    Box::new(KeyTreeAction::SelectFavoriteKey(key.clone())),
                                    move |_, _cx| Label::new(key_clone.clone()).text_ellipsis(),
                                );
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::ClearFavorites),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_favorites")),
                            );
                        }
                        submenu
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(IconName::Asterisk)),
                    i18n_key_tree(cx, "query_mode"),
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(query_mode == QueryMode::All, Box::new(QueryMode::All), |_, cx| {
                                Label::new(i18n_key_tree(cx, "query_mode_all"))
                            })
                            .menu_element_with_check(
                                query_mode == QueryMode::Prefix,
                                Box::new(QueryMode::Prefix),
                                |_, cx| Label::new(i18n_key_tree(cx, "query_mode_prefix")),
                            )
                            .menu_element_with_check(
                                query_mode == QueryMode::Exact,
                                Box::new(QueryMode::Exact),
                                |_, cx| Label::new(i18n_key_tree(cx, "query_mode_exact")),
                            )
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::Binary)),
                    i18n_key_tree(cx, "type_filter"),
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(type_filter.is_none(), Box::new(KeyTypeFilter::All), |_, cx| {
                                Label::new(i18n_key_tree(cx, "type_filter_all"))
                            })
                            .menu_element_with_check(
                                type_filter == Some(KeyType::String),
                                Box::new(KeyTypeFilter::String),
                                |_, _| Label::new("String"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Hash),
                                Box::new(KeyTypeFilter::Hash),
                                |_, _| Label::new("Hash"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::List),
                                Box::new(KeyTypeFilter::List),
                                |_, _| Label::new("List"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Set),
                                Box::new(KeyTypeFilter::Set),
                                |_, _| Label::new("Set"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Zset),
                                Box::new(KeyTypeFilter::Zset),
                                |_, _| Label::new("Zset"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Stream),
                                Box::new(KeyTypeFilter::Stream),
                                |_, _| Label::new("Stream"),
                            )
                    },
                )
            });
        let search_btn = Button::new("key-tree-search-btn")
            .ghost()
            .loading(scanning)
            .disabled(scanning)
            .icon(IconName::Search)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));
        // keyword input
        let keyword_input = Input::new(&self.keyword_state)
            .w_full()
            .flex_1()
            .px_0()
            .prefix(query_mode_dropdown)
            .suffix(search_btn)
            .cleanable(true);
        let enabled_multiple_selection = self.key_tree_list_state.read(cx).delegate().enabled_multiple_selection;
        let refresh_interval_sec = self.state.refresh_interval_sec;

        // Capture the tag-filter state up-front so the `move` closure
        // below sees a stable snapshot — the submenu builder runs each
        // time the dropdown opens, not every render frame.
        let tag_filter_has_records = get_key_metadata_manager().has_any_records(server_id.as_str());
        let tag_filter_active = self.state.selected_tag_filter;
        let more_dropdown = Button::new("key-tree-more-dropdown")
            .outline()
            .icon(Icon::new(IconName::Ellipsis))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                menu.menu_element_with_icon(
                    Icon::new(CustomIconName::RotateCw),
                    Box::new(KeyTreeAction::RefreshAll),
                    move |_, cx| {
                        Label::new(format!(
                            "{} ({})",
                            i18n_key_tree(cx, "refresh_keys"),
                            humanize_keystroke("cmd-r")
                        ))
                    },
                )
                .menu_element_with_icon(
                    Icon::new(CustomIconName::ListChecvronsDownUp),
                    Box::new(KeyTreeAction::CollapseAllKeys),
                    move |_, cx| Label::new(i18n_key_tree(cx, "collapse_keys")),
                )
                .menu_element_with_icon(
                    Icon::new(CustomIconName::Save),
                    Box::new(KeyTreeAction::ExportCsv),
                    move |_, cx| Label::new(i18n_common(cx, "export_csv")),
                )
                .when(!readonly, |this| {
                    let icon = if enabled_multiple_selection {
                        Icon::new(IconName::Check)
                    } else {
                        Icon::new(CustomIconName::ListCheck)
                    };
                    this.menu_element_with_icon(icon, Box::new(KeyTreeAction::ToggleMultiSelectMode), move |_, cx| {
                        Label::new(i18n_key_tree(cx, "toggle_multi_select_mode"))
                    })
                })
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::RotateCw)),
                    i18n_key_tree(cx, "auto_refresh"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        for interval in [0, 1, 5, 10, 30, 60, 120] {
                            let label = if interval == 0 {
                                i18n_key_tree(cx, "disable_auto_refresh")
                            } else {
                                format!("{}s", interval).into()
                            };
                            submenu = submenu.menu_element_with_check(
                                refresh_interval_sec == interval,
                                Box::new(KeyTreeAction::AutoRefresh(interval)),
                                move |_, _cx| Label::new(label.clone()),
                            )
                        }

                        submenu
                    },
                )
                // Tag colour filter — hidden until the user has at
                // least one tagged key. Lives inside the "more" menu
                // (not as a top-level toolbar button) so the search
                // input doesn't lose width on narrow windows.
                .when(tag_filter_has_records, |this| {
                    this.submenu_with_icon(
                        Some(Icon::new(CustomIconName::SwatchBook)),
                        i18n_key_tag(cx, "filter_button_tooltip"),
                        window,
                        cx,
                        move |submenu, _window, _cx| {
                            let mut submenu = submenu.menu_element_with_check(
                                tag_filter_active.is_none(),
                                Box::new(KeyTreeAction::SetTagFilter(SharedString::default())),
                                move |_, cx| Label::new(i18n_key_tag(cx, "filter_all")),
                            );
                            for color in TagColor::ALL {
                                let label_key = match color {
                                    TagColor::Red => "color_red",
                                    TagColor::Orange => "color_orange",
                                    TagColor::Yellow => "color_yellow",
                                    TagColor::Green => "color_green",
                                    TagColor::Blue => "color_blue",
                                    TagColor::Purple => "color_purple",
                                };
                                let color_name: SharedString = color.as_str().into();
                                submenu = submenu.menu_element_with_check(
                                    tag_filter_active == Some(color),
                                    Box::new(KeyTreeAction::SetTagFilter(color_name)),
                                    move |_, cx| Label::new(i18n_key_tag(cx, label_key)),
                                );
                            }
                            submenu
                        },
                    )
                })
                .menu_element_with_icon(
                    Icon::new(CustomIconName::Rss),
                    Box::new(KeyTreeAction::ChangeChannelMode),
                    move |_, cx| Label::new(i18n_key_tree(cx, "pubsub_mode")),
                )
            });

        h_flex()
            .flex_shrink_0()
            .px_2()
            .h(KEY_TREE_KEYWORD_INPUT_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .w_full()
            .gap_x_2()
            .child(keyword_input)
            .child(
                Button::new("key-tree-add-btn")
                    .disabled(readonly)
                    .when(readonly, |this| this.tooltip(i18n_common(cx, "disable_in_readonly")))
                    .when(!readonly, |this| {
                        let tooltip = format!(
                            "{} ({})",
                            i18n_key_tree(cx, "add_key_tooltip"),
                            humanize_keystroke("cmd-n")
                        );
                        this.tooltip(tooltip)
                    })
                    .outline()
                    .icon(CustomIconName::FilePlusCorner)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.handle_add_key(window, cx);
                    })),
            )
            .child(more_dropdown)
    }
}

impl Render for ZedisKeyTree {
    /// Main render method - displays search bar and tree structure
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(scroll_to_index) = self.state.scroll_to_index.take() {
            self.key_tree_list_state.update(cx, |state, cx| {
                state.scroll_to_item(scroll_to_index, ScrollStrategy::Top, window, cx);
            });
        }
        if std::mem::take(&mut self.state.clear_selection) {
            self.key_tree_list_state.update(cx, |state, cx| {
                state.set_selected_index(None, window, cx);
            });
        }
        if let Some(true) = self.should_enter_add_key_mode.take() {
            self.handle_add_key(window, cx);
        }
        v_flex()
            .id("key-tree-container")
            .track_focus(&self.focus_handle)
            .h_full()
            .w_full()
            .child(self.render_keyword_input(window, cx))
            .child(self.render_tree(cx))
            .on_action(cx.listener(|this, e: &QueryMode, _window, cx| {
                let new_mode = *e;

                let server_id = this.server_state.read(cx).server_id();
                if let Ok(mut option) = get_session_option(server_id) {
                    option.query_mode = Some(new_mode.to_string());
                    save_session_option(server_id, option, cx);
                }

                // Step 1: Update server state with new query mode
                this.server_state.update(cx, |state, cx| {
                    state.set_query_mode(new_mode, cx);
                });

                // Step 2: Update local UI state
                this.state.query_mode = new_mode;
            }))
            .on_action(cx.listener(|this, e: &KeyTypeFilter, _window, cx| {
                let filter = match e {
                    KeyTypeFilter::All => None,
                    KeyTypeFilter::String => Some(KeyType::String),
                    KeyTypeFilter::List => Some(KeyType::List),
                    KeyTypeFilter::Set => Some(KeyType::Set),
                    KeyTypeFilter::Zset => Some(KeyType::Zset),
                    KeyTypeFilter::Hash => Some(KeyType::Hash),
                    KeyTypeFilter::Stream => Some(KeyType::Stream),
                };
                this.server_state
                    .update(cx, |state, cx| state.set_type_filter(filter, cx));
            }))
            .on_action(cx.listener(|this, e: &KeyTreeAction, window, cx| match e {
                KeyTreeAction::ChangeChannelMode => {
                    this.server_state.update(cx, |state, cx| {
                        state.change_channel_mode(cx);
                    });
                }
                KeyTreeAction::AutoRefresh(interval) => {
                    this.state.refresh_interval_sec = *interval;
                    this.start_auto_refresh(cx);
                    let server_id = this.server_state.read(cx).server_id();
                    if let Ok(mut option) = get_session_option(server_id) {
                        option.refresh_interval_sec = Some(*interval);
                        save_session_option(server_id, option, cx);
                    }
                }
                KeyTreeAction::RefreshAll => {
                    this.handle_filter(cx);
                }
                KeyTreeAction::CollapseAllKeys => {
                    this.server_state.update(cx, |state, cx| {
                        state.collapse_all_keys(cx);
                    });
                }
                KeyTreeAction::ExportCsv => {
                    let (count, csv) = {
                        let state = this.server_state.read(cx);
                        let keys = state.keys();
                        if keys.is_empty() {
                            return;
                        }
                        let ttls = state.key_ttls();
                        let mut rows: Vec<Vec<String>> = keys
                            .iter()
                            .map(|(k, t)| {
                                let ttl = ttls.get(k).copied().unwrap_or(-1);
                                let ttl_str = if ttl >= 0 { ttl.to_string() } else { String::new() };
                                vec![k.to_string(), t.as_str().to_string(), ttl_str]
                            })
                            .collect();
                        rows.sort_by(|a, b| a[0].cmp(&b[0]));
                        (rows.len(), build_csv(&["key", "type", "ttl_seconds"], &rows))
                    };
                    // The CSV only covers the keys currently loaded into the
                    // tree (a SCAN-limited subset), so confirm with the count
                    // first — it is not the whole keyspace.
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let message = t!("key_tree.export_csv_confirm", count = count, locale = locale).to_string();
                    let weak = cx.entity().downgrade();
                    ZedisDialog::new_alert(i18n_common(cx, "export_csv"), message)
                        .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "export_csv")))
                        .on_ok(move |_, _window, cx| {
                            if let Some(tree) = weak.upgrade() {
                                let csv = csv.clone();
                                tree.update(cx, |this, cx| this.export_keys_csv_to_file(csv, cx));
                            }
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::ToggleMultiSelectMode => {
                    this.key_tree_list_state.update(cx, |state, cx| {
                        state.delegate_mut().toggle_multiple_selection(cx);
                    });
                }
                KeyTreeAction::Search(keyword) => {
                    this.keyword_state.update(cx, |state, cx| {
                        state.set_value(keyword, window, cx);
                    });
                    this.handle_filter(cx);
                }
                KeyTreeAction::Clear => {
                    this.handle_clear_history(cx);
                }
                KeyTreeAction::EditKeyTag(key) => {
                    // Right-click → "Edit tag & note…". Callback patches
                    // just the affected row from the manager's fresh
                    // snapshot — no full tree rebuild for the common
                    // case (no active tag filter). `refresh_metadata_for_key`
                    // delegates to `handle_filter` automatically when a
                    // filter IS active, since row visibility then
                    // depends on the new tag colour.
                    let server_id: SharedString = this.server_state.read(cx).server_id().to_string().into();
                    let key = key.clone();
                    let key_for_callback = key.clone();
                    let weak_tree = cx.entity().downgrade();
                    let on_done: OnTagDialogDone = std::sync::Arc::new(move |cx| {
                        if let Some(tree) = weak_tree.upgrade() {
                            let key = key_for_callback.clone();
                            tree.update(cx, |this, cx| this.refresh_metadata_for_key(&key, cx));
                        }
                    });
                    open_key_tag_dialog(server_id, key, window, cx, Some(on_done));
                }
                KeyTreeAction::SetTagFilter(color_name) => {
                    let new_filter = if color_name.is_empty() {
                        None
                    } else {
                        TagColor::from_str(color_name.as_ref())
                    };
                    if this.state.selected_tag_filter != new_filter {
                        this.state.selected_tag_filter = new_filter;
                        // Re-run the filter pass to apply the new
                        // colour selection. Cached keys are reused;
                        // only the tree-rebuild step re-runs.
                        this.handle_filter(cx);
                    }
                }
                KeyTreeAction::SelectFavoriteKey(key) => {
                    this.select_item(key.clone(), false, false, false, cx);
                }
                KeyTreeAction::ClearFavorites => {
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let _ = get_favorites_manager().clear_history(&server_id);
                            })
                            .await;
                    })
                    .detach();
                }
                KeyTreeAction::DeleteMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_keys_prompt", keys = keys.join(", "), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_keys_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.unlink_keys(keys.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::DeleteKey(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_key_prompt", key = id.clone(), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_key_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.delete_key(id.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::RefreshFolder(id) => {
                    let id = id.clone();
                    this.server_state.update(cx, |state, cx| {
                        state.refresh_prefix(format!("{}:", id.as_str()).into(), cx);
                    });
                }
                KeyTreeAction::DeleteFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_folder_prompt", folder = id.clone(), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_folder_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.delete_folder(id.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::PersistMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.clone();
                    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| state.batch_set_ttl_keys(keys.clone(), None, cx));
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::PersistFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| state.batch_set_ttl_folder(id.clone(), None, cx));
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::SetTtlMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.clone();
                    let ttl_input = cx
                        .new(|cx| InputState::new(window, cx).placeholder(i18n_key_tree(cx, "batch_ttl_placeholder")));
                    let input_child = ttl_input.clone();
                    let input_ok = ttl_input.clone();
                    let prompt = i18n_key_tree(cx, "batch_ttl_prompt");
                    ZedisDialog::new(i18n_key_tree(cx, "batch_ttl_title"))
                        .w(px(360.))
                        .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                        .cancel_text(i18n_common(cx, "cancel"))
                        .button_props(
                            dialog_button_props(cx)
                                .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                                .cancel_text(i18n_common(cx, "cancel")),
                        )
                        .child(move || {
                            v_flex()
                                .gap_2()
                                .child(Label::new(prompt.clone()).text_sm())
                                .child(Input::new(&input_child).small())
                        })
                        .on_ok(move |_, _, cx| match parse_duration(input_ok.read(cx).value().trim()) {
                            Ok(d) => {
                                let secs = d.as_secs();
                                server_state
                                    .update(cx, |state, cx| state.batch_set_ttl_keys(keys.clone(), Some(secs), cx));
                                true
                            }
                            Err(_) => false,
                        })
                        .open(window, cx);
                }
                KeyTreeAction::SetTtlFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let ttl_input = cx
                        .new(|cx| InputState::new(window, cx).placeholder(i18n_key_tree(cx, "batch_ttl_placeholder")));
                    let input_child = ttl_input.clone();
                    let input_ok = ttl_input.clone();
                    let prompt = i18n_key_tree(cx, "batch_ttl_prompt");
                    ZedisDialog::new(i18n_key_tree(cx, "batch_ttl_title"))
                        .w(px(360.))
                        .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                        .cancel_text(i18n_common(cx, "cancel"))
                        .button_props(
                            dialog_button_props(cx)
                                .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                                .cancel_text(i18n_common(cx, "cancel")),
                        )
                        .child(move || {
                            v_flex()
                                .gap_2()
                                .child(Label::new(prompt.clone()).text_sm())
                                .child(Input::new(&input_child).small())
                        })
                        .on_ok(move |_, _, cx| match parse_duration(input_ok.read(cx).value().trim()) {
                            Ok(d) => {
                                let secs = d.as_secs();
                                server_state
                                    .update(cx, |state, cx| state.batch_set_ttl_folder(id.clone(), Some(secs), cx));
                                true
                            }
                            Err(_) => false,
                        })
                        .open(window, cx);
                }
                KeyTreeAction::ExportSelectedKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.read(cx);
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, keys, cx);
                }
                KeyTreeAction::ExportFolder(folder) => {
                    let folder = folder.clone();
                    let prefix = format!("{folder}:");
                    let server_state = this.server_state.read(cx);
                    let keys: Vec<SharedString> = server_state
                        .keys()
                        .keys()
                        .filter(|k| k.as_str() == folder.as_str() || k.as_str().starts_with(&prefix))
                        .cloned()
                        .collect();
                    if keys.is_empty() {
                        return;
                    }
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, keys, cx);
                }
                KeyTreeAction::ExportKey(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.read(cx);
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, vec![id], cx);
                }
                KeyTreeAction::ImportFromFile => {
                    let server_state = this.server_state.read(cx);
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_import_window(server_id, server_name, db, cx);
                }
            }))
            .on_action(cx.listener(|this, event: &EditorAction, window, cx| match event {
                EditorAction::Search => {
                    this.keyword_state.focus_handle(cx).focus(window, cx);
                }
                EditorAction::Delete => {
                    // `cmd-backspace` while the tree is focused deletes the
                    // current selection. `EditorAction::Delete` is otherwise
                    // only handled by the editor view (a sibling), so it never
                    // reached us via focus-tree dispatch. Reuse the tree's own
                    // delete-confirm flow: batch when multi-select has picks,
                    // else the selected key.
                    let multi = {
                        let delegate = this.key_tree_list_state.read(cx).delegate();
                        delegate.enabled_multiple_selection && !delegate.selected_items.is_empty()
                    };
                    if multi {
                        window.dispatch_action(Box::new(KeyTreeAction::DeleteMultipleKeys), cx);
                    } else if let Some(key) = this.server_state.read(cx).key() {
                        window.dispatch_action(Box::new(KeyTreeAction::DeleteKey(key)), cx);
                    }
                }
                _ => {
                    cx.propagate();
                }
            }))
    }
}
