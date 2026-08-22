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

//! ⌘K command palette: fuzzy-search configured servers, navigation
//! commands, and the active connection's *loaded* keys, keyboard-driven.
//! Whole-keyspace search (a fresh SCAN per query) stays out of scope and is
//! served by the ⌘F key-tree filter instead.

use crate::connection::get_servers;
use crate::db::get_favorites_manager;
use crate::helpers::{ShortcutsAction, fuzzy_score_prepared, prepare_fuzzy_query};
use crate::states::{
    Route, ServerView, ZedisGlobalStore, ZedisServerState, command_status_label, i18n_command_palette, i18n_shortcuts,
};
use gpui::{Context, FocusHandle, Focusable, KeyDownEvent, ScrollHandle, Window, div, prelude::*, px};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::{ActiveTheme, label::Label, v_flex};
use std::mem::take;

/// Cap on loaded-key matches shown in the palette. Keeps a non-empty query
/// on a large keyspace from turning tens of thousands of loaded keys into
/// palette rows (and from scoring/sorting an unbounded list per keystroke).
const KEY_RESULT_CAP: usize = 50;

/// Leading sigils that switch the palette's scope (VS Code style): `>` lists
/// navigation commands/pages, `*` lists the active server's favorites. With
/// neither, the palette searches servers + the active connection's loaded keys.
const CMD_SIGIL: char = '>';
const FAV_SIGIL: char = '*';

/// Which slice of candidates the palette shows, selected by a leading sigil.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// No sigil: configured servers + (when typing) loaded keys.
    General,
    /// `>`: navigation commands / pages + the shortcuts reference.
    Commands,
    /// `*`: the active server's favorited keys.
    Favorites,
}

/// What activating a palette row does.
#[derive(Clone)]
enum PaletteCommand {
    /// Connect to / switch to a configured server (by id).
    Server(String),
    /// Navigate to a route.
    Route(Route),
    /// Jump to a loaded key on the active server (select it + open the
    /// editor). Only built for a non-empty query in a server context.
    Key(gpui::SharedString),
    /// Open the keyboard-shortcuts reference overlay (⌘/). Handed off
    /// to the global `ShortcutsAction` handler so the palette stays
    /// decoupled from the overlay entity.
    ShowShortcuts,
    /// Switch the editor area into Pub/Sub (channel) mode on the active
    /// connection — needs the ServerState entity, so handled like `Key`.
    PubsubMode,
}

struct PaletteItem {
    label: gpui::SharedString,
    /// Secondary muted text (host:port for servers, empty for commands).
    hint: gpui::SharedString,
    /// String the fuzzy query is scored against.
    search: String,
    /// Score already computed against the *current* query during
    /// `build_items` (key rows are scored there to cap them) — `ranked`
    /// reuses it instead of scoring the same string a second time.
    prescore: Option<i32>,
    command: PaletteCommand,
}

/// One `<sigil> label` segment of the palette footer's scope legend.
fn scope_hint(sigil: &str, label: gpui::SharedString, chip_bg: gpui::Hsla, text: gpui::Hsla) -> impl IntoElement {
    gpui_component::h_flex()
        .gap_1p5()
        .items_center()
        .child(
            div()
                .px_1()
                .rounded_sm()
                .bg(chip_bg)
                .child(Label::new(sigil.to_string()).text_xs()),
        )
        .child(Label::new(label).text_xs().text_color(text))
}

