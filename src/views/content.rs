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
    states::{GlobalEvent, Route, ServerEvent, ZedisGlobalStore, ZedisServerState, i18n_common, save_app_state},
    views::{
        ZedisAclManager, ZedisClientsManager, ZedisConfigEditor, ZedisEditor, ZedisFunctionEditor, ZedisKeyTree,
        ZedisLuaScriptLibrary, ZedisMemoryAnalysis, ZedisMetrics, ZedisMonitor, ZedisProtoEditor, ZedisScriptEditor,
        ZedisSearchManager, ZedisServers, ZedisSlowlogEditor, ZedisStatusBar, ZedisTerminal,
    },
};
use gpui::{Entity, FocusHandle, Pixels, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    resizable::{ResizableState, h_resizable, resizable_panel},
    v_flex,
};
use tracing::{debug, error, info};
use zedis_ui::ZedisSkeletonLoading;

// Constants for UI dimensions
const LOADING_SKELETON_WIDTH: f32 = 600.0;
const SERVERS_MARGIN: f32 = 8.0;

/// Main content area component for the Zedis application
///
/// Manages the application's main views and routing:
/// - Server list view (Route::Home): Display and manage Redis server connections
/// - Editor view (Route::Editor): Display key tree and value editor for selected server
///
/// Views are lazily initialized and cached for performance, but cleared when
/// no longer needed to conserve memory.
pub struct ZedisContent {
    /// Reference to the server state containing Redis connection and data
    server_state: Entity<ZedisServerState>,

    /// Cached views - lazily initialized and cleared when switching routes
    servers: Option<Entity<ZedisServers>>,
    proto_editor: Option<Entity<ZedisProtoEditor>>,
    script_editor: Option<Entity<ZedisScriptEditor>>,
    value_editor: Option<Entity<ZedisEditor>>,
    terminal: Option<Entity<ZedisTerminal>>,
    metrics: Option<Entity<ZedisMetrics>>,
    slowlog_editor: Option<Entity<ZedisSlowlogEditor>>,
    memory_analysis: Option<Entity<ZedisMemoryAnalysis>>,
    clients_manager: Option<Entity<ZedisClientsManager>>,
    monitor: Option<Entity<ZedisMonitor>>,
    config_editor: Option<Entity<ZedisConfigEditor>>,
    acl_manager: Option<Entity<ZedisAclManager>>,
    search_manager: Option<Entity<ZedisSearchManager>>,
    function_editor: Option<Entity<ZedisFunctionEditor>>,
    lua_script_library: Option<Entity<ZedisLuaScriptLibrary>>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    status_bar: Entity<ZedisStatusBar>,

    /// Persisted width of the key tree panel (resizable by user)
    key_tree_width: Pixels,

