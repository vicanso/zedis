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
    connection::{Capability, get_server},
    constants::EDITOR_KEY_BAR_HEIGHT,
    db::{
        KeyMetadata, TagColor, get_favorites_manager, get_key_metadata_manager, get_recent_keys_manager,
        get_search_history_manager, recent_keys_scope,
    },
    helpers::{
        EditorAction, TtlFilter, build_csv, format_ttl_chip, get_mono_font_family, group_thousands, humanize_keystroke,
        parse_duration, theme_color_for_tag, ttl_chip_kind, validate_long_string, validate_ttl,
    },
    states::{
        GlobalEvent, KeyType, KeyTypeFilter, QueryMode, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState,
        dialog_button_props, escalate_dangerous_body, get_session_option, i18n_common, i18n_editor, i18n_key_tag,
        i18n_key_tree, save_session_option,
    },
    views::{
        OnTagDialogDone, export_to_file, open_batch_key_tag_dialog, open_key_tag_dialog, open_migration_export_window,
    },
};
use ahash::{AHashMap, AHashSet};
use gpui::{
    Action, Anchor, App, AppContext, ClipboardItem, Entity, FocusHandle, Focusable, Hsla, ScrollStrategy, SharedString,
    Subscription, Task, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::ContextMenuExt,
    notification::Notification,
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
mod build;
mod delegate;

use build::*;
use delegate::*;

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
    /// Open a key from the per-connection MRU list (same path as favorites).
    SelectRecentKey(SharedString),
    ClearRecentKeys,
    ExportSelectedKeys,
    ExportFolder(SharedString),
    ExportKey(SharedString),
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
    /// Local TTL-range filter (`TtlFilter::as_str` wire id). `"all"` /
    /// empty clears it. Applied only on the already-loaded TTL cache.
    SetTtlFilter(SharedString),
    /// Multi-select: open the batch tag colour dialog for the current
    /// selection (tag only — notes on each key are preserved).
    BatchTagSelectedKeys,
    /// Copy the full key name to the clipboard.
    CopyKeyName(SharedString),
    /// Copy the folder prefix (with trailing separator) to the clipboard.
    CopyFolderPrefix(SharedString),
    /// Select the key and open the editor's rename dialog.
    RenameKey(SharedString),
    /// Add / remove the key from the local favorites list.
    ToggleFavoriteKey(SharedString),
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
    /// Local TTL-range filter. [`TtlFilter::All`] means no TTL constraint.
    /// Combined with type + tag via AND on the already-loaded key set.
    selected_ttl_filter: TtlFilter,
}

#[derive(Default, Debug, Clone)]
struct KeyTreeItem {
    id: SharedString,
    label: SharedString,
    depth: usize,
    /// Index of the nearest ancestor folder row in the flattened items list
    /// (`None` for top-level rows). Filled by `fill_parent_indices` after the
    /// tree is built; powers the sticky-ancestor overlay's O(depth) walk.
    parent_ix: Option<usize>,
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
    /// stays O(1) per row. Leaves use their own tag; folders use the
    /// **mode** (most common) colour among tagged descendants in local
    /// metadata (see [`stamp_folder_tag_aggregates`]).
    tag: Option<TagColor>,
    /// Folder only: more than one distinct tag colour under this prefix.
    /// Leaves always `false`. Drives tooltip wording; left bar still
    /// shows the mode colour.
    tag_mixed: bool,
    /// Folder only: preformatted "Red 3 · Blue 1" summary for tooltips.
    /// Empty on leaves / untagged folders.
    folder_tag_summary: SharedString,
    /// Free-form note. Empty when there's no annotation. Rendered as a
    /// hover tooltip on the row label so it doesn't steal layout
    /// space from the type badge / TTL chip. Folders never carry notes.
    note: SharedString,
    /// True while this folder's lazy `scan_prefix` is still running, so
    /// `render_item` can show an inline spinner. Always false for leaves.
    is_scanning: bool,
    /// When set, this row is a synthetic "Load more" affordance for the
    /// given (incomplete) folder prefix — clicking it resumes the scan.
    /// `None` for real key / folder rows.
    load_more_prefix: Option<SharedString>,
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

