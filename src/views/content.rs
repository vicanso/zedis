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
    helpers::{EditorAction, get_key_tree_widths},
    states::{
        GlobalEvent, Route, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, i18n_common,
        update_app_state_and_save_quiet_debounced,
    },
    views::{
        ZedisAclManager, ZedisClientsManager, ZedisConfigEditor, ZedisEditor, ZedisFunctionEditor, ZedisKeyTree,
        ZedisKeyspaceNotifications, ZedisLuaScriptLibrary, ZedisMemoryAnalysis, ZedisMetrics, ZedisMonitor,
        ZedisPersistence, ZedisProtoEditor, ZedisScriptEditor, ZedisSearchManager, ZedisServerLoad, ZedisServers,
        ZedisSlowlogEditor, ZedisStatusBar, ZedisTerminal, ZedisTopology, ZedisValueSearch,
    },
};
use gpui::{AnyView, Entity, FocusHandle, Pixels, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    resizable::{ResizableState, h_resizable, resizable_panel},
    v_flex,
};
use std::collections::HashMap;
use tracing::{debug, info};
use zedis_ui::ZedisSkeletonLoading;

// Constants for UI dimensions
const LOADING_SKELETON_WIDTH: f32 = 600.0;
const SERVERS_MARGIN: f32 = 8.0;

/// Main content area component for the Zedis application
///
/// Manages the application's main views and routing:
/// - Server list view (Route::Home): Display and manage Redis server connections
/// - Editor view (Route::Server(ServerView::Editor)): Display key tree and value editor for selected server
///
/// Views are lazily initialized. Tool panels (Metrics, Slowlog, …) are dropped
/// when left so they don't hold large scan buffers. The **editor suite**
/// (key tree + value editor + terminal) is kept for the whole server session
/// so switching to Metrics/Slowlog and back preserves expand state, scroll,
/// multi-select, and the search box — and is only dropped when leaving
/// server routes entirely or when the active connection `(id, db)` changes.
pub struct ZedisContent {
    /// Reference to the server state containing Redis connection and data
    server_state: Entity<ZedisServerState>,

    /// Cached views - lazily initialized and cleared when switching routes
    servers: Option<Entity<ZedisServers>>,
    proto_editor: Option<Entity<ZedisProtoEditor>>,
    script_editor: Option<Entity<ZedisScriptEditor>>,
    value_editor: Option<Entity<ZedisEditor>>,
    terminal: Option<Entity<ZedisTerminal>>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    /// Server tool panels (Metrics, Slowlog, Monitor, …) keyed by their route
    /// view — one map instead of fifteen `Option<Entity<…>>` fields. Created
    /// on first visit (`tool_view`), dropped uniformly by `clear_views` when
    /// the route moves elsewhere.
    tool_views: HashMap<ServerView, AnyView>,
    status_bar: Entity<ZedisStatusBar>,

    /// Persisted width of the key tree panel (resizable by user)
    key_tree_width: Pixels,

    /// Cached current route to avoid unnecessary updates
    current_route: Route,
    should_focus: bool,
    focus_handle: FocusHandle,

    /// Whether this content is the active tab. Only the active tab reacts to
    /// the global `RouteChanged` / `ServerSelected` broadcasts, so parallel
    /// tabs don't stomp on each other's connection or views; the
    /// `ServerListUpdated` cleanup still applies to every tab.
    active: bool,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisContent {
    /// Drop key tree / value editor / terminal. Called when leaving server
    /// routes entirely, or when the active `(server_id, db)` changes so UI
    /// state from the previous connection cannot leak.
    fn drop_editor_suite(&mut self) {
        self.key_tree.take();
        self.value_editor.take();
        self.terminal.take();
    }

    fn clear_views(&mut self) {
        let route = self.current_route.clone();
        if route != Route::Home {
            self.servers.take();
        }
        // Editor suite: keep for any `Route::Server` so tool pages don't
        // wipe expand/scroll/multi-select/keyword. Drop only off-server.
        if !route.is_server() {
            self.drop_editor_suite();
        }
        if route != Route::Protos {
            self.proto_editor.take();
        }
        if route != Route::Scripts {
            self.script_editor.take();
        }
        // Tool panels: keep only the one the current route still shows —
        // leaving a tool route drops its panel (and any large scan buffers),
        // same per-view policy as before, now uniform over the map.
        let current_tool = route.server_view();
        self.tool_views.retain(|view, _| Some(*view) == current_tool);
    }
    /// Create a new content view with route-aware view management
    ///
    /// The single, app-lifetime [`ZedisServerState`] entity (reset on server
    /// switch). Shared with the command palette so it can fuzzy-search the
    /// active connection's loaded keys.
    pub fn server_state(&self) -> Entity<ZedisServerState> {
        self.server_state.clone()
    }

    /// The status bar entity, so the root layout (`main.rs`) can render it as a
    /// full-width row beneath the sidebar + content instead of inside this
    /// content column.
    pub fn status_bar(&self) -> Entity<ZedisStatusBar> {
        self.status_bar.clone()
    }

    /// Mark this content as the active tab (or not). Called by the root
    /// (`main.rs`) when the active tab changes; see the `active` field.
    /// Also flips the server state's `background` flag so an inactive tab's
    /// heartbeat drops to the relaxed cadence (and recovers on activation).
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        self.server_state.update(cx, |state, _| state.set_background(!active));
    }