    /// Cached current route to avoid unnecessary updates
    current_route: Route,
    should_focus: bool,
    focus_handle: FocusHandle,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisContent {
    fn clear_views(&mut self) {
        let route = self.current_route;
        if route != Route::Home {
            self.servers.take();
        }
        if route != Route::Editor && route != Route::Metrics {
            self.key_tree.take();
            self.value_editor.take();
            self.terminal.take();
        }
        if route != Route::Metrics {
            self.metrics.take();
        }
        if route != Route::Protos {
            self.proto_editor.take();
        }
        if route != Route::Scripts {
            self.script_editor.take();
        }
        if route != Route::Slowlog {
            self.slowlog_editor.take();
        }
        if route != Route::MemoryAnalysis {
            self.memory_analysis.take();
        }
        if route != Route::Clients {
            self.clients_manager.take();
        }
        if route != Route::Monitor {
            self.monitor.take();
        }
        if route != Route::Config {
            self.config_editor.take();
        }
        if route != Route::Acl {
            self.acl_manager.take();
        }
        if route != Route::Search {
            self.search_manager.take();
        }
        if route != Route::Functions {
            self.function_editor.take();
        }
        if route != Route::LuaScripts {
            self.lua_script_library.take();
        }
    }
    /// Create a new content view with route-aware view management
    ///
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
                    this.current_route = *route;
                    this.clear_views();
                    cx.notify();
                }
                GlobalEvent::ServerSelected(server_id, db) => {
                    if server_id.is_empty() {
                        return;
                    }
                    this.server_state.update(cx, |state, cx| {
                        state.select(server_id.clone(), *db, cx);
                    });
                }
                _ => {}
            }),
        );

        subscriptions.push(cx.subscribe(&server_state, |this, _server_state, event, cx| {
            if let ServerEvent::TerminalToggled(_) = event {
                this.should_focus = true;
                cx.notify();
            }
        }));

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
            metrics: None,
            slowlog_editor: None,
            memory_analysis: None,
            clients_manager: None,
            monitor: None,
            config_editor: None,
            acl_manager: None,
            search_manager: None,
            function_editor: None,
            lua_script_library: None,
            key_tree: None,
            key_tree_width,
            should_focus: false,
            focus_handle,
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
        div().m(px(SERVERS_MARGIN)).child(servers)
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

    fn render_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self
            .metrics
            .get_or_insert_with(|| {
                debug!("Creating new metrics view");
                cx.new(|cx| ZedisMetrics::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(metrics)
    }

    fn render_slowlog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let slowlog = self
            .slowlog_editor
            .get_or_insert_with(|| {
                debug!("Creating new slowlog editor view");
                cx.new(|cx| ZedisSlowlogEditor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(slowlog)
    }

    fn render_memory_analysis(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let memory_analysis = self
            .memory_analysis
            .get_or_insert_with(|| {
                debug!("Creating new memory analysis view");
                cx.new(|cx| ZedisMemoryAnalysis::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(memory_analysis)
    }

    fn render_clients(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clients = self
            .clients_manager
            .get_or_insert_with(|| {
                debug!("Creating new clients manager view");
                cx.new(|cx| ZedisClientsManager::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(clients)
    }

    fn render_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self
            .monitor
            .get_or_insert_with(|| {
                debug!("Creating new monitor view");
                cx.new(|cx| ZedisMonitor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(monitor)
    }

    fn render_config_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let config_editor = self
            .config_editor
            .get_or_insert_with(|| {
                debug!("Creating new config editor view");
                cx.new(|cx| ZedisConfigEditor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(config_editor)
    }

    fn render_acl_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let acl_manager = self
            .acl_manager
            .get_or_insert_with(|| {
                debug!("Creating new ACL manager view");
                cx.new(|cx| ZedisAclManager::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(acl_manager)
    }

    fn render_search_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_manager = self
            .search_manager
            .get_or_insert_with(|| {
                debug!("Creating new search manager view");
                cx.new(|cx| ZedisSearchManager::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(search_manager)
    }

    fn render_function_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let function_editor = self
            .function_editor
            .get_or_insert_with(|| {
                debug!("Creating new function editor view");
                cx.new(|cx| ZedisFunctionEditor::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(function_editor)
    }

    fn render_lua_script_library(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lib = self
            .lua_script_library
            .get_or_insert_with(|| {
                debug!("Creating new lua script library view");
                cx.new(|cx| ZedisLuaScriptLibrary::new(self.server_state.clone(), window, cx))
            })
            .clone();
        div().size_full().child(lib)
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
                let mut value = cx.global::<ZedisGlobalStore>().value(cx);
                value.set_key_tree_width(*width);
                cx.background_spawn(async move {
                    if let Err(e) = save_app_state(&value) {
                        error!(error = %e, "Failed to save key tree width");
                    } else {
                        info!("Key tree width saved successfully");
                    }
                })
                .detach();
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
            .flex_1()
            .h_full();

        match route {
            Route::Home | Route::Settings => base.child(self.render_servers(window, cx)).into_any_element(),
            Route::Protos => base.child(self.render_proto_editor(window, cx)).into_any_element(),
            Route::Scripts => base.child(self.render_script_editor(window, cx)).into_any_element(),
            _ => {
                let is_busy = self.server_state.read(cx).is_busy();
                let is_metrics = route == Route::Metrics;
                let is_slowlog = route == Route::Slowlog;
                let is_memory_analysis = route == Route::MemoryAnalysis;
                let is_clients = route == Route::Clients;
                let is_monitor = route == Route::Monitor;
                let is_config = route == Route::Config;
                let is_acl = route == Route::Acl;
                let is_search = route == Route::Search;
                let is_functions = route == Route::Functions;
                let is_lua_scripts = route == Route::LuaScripts;

                base.when(is_busy, |this| this.child(self.render_loading(window, cx)))
                    .when(!is_busy, |this| {
                        this.child(
                            div().flex_1().w_full().relative().child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .overflow_hidden()
                                    .when(is_metrics, |this| this.child(self.render_metrics(window, cx)))
                                    .when(is_slowlog, |this| this.child(self.render_slowlog(window, cx)))
                                    .when(is_memory_analysis, |this| {
                                        this.child(self.render_memory_analysis(window, cx))
                                    })
                                    .when(is_clients, |this| this.child(self.render_clients(window, cx)))
                                    .when(is_monitor, |this| this.child(self.render_monitor(window, cx)))
                                    .when(is_config, |this| this.child(self.render_config_editor(window, cx)))
                                    .when(is_acl, |this| this.child(self.render_acl_manager(window, cx)))
                                    .when(is_search, |this| this.child(self.render_search_manager(window, cx)))
                                    .when(is_functions, |this| this.child(self.render_function_editor(window, cx)))
                                    .when(is_lua_scripts, |this| {
                                        this.child(self.render_lua_script_library(window, cx))
                                    })
                                    .when(
                                        !is_metrics
                                            && !is_slowlog
                                            && !is_memory_analysis
                                            && !is_clients
                                            && !is_monitor
                                            && !is_config
                                            && !is_acl
                                            && !is_search
                                            && !is_functions
                                            && !is_lua_scripts,
                                        |this| this.child(self.render_editor(window, cx)),
                                    ),
                            ),
                        )
                    })
                    .child(self.status_bar.clone())
                    .on_action(cx.listener(move |this, event: &EditorAction, _window, cx| match event {
                        EditorAction::UpdateTtl | EditorAction::Reload | EditorAction::Create => {
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