    /// Scroll offset seen by the last list-state notify, so the observer below
    /// only re-renders (→ sticky breadcrumb recompute) when the viewport
    /// actually moved — guarding against notify/render ping-pong.
    last_observed_scroll_y: f32,

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

        // Pause auto-refresh while the editor suite is cached but not
        // visible (user is on Metrics/Slowlog/…). Resume when Editor is
        // shown again so SCAN doesn't run against a hidden tree.
        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global, event, cx| {
            if let GlobalEvent::RouteChanged(route) = event {
                if route.server_view() == Some(ServerView::Editor) {
                    this.start_auto_refresh(cx);
                } else {
                    this.auto_refresh_task = None;
                }
            }
        }));

        // A scan can be started programmatically (e.g. Memory Analyzer's
        // "search this prefix"): mirror the externally-set keyword into the
        // search box and the local caches so the tree, box and auto-refresh
        // all agree. A scan typed into the box is a no-op here (values match).
        subscriptions.push(cx.subscribe_in(
            &server_state,
            window,
            |this, server_state, event: &ServerEvent, window, cx| {
                if !matches!(event, ServerEvent::KeyScanStarted) {
                    return;
                }
                let keyword = server_state.read(cx).keyword();
                if this.keyword_state.read(cx).value() == keyword {
                    return;
                }
                this.keyword_state.update(cx, |input, cx| {
                    input.set_value(keyword.clone(), window, cx);
                });
                this.state.keyword = keyword.clone();
                this.state.preserve_expand_on_scan = false;
                this.current_keyword.update(cx, |state, _cx| *state = keyword);
            },
        ));
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
        // Programmatic scrolls (keyboard navigation, the sticky-breadcrumb /
        // scroll_to_item jumps) notify the list state without any wheel event
        // reaching the container, which would leave the sticky breadcrumb
        // stale. Re-render when the offset actually moved; the comparison
        // keeps unrelated list notifies from ping-ponging renders.
        subscriptions.push(cx.observe(&key_tree_list_state, |this, state, cx| {
            let y = state.read(cx).scroll_handle().base_handle().offset().y.as_f32();
            if (y - this.last_observed_scroll_y).abs() > f32::EPSILON {
                this.last_observed_scroll_y = y;
                cx.notify();
            }
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
            last_observed_scroll_y: 0.0,
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
            let s = self.server_state.read(cx);
            (s.key_separator().to_string(), s.max_key_tree_depth())
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
        let server_state = self.server_state.read(cx);
        let keys = server_state.keys();
        let key_separator = server_state.key_separator().to_string();
        let auto_expand_threshold = server_state.auto_expand_threshold();
        if keys.len() < auto_expand_threshold {
            let mut expanded_items: AHashSet<SharedString> = AHashSet::new();
            keys.iter().for_each(|(key, _)| {
                if !key.contains(key_separator.as_str()) {
                    return;
                }
                let parts: Vec<&str> = key.split(key_separator.as_str()).collect();
                for i in 1..parts.len() {
                    let prefix = parts[..i].join(key_separator.as_str());
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
        // Server-side SCAN TYPE already narrows the snapshot; re-apply
        // locally so tag-sourced rows and older Redis paths stay honest.
        let type_filter_snapshot = server_state.type_filter();
        let separator = server_state.key_separator().to_string();
        let max_key_tree_depth = server_state.max_key_tree_depth();
        // Without tree TTL chips, `key_ttls` is empty — ignore any stale
        // TTL filter so the tree is not accidentally wiped.
        let ttl_enabled = server_state.show_key_tree_ttl();

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
        let ttl_filter_snapshot = if ttl_enabled {
            self.state.selected_ttl_filter
        } else {
            TtlFilter::All
        };
        self.key_tree_list_state.update(cx, move |_state, cx| {
            cx.spawn(async move |handle, cx| {
                let task = cx.background_spawn(async move {
                    let start = std::time::Instant::now();
                    // Source switch: tag filter → local metadata union
                    // (covers tagged keys not yet in the SCAN page);
                    // otherwise the SCAN snapshot is the source.
                    // Then local AND of type + tag + TTL on that set.
                    let raw_keys = (*keys_snapshot).clone();
                    let source = match tag_filter_snapshot {
                        Some(color) => build_tagged_keys_list(color, &raw_keys, &metadata_snapshot),
                        None => raw_keys,
                    };
                    let keys_input = apply_local_key_filters(
                        source,
                        type_filter_snapshot,
                        tag_filter_snapshot,
                        ttl_filter_snapshot,
                        &key_ttls_snapshot,
                        &metadata_snapshot,
                    );
                    let mut items = new_key_tree_items(
                        keys_input,
                        keyword,
                        expanded_items,
                        suppressed,
                        &separator,
                        max_key_tree_depth,
                        &key_ttls_snapshot,
                        &metadata_snapshot,
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
                    let mut items = append_load_more_rows(items, &incomplete_prefixes, &load_more_label);
                    fill_parent_indices(&mut items);
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

    /// Rebuild the tree after a tag/note change so leaf rows and
    /// ancestor folder aggregates stay consistent (local-only, no re-SCAN).
    fn refresh_metadata_for_key(&mut self, _key: &SharedString, cx: &mut Context<Self>) {
        self.update_key_tree(true, cx);
    }

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

    /// Ancestor-folder chain of the first visible row — `(item index, label,
    /// depth)`, shallowest first — for the sticky overlay. Empty when the list
    /// is at the top / unmeasured, or the top row is top-level.
    ///
    /// The first visible index is derived from the scroll offset and a
    /// self-calibrated row pitch (content height ÷ row count — the List
    /// contract guarantees uniform row heights), so no height constant can
    /// drift out of sync with the row styling.
    fn sticky_ancestors(&self, cx: &App) -> Vec<(usize, SharedString, usize)> {
        let state = self.key_tree_list_state.read(cx);
        let items = &state.delegate().items;
        if items.is_empty() {
            return Vec::new();
        }
        let handle = state.scroll_handle().base_handle();
        let scrolled = -handle.offset().y.as_f32();
        let viewport_h = handle.bounds().size.height.as_f32();
        if scrolled <= 0.0 || viewport_h <= 0.0 {
            return Vec::new();
        }
        let content_h = handle.max_offset().y.as_f32() + viewport_h;
        let row_h = content_h / items.len() as f32;
        if row_h <= 0.0 {
            return Vec::new();
        }
        let first_visible = ((scrolled / row_h) as usize).min(items.len() - 1);
        // The breadcrumb overlay covers roughly one row at the top, so the row
        // it must describe is the one just below it — anchoring on the covered
        // row makes the sticky switch a row late and linger at boundaries.
        let anchor = (first_visible + 1).min(items.len() - 1);
        let mut chain: Vec<(usize, SharedString, usize)> = Vec::new();
        let mut cur = items[anchor].parent_ix;
        while let Some(ix) = cur {
            chain.push((ix, items[ix].label.clone(), items[ix].depth));
            cur = items[ix].parent_ix;
        }
        chain.reverse();
        // Trim (deepest first) every entry whose subtree end is already on
        // screen below the overlay — a folder that fits in the viewport would
        // otherwise flash a sticky row while scrolling past it. If the deepest
        // survivor extends beyond the viewport, its ancestors necessarily do.
        let viewport_rows = (viewport_h / row_h).ceil() as usize;
        while let Some((_, _, depth)) = chain.last() {
            if subtree_ends_before(items, anchor, *depth, first_visible + viewport_rows) {
                chain.pop();
            } else {
                break;
            }
        }
        chain
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

        let sticky = self.sticky_ancestors(cx);
        let border_color = cx.theme().border;
        let sticky_bg = cx.theme().sidebar;
        let icon_color = cx.theme().foreground.alpha(0.9);
        let label_color = cx.theme().foreground;

        div()
            .p_1()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .h_full()
            // Positioning context for the sticky-ancestor overlay below.
            .relative()
            // Wheel/trackpad scrolling repaints the list without notifying its
            // entity, so nudge a re-render here to keep the sticky overlay in
            // step (scrollbar drags & keyboard nav land on existing notifies).
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .child(List::new(&self.key_tree_list_state))
            .when(!sticky.is_empty(), |this| {
                // Pinned ancestor path as ONE breadcrumb row — the chain joined
                // with the key separator ("bench:gui:v1:hash") instead of one
                // row per level, so deep nesting doesn't eat the viewport.
                // Clicking jumps to the deepest pinned folder (the subtree the
                // top of the viewport is inside).
                let separator = self.server_state.read(cx).key_separator().to_string();
                let deep_ix = sticky.last().map(|(ix, _, _)| *ix).unwrap_or_default();
                let path: SharedString = sticky
                    .iter()
                    .map(|(_, label, _)| label.as_ref())
                    .collect::<Vec<_>>()
                    .join(separator.as_str())
                    .into();
                this.child(
                    h_flex()
                        .id("ktree-sticky-path")
                        .absolute()
                        // Anchor at the container's very top and re-create the
                        // `p_1` inset as own top padding — anchoring at top(4px)
                        // left a sliver of the scrolled rows peeking above the
                        // overlay (1px flicker while scrolling).
                        .top_0()
                        .left(px(4.))
                        .right(px(4.))
                        .pt(px(12.))
                        .pb_2()
                        .px_2()
                        // Root indent + 3px for the rows' selection border so
                        // the icon lines up with top-level rows.
                        .pl(px(TREE_INDENT_OFFSET + 3.))
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .font_family(get_mono_font_family())
                        .bg(sticky_bg)
                        .border_b_1()
                        .border_color(border_color)
                        .child(Icon::new(IconName::FolderOpen).text_color(icon_color))
                        .child(
                            Label::new(path)
                                .text_color(label_color)
                                .text_ellipsis()
                                .whitespace_nowrap(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.key_tree_list_state.update(cx, |state, cx| {
                                state.scroll_to_item(IndexPath::new(deep_ix), ScrollStrategy::Top, window, cx);
                            });
                        })),
                )
            })
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
        let recent_scope = recent_keys_scope(server_id.as_str(), server_state.db());
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
        let show_key_tree_ttl = self.server_state.read(cx).show_key_tree_ttl();
        let ttl_filter = self.state.selected_ttl_filter;
        // Always offer the tag filter next to Type/TTL (not buried in ⋯ and
        // not gated on has_any_records — empty servers still show "All keys").
        let tag_filter_active = self.state.selected_tag_filter;

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
                let recent_keys = get_recent_keys_manager()
                    .records(recent_scope.as_str())
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
                    Some(Icon::new(CustomIconName::Activity)),
                    i18n_key_tree(cx, "recent_keys"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        if recent_keys.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_recent_keys"));
                        } else {
                            for key in &recent_keys {
                                let key_clone = key.clone();
                                submenu = submenu.menu_element(
                                    Box::new(KeyTreeAction::SelectRecentKey(key.clone())),
                                    move |_, _cx| Label::new(key_clone.clone()).text_ellipsis(),
                                );
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::ClearRecentKeys),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_recent_keys")),
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
                // Tag colour filter (local metadata AND). Always visible so
                // it is discoverable next to Type / TTL — not only after the
                // first tag exists and not only under the ⋯ menu.
                .submenu_with_icon(
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
                // Local TTL-range filter (AND with type + tag). Only when
                // tree TTL chips are enabled — otherwise `key_ttls` is empty
                // and every constrained filter would yield an empty tree.
                .when(show_key_tree_ttl, |this| {
                    this.submenu_with_icon(
                        Some(Icon::new(CustomIconName::Clock3)),
                        i18n_key_tree(cx, "ttl_filter"),
                        window,
                        cx,
                        move |submenu, _window, _cx| {
                            let mut submenu = submenu.menu_element_with_check(
                                matches!(ttl_filter, TtlFilter::All),
                                Box::new(KeyTreeAction::SetTtlFilter(TtlFilter::All.as_str().into())),
                                move |_, cx| Label::new(i18n_key_tree(cx, "ttl_filter_all")),
                            );
                            for (filter, label_key) in [
                                (TtlFilter::NoTtl, "ttl_filter_no_ttl"),
                                (TtlFilter::Expiring, "ttl_filter_expiring"),
                                (TtlFilter::Lt1h, "ttl_filter_lt_1h"),
                                (TtlFilter::Lt1d, "ttl_filter_lt_1d"),
                                (TtlFilter::Lt7d, "ttl_filter_lt_7d"),
                                (TtlFilter::Gte7d, "ttl_filter_gte_7d"),
                            ] {
                                let id: SharedString = filter.as_str().into();
                                submenu = submenu.menu_element_with_check(
                                    ttl_filter == filter,
                                    Box::new(KeyTreeAction::SetTtlFilter(id)),
                                    move |_, cx| Label::new(i18n_key_tree(cx, label_key)),
                                );
                            }
                            submenu
                        },
                    )
                })
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
                // Multi-select is local UI (enables bulk export); allowed in RO.
                .when(Capability::ToggleMultiSelect.allowed(readonly), |this| {
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
                .menu_element_with_icon(
                    Icon::new(CustomIconName::Rss),
                    Box::new(KeyTreeAction::ChangeChannelMode),
                    move |_, cx| Label::new(i18n_key_tree(cx, "pubsub_mode")),
                )
            });

        h_flex()
            .flex_shrink_0()
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .w_full()
            .gap_x_2()
            .child(keyword_input)
            .child({
                let can_create = Capability::CreateKey.allowed(readonly);
                Button::new("key-tree-add-btn")
                    .disabled(!can_create)
                    .when(!can_create, |this| this.tooltip(i18n_common(cx, "disable_in_readonly")))
                    .when(can_create, |this| {
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
                    }))
            })
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
                KeyTreeAction::BatchTagSelectedKeys => {
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
                    let server_id: SharedString = this.server_state.read(cx).server_id().to_string().into();
                    let weak_tree = cx.entity().downgrade();
                    let on_done: OnTagDialogDone = std::sync::Arc::new(move |cx| {
                        if let Some(tree) = weak_tree.upgrade() {
                            // Batch may flip visibility under a tag filter
                            // and always affects folder aggregates → rebuild.
                            tree.update(cx, |this, cx| this.update_key_tree(true, cx));
                        }
                    });
                    open_batch_key_tag_dialog(server_id, keys, window, cx, Some(on_done));
                }
                KeyTreeAction::SetTagFilter(color_name) => {
                    let new_filter = if color_name.is_empty() {
                        None
                    } else {
                        TagColor::from_str(color_name.as_ref())
                    };
                    if this.state.selected_tag_filter != new_filter {
                        this.state.selected_tag_filter = new_filter;
                        // Local-only: rebuild the tree from the cached
                        // SCAN snapshot + metadata. No re-SCAN.
                        this.update_key_tree(true, cx);
                    }
                }
                KeyTreeAction::SetTtlFilter(id) => {
                    let new_filter = if id.is_empty() {
                        TtlFilter::All
                    } else {
                        TtlFilter::from_name(id.as_ref())
                    };
                    if this.state.selected_ttl_filter != new_filter {
                        this.state.selected_ttl_filter = new_filter;
                        this.update_key_tree(true, cx);
                    }
                }
                KeyTreeAction::SelectFavoriteKey(key) => {
                    this.select_item(key.clone(), false, false, false, cx);
                }
                KeyTreeAction::CopyKeyName(key) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
                    window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                }
                KeyTreeAction::CopyFolderPrefix(id) => {
                    // Trailing separator matches the folder's scan prefix
                    // (same shape RefreshFolder uses).
                    cx.write_to_clipboard(ClipboardItem::new_string(format!("{}:", id.as_str())));
                    window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                }
                KeyTreeAction::RenameKey(key) => {
                    // Select first so the editor's rename dialog prefills this
                    // key; emit_editor_action re-checks Capability::RenameKey.
                    let key = key.clone();
                    this.server_state.update(cx, |state, cx| {
                        state.select_key(key, cx);
                        state.emit_editor_action(EditorAction::Rename, cx);
                    });
                }
                KeyTreeAction::ToggleFavoriteKey(key) => {
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let key = key.clone();
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let manager = get_favorites_manager();
                                let is_favorited = manager
                                    .records(&server_id)
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|k| k.as_ref() == key.as_ref());
                                if is_favorited {
                                    let _ = manager.remove_record(&server_id, key.as_ref());
                                } else {
                                    let _ = manager.add_record(&server_id, key.as_ref());
                                }
                            })
                            .await;
                    })
                    .detach();
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
                KeyTreeAction::SelectRecentKey(key) => {
                    this.select_item(key.clone(), false, false, false, cx);
                }
                KeyTreeAction::ClearRecentKeys => {
                    let server_state = this.server_state.read(cx);
                    let scope = recent_keys_scope(server_state.server_id(), server_state.db());
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let _ = get_recent_keys_manager().clear_history(&scope);
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