pub struct ZedisCommandPalette {
    /// The active connection's state — source of loaded keys for deep search.
    server_state: gpui::Entity<ZedisServerState>,
    /// Favorited keys for the active server, snapshotted when the palette
    /// opens so the per-keystroke `build_items` stays DB- and alloc-light.
    favorites: Vec<gpui::SharedString>,
    open: bool,
    query: gpui::Entity<gpui_component::input::InputState>,
    selected: usize,
    focus_handle: FocusHandle,
    /// Set when the palette is opened; `render` consumes it to reset
    /// and focus the search input. `toggle` runs from a focus-
    /// independent global action handler that has no `Window`, so the
    /// actual input focus is deferred to the next render.
    pending_focus: bool,
    /// Focus that was live when the palette opened; handed back on close.
    prev_focus: Option<FocusHandle>,
    /// Set on close; the next (closed) render restores `prev_focus` — or
    /// blurs when there is none. Deferred because `toggle` has no `Window`.
    pending_restore: bool,
    /// Scroll handle for the results list. The list overflows the
    /// fixed-height panel, so keyboard navigation calls
    /// `scroll_to_item` to keep the selected row in view — otherwise
    /// selection silently moves off-screen and looks like it ran past
    /// the last element.
    scroll_handle: ScrollHandle,
    /// Built candidate list + ranking, cached under `items_signature`:
    /// `render` runs on every notify (arrow-key selection moves, hover)
    /// but only a changed query — or a key-tree page landing mid-scan —
    /// should re-clone the server list and re-score thousands of keys.
    cached_items: Vec<PaletteItem>,
    cached_order: Vec<usize>,
    /// `(scope, query, key_tree_id)` the cache was built for; `None`
    /// forces a rebuild (set on every open so favorites/server edits
    /// from the previous session are re-read).
    items_signature: Option<(Scope, String, String)>,
}