    /// Sets up subscriptions to automatically clean up cached views when
    /// switching routes to optimize memory usage.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let global_state = cx.global::<ZedisGlobalStore>().state();
        let server_state = cx.new(|_cx| ZedisServerState::new());
        let status_bar = cx.new(|cx| ZedisStatusBar::new(server_state.clone(), window, cx));

        subscriptions.push(
            cx.subscribe(&global_state, |this, _global_state, event, cx| match event {
                GlobalEvent::RouteChanged(route) => {
                    // Inactive tabs keep their route/views frozen; the root
                    // re-broadcasts the projected route when a tab is
                    // re-activated.
                    if !this.active {
                        return;
                    }
                    this.current_route = route.clone();
                    this.clear_views();
                    // clear_views drops the previously focused view, so the
                    // window is left with no focus target — global
                    // keybindings (e.g. Esc → back on tool pages) wouldn't
                    // dispatch until the user clicked. Re-focus the content
                    // container so they work immediately. A destination view
                    // that wants its own input focused overrides this when
                    // it renders.
                    this.should_focus = true;
                    cx.notify();
                }
                GlobalEvent::ServerSelected(server_id, db) => {
                    // Only the active tab follows server selection — an
                    // inactive tab must keep its own connection.
                    if !this.active {
                        return;
                    }
                    if server_id.is_empty() {
                        // Disconnect / Home: drop the suite so a later
                        // reconnect never reuses another server's tree.
                        this.drop_editor_suite();
                        cx.notify();
                        return;
                    }
                    // Compare before `select` mutates server_state.
                    let (prev_id, prev_db) = {
                        let s = this.server_state.read(cx);
                        (s.server_id().to_string(), s.db())
                    };
                    let connection_changed = prev_id.as_str() != server_id.as_str() || prev_db != *db;
                    this.server_state.update(cx, |state, cx| {
                        state.select(server_id.clone(), *db, cx);
                    });
                    // Fresh connection ⇒ fresh editor suite (keyword /
                    // multi-select / scroll must not carry over).
                    if connection_changed {
                        this.drop_editor_suite();
                    }
                    // `select` flips `server_status` to Loading — re-render so
                    // the busy skeleton appears immediately (and clears later
                    // when ServerInfoUpdated fires below).
                    cx.notify();
                }
                GlobalEvent::ServerListUpdated => {
                    // If the currently-tracked server was just deleted,
                    // stop the heartbeat from logging "config not found"
                    // every tick by clearing our reference to it.
                    this.server_state.update(cx, |state, cx| {
                        state.clear_if_removed(cx);
                    });
                }
                _ => {}
            }),
        );

