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
    helpers::get_key_tree_widths,
    states::{Route, ZedisGlobalStore, ZedisServerState, i18n_common, save_app_state},
    views::{ZedisEditor, ZedisKeyTree, ZedisServers, ZedisSettingEditor, ZedisStatusBar},
};
use gpui::{Entity, Pixels, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme,
    label::Label,
    resizable::{ResizableState, h_resizable, resizable_panel},
    skeleton::Skeleton,
    v_flex,
};
use tracing::{debug, error, info};

// Constants for UI dimensions
const LOADING_SKELETON_WIDTH: f32 = 600.0;
const LOADING_SKELETON_SMALL_WIDTH: f32 = 100.0;
const LOADING_SKELETON_MEDIUM_WIDTH: f32 = 220.0;
const LOADING_SKELETON_LARGE_WIDTH: f32 = 420.0;
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
    settings: Option<Entity<ZedisSettingEditor>>,
    value_editor: Option<Entity<ZedisEditor>>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    status_bar: Entity<ZedisStatusBar>,

    /// Persisted width of the key tree panel (resizable by user)
    key_tree_width: Pixels,

    /// Cached current route to avoid unnecessary updates
    current_route: Route,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisContent {
    /// Create a new content view with route-aware view management
    ///
    /// Sets up subscriptions to automatically clean up cached views when
    /// switching routes to optimize memory usage.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let status_bar = cx.new(|cx| ZedisStatusBar::new(server_state.clone(), window, cx));

        // Subscribe to global state changes for automatic view cleanup
        // This ensures we only keep views in memory that are currently relevant
        subscriptions.push(cx.observe(&cx.global::<ZedisGlobalStore>().state(), |this, model, cx| {
            let route = model.read(cx).route();
            if route == this.current_route {
                return;
            }
            this.current_route = route;

            // Clean up servers view when not on home route
            if route != Route::Home && this.servers.is_some() {
                info!("Cleaning up servers view (route changed)");
                let _ = this.servers.take();
            }

            // Clean up editor views when not on editor route
            if route != Route::Editor {
                info!("Cleaning up key tree and value editor view (route changed)");
                if this.value_editor.is_some() {
                    let _ = this.value_editor.take();
                }
                if this.key_tree.is_some() {
                    let _ = this.key_tree.take();
                }
            }

            cx.notify();
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
            settings: None,
            key_tree: None,
            key_tree_width,
            _subscriptions: subscriptions,
        }
    }
    /// Render the server management view (home page)
    ///
    /// Lazily initializes the servers view on first render and caches it
    /// for subsequent renders until the route changes.
    fn render_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Reuse existing view or create new one
        let servers = self
            .servers
            .get_or_insert_with(|| {
                debug!("Creating new servers view");
                cx.new(|cx| ZedisServers::new(self.server_state.clone(), window, cx))
            })
            .clone();

        div().m(px(SERVERS_MARGIN)).child(servers)
    }
    fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self
            .settings
            .get_or_insert_with(|| {
                debug!("Creating new settings view");
                cx.new(|cx| ZedisSettingEditor::new(window, cx))
            })
            .clone();
        div().child(settings)
    }
    /// Render a loading skeleton screen with animated placeholders
    ///
    /// Displayed when the application is busy (e.g., connecting to Redis server,
    /// loading keys). Provides visual feedback that something is happening.
    fn render_loading(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w_full().h_full().items_center().justify_center().child(
            v_flex()
                .gap_2()
                .w(px(LOADING_SKELETON_WIDTH))
                // Variable-width skeletons create a more natural loading appearance
                .child(Skeleton::new().w(px(LOADING_SKELETON_WIDTH)).h_4().rounded_md())
                .child(Skeleton::new().w(px(LOADING_SKELETON_SMALL_WIDTH)).h_4().rounded_md())
                .child(Skeleton::new().w(px(LOADING_SKELETON_MEDIUM_WIDTH)).h_4().rounded_md())
                .child(Skeleton::new().w(px(LOADING_SKELETON_LARGE_WIDTH)).h_4().rounded_md())
                .child(Skeleton::new().w(px(LOADING_SKELETON_WIDTH)).h_4().rounded_md())
                .child(
                    Label::new(i18n_common(cx, "loading"))
                        .w_full()
                        .text_color(cx.theme().muted_foreground)
                        .mt_2()
                        .text_align(gpui::TextAlign::Center),
                ),
        )
    }
    /// Render the main editor interface with resizable panels
    ///
    /// Layout:
    /// - Left panel: Key tree for browsing Redis keys
    /// - Right panel: Value editor for viewing/editing selected key
    ///
    /// The key tree width is user-adjustable and persisted to disk.
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.clone();

        // Lazily initialize value editor - reuse existing or create new
        let value_editor = self
            .value_editor
            .get_or_insert_with(|| {
                debug!("Creating new value editor view");
                cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx))
            })
            .clone();

        // Lazily initialize key tree - reuse existing or create new
        let key_tree = self
            .key_tree
            .get_or_insert_with(|| {
                debug!("Creating new key tree view");
                cx.new(|cx| ZedisKeyTree::new(server_state.clone(), window, cx))
            })
            .clone();

        let (key_tree_width, min_width, max_width) = get_key_tree_widths(self.key_tree_width);

        h_resizable("editor-container")
            .child(
                // Left panel: Resizable key tree
                resizable_panel()
                    .size(key_tree_width)
                    .size_range(min_width..max_width)
                    .child(key_tree),
            )
            .child(
                // Right panel: Value editor (takes remaining space)
                resizable_panel().child(value_editor),
            )
            .on_resize(cx.listener(move |this, event: &Entity<ResizableState>, _window, cx| {
                // Get the new width from the resize event
                let Some(width) = event.read(cx).sizes().first() else {
                    return;
                };

                // Update local state
                this.key_tree_width = *width;

                // Persist to global state and save to disk
                let mut value = cx.global::<ZedisGlobalStore>().value(cx);
                value.set_key_tree_width(*width);

                // Save asynchronously to avoid blocking UI
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
    /// Main render method - routes to appropriate view based on application state
    ///
    /// Rendering logic:
    /// 1. If on home route -> show server list
    /// 2. If server is busy (connecting/loading) -> show loading skeleton
    /// 3. Otherwise -> show editor interface (key tree + value editor)
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = cx.global::<ZedisGlobalStore>().read(cx).route();
        let base = v_flex().id("main-container").flex_1().h_full();

        // Route 1: Server management view
        match route {
            Route::Home => base.child(self.render_servers(window, cx)).into_any_element(),
            Route::Settings => base.child(self.render_settings(window, cx)).into_any_element(),
            _ => {
                // Route 2: Loading state (show skeleton while connecting/loading)
                let is_busy = self.server_state.read(cx).is_busy();

                // Route 3: Main editor interface
                base.when(is_busy, |this| this.child(self.render_loading(window, cx)))
                    .when(!is_busy, |this| {
                        this.child(
                            div().flex_1().w_full().relative().child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .child(self.render_editor(window, cx)),
                            ),
                        )
                    })
                    .child(self.status_bar.clone())
                    .into_any_element()
            }
        }
    }
}