impl ZedisCommandPalette {
    pub fn new(server_state: gpui::Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(i18n_command_palette(cx, "search_placeholder"))
        });
        Self {
            server_state,
            favorites: Vec::new(),
            open: false,
            query,
            selected: 0,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            prev_focus: None,
            pending_restore: false,
            scroll_handle: ScrollHandle::new(),
            cached_items: Vec::new(),
            cached_order: Vec::new(),
            items_signature: None,
        }
    }

    /// Rebind to another tab's server state (the root swaps this on
    /// workspace-tab switch so the palette searches the active tab's keys).
    pub fn set_server_state(&mut self, server_state: gpui::Entity<ZedisServerState>) {
        self.server_state = server_state;
    }

    /// Open the palette (or close it if already open). `render`
    /// performs the input reset+focus on open via `pending_focus`,
    /// since this is invoked from a global action handler with no
    /// `Window`.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.selected = 0;
            self.pending_focus = true;
            // Rebuild on open: servers/favorites/route may have changed
            // since the cache was last filled.
            self.items_signature = None;
            // Snapshot the active server's favorites once per open — the
            // build_items run on every keystroke must stay DB-free.
            let server_id = self.server_state.read(cx).server_id().to_string();
            self.favorites = get_favorites_manager()
                .records(&server_id)
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect();
            // The ScrollHandle keeps its offset across open/close, so
            // without this the list stays scrolled where it was last
            // time while the selection is back at the top.
            self.scroll_handle.set_offset(gpui::Point::default());
        } else {
            self.pending_restore = true;
        }
        cx.notify();
    }

    /// Close after executing a command: the route may just have changed,
    /// so the pre-open focus target could be gone — blur instead of
    /// restoring. Critical either way: when open we focus the search
    /// input, and the closed render drops that element; leaving focus
    /// orphaned on a handle no longer in the tree kills every
    /// focus-routed keyboard action.
    fn close(&mut self, cx: &mut Context<Self>) {
        self.prev_focus = None;
        self.dismiss(cx);
    }

    /// Close and hand focus back to whatever had it before the palette
    /// opened (Esc / backdrop / ⌘K re-toggle). A bare `blur()` leaves the
    /// window with nothing focused, so every focus-routed keybinding dies
    /// until the next click. The restore runs in the next (closed) render
    /// pass, which — unlike `toggle` — has a `Window`.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_restore = true;
        cx.notify();
    }

    /// Split the raw input into (scope, effective query). A leading `>` or `*`
    /// sigil selects the scope; the remaining text is what actually gets
    /// matched. With no sigil the trimmed input searches the General scope.
    fn parse_query(&self, cx: &Context<Self>) -> (Scope, String) {
        let raw = self.query.read(cx).value().to_string();
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix(CMD_SIGIL) {
            (Scope::Commands, rest.trim().to_string())
        } else if let Some(rest) = trimmed.strip_prefix(FAV_SIGIL) {
            (Scope::Favorites, rest.trim().to_string())
        } else {
            (Scope::General, raw.trim().to_string())
        }
    }

    /// Build the candidate list for the current scope. General lists servers
    /// first, then (on a non-empty query) loaded-key matches; `>` lists
    /// navigation commands + the shortcuts reference; `*` lists the active
    /// server's favorites. `query` is the effective query (sigil stripped);
    /// `ranked` scores against the same string.
    fn build_items(&self, scope: Scope, query: &str, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = Vec::new();

        // `selected_server` can linger after navigating back to Home, so a
        // server is "in context" only when one is selected AND we're not on a
        // global page — otherwise Home would list server-scoped entries too.
        let (has_server, current_route) = {
            let state = cx.global::<ZedisGlobalStore>().read(cx);
            (state.selected_server().is_some(), state.route())
        };
        let in_server_context = has_server && !matches!(current_route, Route::Home | Route::Settings);

        match scope {
            // `*` — the active server's starred keys (a leading star marks
            // them). Kept out of the General list so servers stay up top; this
            // is the keyboard-fast path to favorites.
            Scope::Favorites => {
                if in_server_context {
                    for key in &self.favorites {
                        items.push(PaletteItem {
                            label: format!("★ {key}").into(),
                            hint: gpui::SharedString::default(),
                            search: key.to_string(),
                            prescore: None,
                            command: PaletteCommand::Key(key.clone()),
                        });
                    }
                }
            }

            // `>` — navigation commands / pages, then the shortcuts reference.
            Scope::Commands => {
                // Server views are offered against the current connection —
                // `None` (not on a server route) hides them, replacing the old
                // `needs_server` gate.
                let conn = current_route.server();
                let view_route = |view: ServerView| conn.clone().map(|(id, db)| Route::Server { id, db, view });
                // Same capability gates as the status-bar Tools menu — the
                // menu shows those entries disabled with a reason; a palette
                // row can't carry that affordance, so gated tools are hidden
                // outright instead of failing after navigation.
                let (supports_search, supports_acl, supports_functions, supports_topology) = {
                    let state = self.server_state.read(cx);
                    (
                        state.supports_search(),
                        state.supports_acl(),
                        state.supports_functions(),
                        state.supports_topology(),
                    )
                };
                // (i18n key, target, available) — order defines empty-query
                // display order.
                let commands: [(&str, Option<Route>, bool); 19] = [
                    ("cmd_home", Some(Route::Home), true),
                    ("cmd_editor", view_route(ServerView::Editor), true),
                    ("cmd_metrics", view_route(ServerView::Metrics), true),
                    ("cmd_performance", view_route(ServerView::Slowlog), true),
                    ("cmd_memory", view_route(ServerView::MemoryAnalysis), true),
                    ("cmd_clients", view_route(ServerView::Clients), true),
                    ("cmd_monitor", view_route(ServerView::Monitor), true),
                    ("cmd_server_load", view_route(ServerView::ServerLoad), true),
                    ("cmd_persistence", view_route(ServerView::Persistence), true),
                    (
                        "cmd_keyspace_notifications",
                        view_route(ServerView::KeyspaceNotifications),
                        true,
                    ),
                    ("cmd_server_info", view_route(ServerView::ServerInfo), true),
                    ("cmd_topology", view_route(ServerView::Topology), supports_topology),
                    ("cmd_config", view_route(ServerView::Config), true),
                    ("cmd_acl", view_route(ServerView::Acl), supports_acl),
                    ("cmd_value_search", view_route(ServerView::ValueSearch), true),
                    ("cmd_search", view_route(ServerView::Search), supports_search),
                    ("cmd_functions", view_route(ServerView::Functions), supports_functions),
                    ("cmd_lua_scripts", view_route(ServerView::LuaScripts), true),
                    ("cmd_settings", Some(Route::Settings), true),
                ];
                for (key, route, available) in commands {
                    if !available {
                        continue;
                    }
                    let Some(route) = route else { continue };
                    // Don't offer to navigate to the page we're already on.
                    if route == current_route {
                        continue;
                    }
                    // A panel the probe found unusable on this server stays
                    // listed (navigating lands on the explanatory placeholder)
                    // but carries the reason as its hint.
                    let hint = route
                        .server_view()
                        .and_then(|view| self.server_state.read(cx).panel_block(view))
                        .map(|(command, status)| command_status_label(cx, command, status))
                        .unwrap_or_default();
                    let label = i18n_command_palette(cx, key);
                    items.push(PaletteItem {
                        label: label.clone(),
                        hint,
                        search: label.to_string(),
                        prescore: None,
                        command: PaletteCommand::Route(route),
                    });
                }
                // Pub/Sub mode only makes sense against a connection.
                if conn.is_some() {
                    let label = i18n_command_palette(cx, "cmd_pubsub");
                    items.push(PaletteItem {
                        label: label.clone(),
                        hint: gpui::SharedString::default(),
                        search: label.to_string(),
                        prescore: None,
                        command: PaletteCommand::PubsubMode,
                    });
                }
                let shortcuts_label = i18n_shortcuts(cx, "title");
                items.push(PaletteItem {
                    label: shortcuts_label.clone(),
                    hint: gpui::SharedString::default(),
                    search: shortcuts_label.to_string(),
                    prescore: None,
                    command: PaletteCommand::ShowShortcuts,
                });
            }

            // No sigil — configured servers first, then (on a non-empty query)
            // the active connection's loaded keys. Full-keyspace search stays
            // with the ⌘F tree filter; we only score the SCAN-paginated subset
            // the tree holds and cap at `KEY_RESULT_CAP`, so a big keyspace
            // never materialises tens of thousands of rows per keystroke.
            Scope::General => {
                for server in get_servers().unwrap_or_default() {
                    let hint = format!("{}:{}", server.host, server.port);
                    // Match against name + address so "10.0" finds a server too.
                    let search = format!("{} {hint}", server.name);
                    items.push(PaletteItem {
                        label: server.name.clone().into(),
                        hint: hint.into(),
                        search,
                        prescore: None,
                        command: PaletteCommand::Server(server.id.clone()),
                    });
                }
                // Scored live on every keystroke: the loaded set is the SCAN-
                // paginated subset the tree holds (not the whole keyspace), and
                // fuzzy-scoring a few thousand short keys is sub-millisecond, so
                // no debounce is needed. Whole-keyspace search lives in the ⌘F
                // tree filter.
                if in_server_context && !query.is_empty() {
                    let state = self.server_state.read(cx);
                    // Lowercase the query once for the whole batch.
                    let prepared = prepare_fuzzy_query(query);
                    let mut scored: Vec<(i32, gpui::SharedString, &'static str)> = state
                        .keys()
                        .iter()
                        .filter_map(|(k, t)| fuzzy_score_prepared(&prepared, k).map(|s| (s, k.clone(), t.as_str())))
                        .collect();
                    scored.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
                    scored.truncate(KEY_RESULT_CAP);
                    for (score, key, type_hint) in scored {
                        items.push(PaletteItem {
                            label: key.clone(),
                            hint: type_hint.into(),
                            search: key.to_string(),
                            // Carry the score so `ranked` doesn't fuzzy-match
                            // the same key against the same query again.
                            prescore: Some(score),
                            command: PaletteCommand::Key(key),
                        });
                    }
                }
            }
        }

        items
    }

    /// Filter+rank `items` by `query`. Returns indices into `items`, best
    /// match first. Empty query keeps the natural (insertion) order.
    fn ranked(items: &[PaletteItem], query: &str) -> Vec<usize> {
        let prepared = prepare_fuzzy_query(query);
        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| match it.prescore {
                // Key rows were already scored against this same query in
                // `build_items` — reuse instead of scoring twice.
                Some(s) => Some((s, i)),
                None => fuzzy_score_prepared(&prepared, &it.search).map(|s| (s, i)),
            })
            .collect();
        // Stable sort, higher score first. Equal scores (e.g. empty
        // query → all 0) keep insertion order (sort_by_key is stable).
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    fn execute(&mut self, command: &PaletteCommand, window: &mut Window, cx: &mut Context<Self>) {
        let command = command.clone();
        // The shortcuts overlay is owned by the `Zedis` root, not the
        // palette; close here and let its global action handler open it.
        if let PaletteCommand::ShowShortcuts = command {
            self.close(cx);
            window.dispatch_action(Box::new(ShortcutsAction::Toggle), cx);
            return;
        }
        // Selecting a key needs the per-connection ServerState entity (not the
        // global store), so handle it here rather than in the update_global
        // block below: select the key, then jump to the editor.
        if let PaletteCommand::Key(key) = &command {
            let key = key.clone();
            self.server_state.update(cx, |state, cx| state.select_key(key, cx));
            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
            });
            self.close(cx);
            return;
        }
        // Like `Key`: needs the per-connection ServerState entity.
        if let PaletteCommand::PubsubMode = &command {
            self.server_state.update(cx, |state, cx| state.change_channel_mode(cx));
            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
            });
            self.close(cx);
            return;
        }
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| match command {
                PaletteCommand::Server(id) => {
                    let db = state.open_db_for(&id);
                    state.connect_server(id, db, cx);
                }
                PaletteCommand::Route(route) => {
                    // Mirror the sidebar Home button: returning to Home leaves
                    // the server context, so clear the selected connection
                    // (which itself routes Home) — otherwise the sidebar keeps
                    // the old server row highlighted instead of Home.
                    if route == Route::Home {
                        state.clear_selected_server(cx);
                    } else {
                        state.go_to(route, cx);
                    }
                }
                // Handled above (early return); arms kept for exhaustiveness.
                PaletteCommand::ShowShortcuts => {}
                PaletteCommand::Key(_) => {}
                PaletteCommand::PubsubMode => {}
            });
        });
        self.close(cx);
    }
}