        // Content's full-page busy gate reads `server_state.is_busy()`.
        // GPUI does not re-render this entity when only `server_state`
        // notifies, so we must `cx.notify()` ourselves when load status
        // changes — otherwise a cold start can leave the skeleton up after
        // SelectServer finishes (Idle/Failed) until some other event happens.
        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| match event {
                ServerEvent::TerminalToggled(_) => {
                    this.should_focus = true;
                    cx.notify();
                }
                ServerEvent::ServerInfoUpdated | ServerEvent::ServerSelected(_) => {
                    cx.notify();
                }
                _ => {}
            }),
        );

        // Restore persisted key tree width from global state
        let global_store = cx.global::<ZedisGlobalStore>().read(cx);
        let key_tree_width = global_store.key_tree_width();
        let route = global_store.route();
        info!("Creating new content view");

        Self {
            server_state,
            status_bar,
            current_route: route,
            servers: None,
            value_editor: None,
            terminal: None,
            tool_views: HashMap::new(),
            key_tree: None,
            key_tree_width,
            should_focus: false,
            focus_handle,
            active: true,
            proto_editor: None,
            script_editor: None,
            _subscriptions: subscriptions,
        }
    }

    /// Render the server management view (home page)
    fn render_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let servers = self
            .servers
            .get_or_insert_with(|| {
                debug!("Creating new servers view");
                cx.new(|cx| ZedisServers::new(window, cx))
            })
            .clone();
        // A long server list (many connections / groups) must scroll instead
        // of overflowing and clipping the lower cards. Mirror the bounded-box
        // pattern the other routes use: a `flex_1 relative` shell with an
        // `absolute inset_0` child pins the scroll viewport to a definite size
        // (a plain `size_full` flex child still sizes to its content here), so
        // `overflow_y_scroll` has real bounds to scroll within.
        div().flex_1().w_full().relative().child(
            div()
                .id("servers-scroll")
                .absolute()
                .inset_0()
                .size_full()
                .overflow_y_scroll()
                // Inset via padding, not margin: a scroll viewport often omits
                // its content's trailing margin from the scrollable height, so
                // a bottom margin would clip the last row. Padding on this
                // in-flow child is part of its box height and always scrolls
                // into reach.
                .child(div().p(px(SERVERS_MARGIN)).child(servers)),
        )
    }

    fn render_proto_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let proto_editor = self
            .proto_editor
            .get_or_insert_with(|| {
                debug!("Creating new proto editor view");
                cx.new(|cx| ZedisProtoEditor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(proto_editor)
    }

    fn render_script_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let script_editor = self
            .script_editor
            .get_or_insert_with(|| {
                debug!("Creating new script editor view");
                cx.new(|cx| ZedisScriptEditor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(script_editor)
    }

    /// Cached tool-panel view for a server route, created on first visit.
    /// `None` for [`ServerView::Editor`] — the editor suite has its own
    /// dedicated fields (`render_editor`) because its parts are accessed
    /// typed elsewhere (terminal toggle, key-tree width persistence).
    fn tool_view(&mut self, view: ServerView, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyView> {
        if let Some(existing) = self.tool_views.get(&view) {
            return Some(existing.clone());
        }
        let state = self.server_state.clone();
        debug!(view = ?view, "creating tool view");
        let created: AnyView = match view {
            ServerView::Editor => return None,
            ServerView::Metrics => cx.new(|cx| ZedisMetrics::new(state, window, cx)).into(),
            ServerView::Slowlog => cx.new(|cx| ZedisSlowlogEditor::new(state, window, cx)).into(),
            ServerView::MemoryAnalysis => cx.new(|cx| ZedisMemoryAnalysis::new(state, window, cx)).into(),
            ServerView::Clients => cx.new(|cx| ZedisClientsManager::new(state, window, cx)).into(),
            ServerView::Monitor => cx.new(|cx| ZedisMonitor::new(state, window, cx)).into(),
            ServerView::Config => cx.new(|cx| ZedisConfigEditor::new(state, window, cx)).into(),
            ServerView::Acl => cx.new(|cx| ZedisAclManager::new(state, window, cx)).into(),
            ServerView::Search => cx.new(|cx| ZedisSearchManager::new(state, window, cx)).into(),
            ServerView::Functions => cx.new(|cx| ZedisFunctionEditor::new(state, window, cx)).into(),
            ServerView::LuaScripts => cx.new(|cx| ZedisLuaScriptLibrary::new(state, window, cx)).into(),
            ServerView::Persistence => cx.new(|cx| ZedisPersistence::new(state, window, cx)).into(),
            ServerView::KeyspaceNotifications => cx.new(|cx| ZedisKeyspaceNotifications::new(state, window, cx)).into(),
            ServerView::Topology => cx.new(|cx| ZedisTopology::new(state, window, cx)).into(),
            ServerView::ServerLoad => cx.new(|cx| ZedisServerLoad::new(state, window, cx)).into(),
            ServerView::ValueSearch => cx.new(|cx| ZedisValueSearch::new(state, window, cx)).into(),
        };
        self.tool_views.insert(view, created.clone());
        Some(created)
    }

    fn render_loading(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w_full().h_full().items_center().justify_center().child(
            div()
                .w(px(LOADING_SKELETON_WIDTH))
                .child(ZedisSkeletonLoading::new().text(i18n_common(cx, "loading"))),
        )
    }

    /// Render the main editor interface with resizable panels
    ///
    /// Layout:
    /// - Left panel: Key tree for browsing Redis keys
    /// - Right panel: Value editor or terminal for the selected key
    ///
    /// The key tree width is user-adjustable and persisted to disk.
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.clone();

        let key_tree = self
            .key_tree
            .get_or_insert_with(|| {
                debug!("Creating new key tree view");
                cx.new(|cx| ZedisKeyTree::new(server_state.clone(), window, cx))
            })
            .clone();

        let mut right_panel = resizable_panel();
        if let Some(content_width) = cx.global::<ZedisGlobalStore>().read(cx).content_width() {
            right_panel = right_panel.size(content_width);
        }
        let (key_tree_width, min_width, max_width) = get_key_tree_widths(self.key_tree_width);

        let right_panel_content = if server_state.read(cx).is_terminal() {
            let terminal = self
                .terminal
                .get_or_insert_with(|| {
                    debug!("Creating new terminal view");
                    cx.new(|cx| ZedisTerminal::new(server_state.clone(), window, cx))
                })
                .clone();
            terminal.into_any_element()
        } else {
            let value_editor = self
                .value_editor
                .get_or_insert_with(|| {
                    debug!("Creating new value editor view");
                    cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx))
                })
                .clone();
            value_editor.into_any_element()
        };

        h_resizable("editor-container")
            .child(
                resizable_panel()
                    .size(key_tree_width)
                    .size_range(min_width..max_width)
                    .child(key_tree),
            )
            .child(right_panel.child(right_panel_content))
            .on_resize(cx.listener(move |this, event: &Entity<ResizableState>, _window, cx| {
                let Some(width) = event.read(cx).sizes().first() else {
                    return;
                };
                this.key_tree_width = *width;
                // Drags fire a stream of resize events — update state per
                // event, write the config once the drag settles. Quiet: the
                // width repaints locally via `this.key_tree_width` already.
                let width = *width;
                update_app_state_and_save_quiet_debounced(cx, "save_key_tree_width", move |state, _| {
                    state.set_key_tree_width(width);
                });
            }))
    }
}

impl Render for ZedisContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = cx.global::<ZedisGlobalStore>().read(cx).route();
        if std::mem::take(&mut self.should_focus) {
            self.focus_handle.focus(window, cx);
        }
        let base = v_flex()
            .id("main-container")
            .track_focus(&self.focus_handle)
            // Scope the Esc→back keybinding to the workspace only. The
            // command palette is a sibling of this view (see main.rs), so
            // it does NOT inherit this context — Esc there is handled by
            // the palette's own capture handler instead of being eaten by
            // NavAction::Back.
            .key_context("Workspace")
            .flex_1()
            .h_full();

        match route {
            Route::Home | Route::Settings => base.child(self.render_servers(window, cx)).into_any_element(),
            Route::Protos => base.child(self.render_proto_editor(window, cx)).into_any_element(),
            Route::Scripts => base.child(self.render_script_editor(window, cx)).into_any_element(),
            _ => {
                let is_busy = self.server_state.read(cx).is_busy();
                // Tool panel for the current route; `None` ⇒ the editor
                // suite. Created only when actually shown (not while the
                // busy skeleton is up).
                let tool = if is_busy {
                    None
                } else {
                    route.server_view().and_then(|view| self.tool_view(view, window, cx))
                };

                base.when(is_busy, |this| this.child(self.render_loading(window, cx)))
                    .when(!is_busy, |this| {
                        this.child(
                            div().flex_1().w_full().relative().child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .overflow_hidden()
                                    .map(|this| match tool {
                                        Some(view) => this.child(div().size_full().child(view)),
                                        None => this.child(self.render_editor(window, cx)),
                                    }),
                            ),
                        )
                    })
                    .on_action(cx.listener(move |this, event: &EditorAction, _window, cx| match event {
                        EditorAction::UpdateTtl
                        | EditorAction::Reload
                        | EditorAction::Create
                        | EditorAction::ReloadKeyTree => {
                            this.server_state.update(cx, move |state, cx| {
                                state.emit_editor_action(*event, cx);
                            });
                        }
                        EditorAction::Cmd => {
                            this.server_state.update(cx, |state, cx| {
                                state.toggle_terminal(cx);
                            });
                        }
                        _ => {
                            cx.propagate();
                        }
                    }))
                    .into_any_element()
            }
        }
    }
}
