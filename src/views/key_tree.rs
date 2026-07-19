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
    Action, Anchor, App, AppContext, ClipboardItem, Entity, FocusHandle, Focusable, FontWeight, Hsla, ScrollStrategy,
    SharedString, Subscription, Task, Window, div, prelude::*, px, rgb,
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
use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::info;
use zedis_ui::{ZedisDialog, ZedisFormField, ZedisFormFieldType, ZedisFormOptions, ZedisSkeletonLoading};

// Constants for tree layout and behavior
mod actions;
mod build;
mod delegate;
mod render;

use actions::KeyTreeAction;

use build::*;
use delegate::*;

const TREE_INDENT_BASE: f32 = 16.0; // Base indentation per level in pixels
const TREE_INDENT_OFFSET: f32 = 8.0; // Additional offset for all items
const EXPANDED_ITEMS_INITIAL_CAPACITY: usize = 10;
/// Coalescing window for `KeyTreeUpdated` bursts: the scanner emits one event
/// per SCAN page, so a large keyspace would otherwise trigger a full-tree
/// rebuild (clone + sort + build) hundreds of times per scan.
const KEY_TREE_UPDATE_COALESCE: Duration = Duration::from_millis(100);
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

    /// Trailing-edge rebuild scheduled while scan pages are streaming in;
    /// dropping it cancels the pending rebuild.
    pending_tree_update: Option<Task<()>>,

    /// When the last `KeyTreeUpdated`-driven rebuild started — the
    /// leading-edge check in `schedule_key_tree_update`.
    last_tree_update_at: Option<Instant>,

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
    /// Put the caret in the keyword filter (`EditorAction::Search` / ⌘F).
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.keyword_state.focus_handle(cx).focus(window, cx);
    }

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
                    this.schedule_key_tree_update(cx);
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
                    // current keyword + query mode, keeping expanded folders.
                    this.handle_filter(false, cx);
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
                    // Flush any coalesced rebuild first so the expansion
                    // pass below sees the final key set.
                    this.flush_key_tree_update(cx);
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
                // Placeholder carries the focus shortcut (⌘F / Ctrl+F via
                // `humanize_keystroke`, matching `EditorAction::Search`'s
                // `secondary-f` binding) so the affordance is discoverable
                // right where it lands. Single-line — a `\n` in a
                // placeholder panics the wrapped-lines cache (see CLAUDE.md).
                .placeholder(format!(
                    "{} ({})",
                    i18n_common(cx, "keyword_placeholder"),
                    humanize_keystroke("cmd-f")
                ))
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
                // Explicit search from the box → always a fresh query.
                view.handle_filter(true, cx);
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
            pending_tree_update: None,
            last_tree_update_at: None,
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

    /// Coalesce `KeyTreeUpdated` bursts into at most one rebuild per
    /// [`KEY_TREE_UPDATE_COALESCE`] window. The first event of a burst
    /// rebuilds immediately (so the first scan page renders at once); the
    /// rest fold into one trailing rebuild, which snapshots whatever pages
    /// have landed by the time it fires — no event is ever lost, later
    /// pages just ride along.
    fn schedule_key_tree_update(&mut self, cx: &mut Context<Self>) {
        if self.pending_tree_update.is_some() {
            // The pending rebuild reads server state when it fires, so it
            // already covers this event.
            return;
        }
        let since_last = self.last_tree_update_at.map(|at| at.elapsed()).unwrap_or(Duration::MAX);
        if since_last >= KEY_TREE_UPDATE_COALESCE {
            self.last_tree_update_at = Some(Instant::now());
            self.update_key_tree(true, cx);
            return;
        }
        let wait = KEY_TREE_UPDATE_COALESCE - since_last;
        self.pending_tree_update = Some(cx.spawn(async move |handle, cx| {
            cx.background_executor().timer(wait).await;
            let _ = handle.update(cx, |this, cx| {
                this.pending_tree_update = None;
                this.last_tree_update_at = Some(Instant::now());
                this.update_key_tree(true, cx);
            });
        }));
    }

    /// Run any coalesced rebuild now — used on `KeyScanFinished` so
    /// follow-up work (auto-expansion) operates on the final key set.
    fn flush_key_tree_update(&mut self, cx: &mut Context<Self>) {
        if self.pending_tree_update.take().is_some() {
            self.last_tree_update_at = Some(Instant::now());
            self.update_key_tree(true, cx);
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
            // Arc share, not a structural copy — the server side mutates
            // its map through `Arc::make_mut`, so this snapshot stays
            // immutable for the background build while writes COW at most
            // once per build window.
            self.state.cached_key_ttls = server_state.key_ttls_arc();
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
                    // otherwise the SCAN snapshot is the source (cloned out
                    // of the Arc only on this path — the union path builds
                    // its own Vec). Then local AND of type + tag + TTL.
                    let source = match tag_filter_snapshot {
                        Some(color) => build_tagged_keys_list(color, &keys_snapshot, &metadata_snapshot),
                        None => (*keys_snapshot).clone(),
                    };
                    let keys_input = apply_local_key_filters(
                        source,
                        type_filter_snapshot,
                        tag_filter_snapshot,
                        ttl_filter_snapshot,
                        &key_ttls_snapshot,
                        &metadata_snapshot,
                    );
                    let mut items = new_key_tree_items(KeyTreeBuildInput {
                        keys: keys_input,
                        keyword,
                        expanded_items,
                        suppressed,
                        separator: &separator,
                        max_key_tree_depth,
                        key_ttls: &key_ttls_snapshot,
                        metadata: &metadata_snapshot,
                    });
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

    /// Run a keyword filter. `force_new_query` marks a scan the user kicked
    /// off from the search box (Enter / the search button / a history pick):
    /// it is always treated as a brand-new query, so the tree fully collapses
    /// and every lazy-load folder's expansion is dropped — even when the
    /// keyword is unchanged. Only the refresh paths (⌘R / ⋯ "Refresh keys" /
    /// auto-refresh) pass `false` to keep the expanded folders in place. This
    /// is what stops an in-flight "Load more" from leaving `string:` expanded
    /// but half-loaded after the user re-searches the same (e.g. empty) term:
    /// a fresh global scan never restores a folder's per-prefix "Load more",
    /// so the folder must reset to a clean, collapsed state instead.
    fn handle_filter(&mut self, force_new_query: bool, cx: &mut Context<Self>) {
        // Don't trigger filter while already scanning
        let server_state_clone = self.server_state.clone();
        let server_state = self.server_state.read(cx);
        if server_state.scanning() {
            return;
        }

        let keyword = self.keyword_state.read(cx).value();
        // Same keyword + query mode as the displayed tree ⇒ a refresh, not a
        // new query: keep the folder-expanded state (the `KeyScanReset` handler
        // consumes this flag). An explicit search-box search (`force_new_query`)
        // is never a refresh — it always collapses, so no folder is left
        // expanded-but-half-loaded.
        // `last_scan` is owned by the `KeyScanFinished` handler (so the
        // *initial* load — which never calls `handle_filter` — also
        // seeds it). Here we only read it to tell a refresh apart from
        // a new query.
        let scan_sig = (keyword.clone(), self.state.query_mode);
        self.state.preserve_expand_on_scan = !force_new_query && self.state.last_scan.as_ref() == Some(&scan_sig);
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
                    state.set_search_history(history.into_iter().map(Into::into).collect());
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
}