impl Focusable for ZedisCommandPalette {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZedisCommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            // Zero-footprint when closed; hand back (or drop) the focus the
            // palette took, deferred here from the window-less close paths.
            if take(&mut self.pending_restore) {
                match self.prev_focus.take() {
                    Some(prev) => prev.focus(window, cx),
                    None => window.blur(),
                }
            }
            return div().into_any_element();
        }

        // Deferred from `toggle` (global action handler has no Window):
        // reset and focus the search input on the first render after open.
        if self.pending_focus {
            self.pending_focus = false;
            // Remember the pre-open focus so a dismissal can hand it back.
            self.prev_focus = window.focused(cx);
            self.query.update(cx, |state, cx| {
                state.set_value(gpui::SharedString::default(), window, cx);
                state.focus(window, cx);
            });
        }

        let (scope, query_str) = self.parse_query(cx);
        // Rebuild + re-rank only when the inputs actually changed: the
        // query text, the scope sigil, or the loaded key set (a scan page
        // landing while the palette is open bumps `key_tree_id`). Plain
        // notifies — arrow-key selection, hover — reuse the cache.
        let key_tree_id = self.server_state.read(cx).key_tree_id().to_string();
        let signature_current = self
            .items_signature
            .as_ref()
            .is_some_and(|(s, q, id)| *s == scope && q == &query_str && id == &key_tree_id);
        if !signature_current {
            self.cached_items = self.build_items(scope, &query_str, cx);
            self.cached_order = Self::ranked(&self.cached_items, &query_str);
            self.items_signature = Some((scope, query_str.clone(), key_tree_id));
        }
        let items = &self.cached_items;
        let order = &self.cached_order;
        let count = order.len();
        // Clamp selection to the filtered list.
        let selected = if count == 0 { 0 } else { self.selected.min(count - 1) };

        let theme = cx.theme();
        let panel_bg = theme.background;
        let border = theme.border;
        let active = theme.list_active;
        let muted = theme.muted_foreground;
        let radius = theme.radius;
        let radius_lg = theme.radius_lg;

        // Rows are direct children of this tracked, scrollable
        // container so `scroll_handle.scroll_to_item(ix)` lines up with
        // the row index. `max_h` caps the list and triggers scrolling;
        // because the panel is no longer stretched (backdrop uses
        // `items_start`), the list sizes to its content below the cap,
        // so the popup is adaptive on Home. The sibling scrollbar
        // (added on the wrapper) reads the same handle.
        let mut list = v_flex()
            .id("zedis-palette-list")
            .w_full()
            .gap_0p5()
            .p_1()
            .max_h(px(360.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle);
        if count == 0 {
            list = list.child(
                Label::new(i18n_command_palette(cx, "empty"))
                    .text_sm()
                    .text_color(muted)
                    .p_2(),
            );
        } else {
            for (row, &item_idx) in order.iter().enumerate() {
                let it = &items[item_idx];
                let is_sel = row == selected;
                let cmd = it.command.clone();
                let mut r = gpui_component::h_flex()
                    .id(("zedis-palette-row", row))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(|this| this.bg(active))
                    .child(Label::new(it.label.clone()).text_sm())
                    .when(!it.hint.is_empty(), |this| {
                        this.child(Label::new(it.hint.clone()).text_xs().text_color(muted))
                    })
                    // Rows are stateful interactive divs so a click runs
                    // the command directly. stop_propagation keeps the
                    // click from bubbling to the backdrop close handler.
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.execute(&cmd, window, cx);
                            cx.stop_propagation();
                        }),
                    );
                if is_sel {
                    r = r.bg(active);
                }
                list = list.child(r);
            }
        }

        // Snapshot the resolved command for the keyboard handler so it
        // doesn't need to re-rank inside the listener.
        let chosen: Option<PaletteCommand> = order.get(selected).map(|&i| items[i].command.clone());

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            // The backdrop is a flex row; its default cross-axis
            // align is `stretch`, which would stretch the panel to
            // its `max_h` and leave Home looking half-empty. Align to
            // the top so the panel stays content-sized (adaptive).
            .items_start()
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    // Click on the dim backdrop closes.
                    this.dismiss(cx);
                }),
            )
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.dismiss(cx);
                        cx.stop_propagation();
                    }
                    "down" => {
                        if count > 0 {
                            this.selected = (selected + 1).min(count - 1);
                            // Keep the (clamped) selection on-screen;
                            // without this the highlight scrolls out of
                            // the fixed-height list and looks like it
                            // ran past the last item.
                            this.scroll_handle.scroll_to_item(this.selected);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                    "up" => {
                        this.selected = selected.saturating_sub(1);
                        this.scroll_handle.scroll_to_item(this.selected);
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "enter" => {
                        if let Some(cmd) = &chosen {
                            this.execute(cmd, window, cx);
                        }
                        cx.stop_propagation();
                    }
                    // Everything else (typing) falls through to the input.
                    _ => {}
                }
            }))
            .child(
                v_flex()
                    .mt(px(96.))
                    .w(px(560.))
                    .max_h(px(440.))
                    .bg(panel_bg)
                    .border_1()
                    .border_color(border)
                    .rounded(radius_lg)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx: &mut gpui::App| {
                        // Clicks inside the panel must not bubble to the
                        // backdrop close handler.
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(border)
                            .child(gpui_component::input::Input::new(&self.query)),
                    )
                    .child(
                        // Relative wrapper so the absolutely-positioned
                        // scrollbar overlays the list (not the window),
                        // and stays a sibling of the scroller so it
                        // doesn't scroll with the content.
                        div().relative().child(list).child(
                            // `ScrollbarMode::Always`: the theme
                            // default is `Scrolling`, which only
                            // shows the bar for a brief fade after
                            // the offset *changes* — and keyboard
                            // nav only changes the offset once
                            // selection is pushed past the viewport
                            // (near the bottom), so the bar appeared
                            // to show "only at the bottom". Always
                            // keeps it visible the whole time the
                            // list overflows; it still auto-hides
                            // when the list fits (scrollbar.rs:574).
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .child(Scrollbar::vertical(&self.scroll_handle).mode(ScrollbarMode::Always)),
                        ),
                    )
                    .when(scope == Scope::General, |this| {
                        // Footer: the `>` / `*` scope legend, plus (while typing)
                        // a note that ⌘K key results are the *loaded* subset and
                        // ⌘F searches the whole database — drawing the ⌘K/⌘F line.
                        this.child(
                            v_flex()
                                .w_full()
                                .border_t_1()
                                .border_color(border)
                                .child(
                                    gpui_component::h_flex()
                                        .gap_4()
                                        .px_3()
                                        .py_1p5()
                                        .child(scope_hint(
                                            ">",
                                            i18n_command_palette(cx, "scope_commands"),
                                            active,
                                            muted,
                                        ))
                                        .child(scope_hint(
                                            "*",
                                            i18n_command_palette(cx, "scope_favorites"),
                                            active,
                                            muted,
                                        )),
                                )
                                .when(!query_str.is_empty(), |this| {
                                    this.child(
                                        Label::new(i18n_command_palette(cx, "keys_hint"))
                                            .text_xs()
                                            .text_color(muted)
                                            .px_3()
                                            .pb_1p5(),
                                    )
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}
