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
    components::SkeletonLoading,
    connection::{get_command_description, get_connection_manager, list_commands},
    error::Error,
    helpers::{
        EditorAction, get_font_family, get_key_tree_widths, redis_value_to_string, starts_with_ignore_ascii_case,
    },
    states::{Route, ServerEvent, ZedisGlobalStore, ZedisServerState, save_app_state},
    views::{ZedisEditor, ZedisKeyTree, ZedisProtoEditor, ZedisServers, ZedisSettingEditor, ZedisStatusBar},
};
use gpui::{Entity, FocusHandle, Pixels, ScrollHandle, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme,
    input::{Input, InputEvent, InputState},
    label::Label,
    resizable::{ResizableState, h_resizable, resizable_panel},
    v_flex,
};
use redis::cmd;
use tracing::{debug, error, info};
type Result<T, E = Error> = std::result::Result<T, E>;

// Constants for UI dimensions
const LOADING_SKELETON_WIDTH: f32 = 600.0;
const SERVERS_MARGIN: f32 = 8.0;
const CMD_LABEL: &str = "$";
const CMD_CLEAR: &str = "clear";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const ZEDIS_LOGO: &str = r#" __________ ____ ___ ____  
|__  / ____|  _ \_ _/ ___| 
  / /|  _| | | | | |\___ \    ZEDIS Native Redis GUI v{VERSION}
 / /_| |___| |_| | | ___) |
/____|_____|____/___|____/ 
"#;

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
    setting_editor: Option<Entity<ZedisSettingEditor>>,
    proto_editor: Option<Entity<ZedisProtoEditor>>,
    value_editor: Option<Entity<ZedisEditor>>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    status_bar: Entity<ZedisStatusBar>,
    cmd_output_scroll_handle: ScrollHandle,
    cmd_input_state: Entity<InputState>,
    cmd_outputs: Vec<SharedString>,
    redis_commands: Vec<SharedString>,
    cmd_suggestions: Vec<String>,
    cmd_suggestion_index: Option<usize>,

    /// Persisted width of the key tree panel (resizable by user)
    key_tree_width: Pixels,

    /// Cached current route to avoid unnecessary updates
    current_route: Route,
    should_focus: Option<bool>,
    should_focus_cmd_input: Option<bool>,
    focus_handle: FocusHandle,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisContent {
    fn clear_views(&mut self) {
        let route = self.current_route;
        if route != Route::Editor {
            self.key_tree.take();
            self.value_editor.take();
        }
        if route != Route::Settings {
            self.setting_editor.take();
        }
        if route != Route::Protos {
            self.proto_editor.take();
        }
    }
    /// Create a new content view with route-aware view management
    ///
    /// Sets up subscriptions to automatically clean up cached views when
    /// switching routes to optimize memory usage.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let status_bar = cx.new(|cx| ZedisStatusBar::new(server_state.clone(), window, cx));

        // Subscribe to global state changes for automatic view cleanup
        // This ensures we only keep views in memory that are currently relevant
        subscriptions.push(cx.observe(&cx.global::<ZedisGlobalStore>().state(), |this, model, cx| {
            let route = model.read(cx).route();
            if route == this.current_route {
                return;
            }
            this.current_route = route;

            this.clear_views();

            cx.notify();
        }));

        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| match event {
                ServerEvent::TerminalToggled(terminal) => {
                    this.should_focus = Some(true);
                    if *terminal {
                        this.should_focus_cmd_input = Some(true);
                    } else {
                        this.should_focus_cmd_input = None;
                    }
                    cx.notify();
                }
                ServerEvent::ServerInfoUpdated(_) => {
                    this.update_redis_commands(cx);
                }
                ServerEvent::ServerSelected(_, _) => {
                    this.reset_cmd_state(cx);
                }
                _ => {}
            }),
        );

        // Restore persisted key tree width from global state
        let global_store = cx.global::<ZedisGlobalStore>().read(cx);
        let key_tree_width = global_store.key_tree_width();
        let route = global_store.route();
        let cmd_input_state = cx.new(|cx| InputState::new(window, cx));
        subscriptions.push(
            cx.subscribe_in(&cmd_input_state, window, |this, state, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let cmd = state.read(cx).value();
                    let mut selected_cmd = "".to_string();
                    if let Some(index) = this.cmd_suggestion_index
                        && let Some(suggestion) = this.cmd_suggestions.get(index)
                        && !starts_with_ignore_ascii_case(cmd.as_str(), suggestion)
                    {
                        selected_cmd = suggestion.clone();
                    }

                    if !selected_cmd.is_empty() {
                        this.apply_suggestion(window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    state.update(cx, |state, cx| {
                        state.set_value(SharedString::default(), window, cx);
                    });
                    this.cmd_suggestions.clear();
                    this.cmd_suggestion_index = None;
                    this.execute_command(cmd, cx);
                }
                InputEvent::Change => {
                    let value = state.read(cx).value().to_string();
                    if !value.is_empty()
                        && !value.contains(' ')
                        && let Some(last) = value.chars().last()
                        && let Some(index) = last.to_digit(10)
                        && index <= this.cmd_suggestions.len() as u32
                    {
                        this.cmd_suggestion_index = Some((index - 1) as usize);
                        this.apply_suggestion(window, cx);
                        return;
                    }

                    this.update_suggestions(value);
                    cx.notify();
                }
                _ => {}
            }),
        );
        info!("Creating new content view");

        Self {
            server_state,
            status_bar,
            current_route: route,
            servers: None,
            value_editor: None,
            setting_editor: None,
            key_tree: None,
            cmd_outputs: Vec::with_capacity(5),
            redis_commands: Vec::new(),
            key_tree_width,
            cmd_input_state,
            cmd_suggestions: Vec::new(),
            cmd_suggestion_index: None,
            should_focus: None,
            should_focus_cmd_input: None,
            cmd_output_scroll_handle: ScrollHandle::new(),
            focus_handle,
            proto_editor: None,
            _subscriptions: subscriptions,
        }
    }
    fn reset_cmd_state(&mut self, _cx: &mut Context<Self>) {
        self.cmd_outputs.clear();
        self.cmd_outputs.extend(
            ZEDIS_LOGO
                .replace("{VERSION}", VERSION)
                .lines()
                .map(|line| line.to_string().into()),
        );
        self.cmd_output_scroll_handle = ScrollHandle::new();
    }
    fn update_redis_commands(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let version = server_state.version();
        let commands = list_commands(version);
        self.redis_commands = commands;
    }

    /// Update command suggestions based on the current input
    fn update_suggestions(&mut self, input: String) {
        self.cmd_suggestions.clear();
        self.cmd_suggestion_index = None;

        if input.is_empty() {
            return;
        }

        // Get the words from input
        let words: Vec<&str> = input.split_whitespace().collect();
        if words.is_empty() {
            return;
        }

        // Try to match with progressively more words to support multi-word commands
        // like "ACL GETUSER", "CLUSTER INFO", etc.
        // We try from the longest possible command (up to 3 words) down to 1 word
        let max_words = words.len().min(3); // Redis commands typically have at most 3 words

        for word_count in (1..=max_words).rev() {
            let cmd_input = words[..word_count].join(" ").to_uppercase();

            // Find commands that start with this input
            let matches: Vec<String> = self
                .redis_commands
                .iter()
                .filter(|cmd| cmd.as_str().starts_with(&cmd_input))
                .take(5)
                .map(|cmd| cmd.to_string())
                .collect();

            // If we found matches, use them
            if !matches.is_empty() {
                self.cmd_suggestions = matches;
                self.cmd_suggestion_index = self.cmd_suggestions.iter().position(|cmd| cmd == &cmd_input);
                return;
            }
        }
    }

    /// Apply the currently selected suggestion or the first one
    fn apply_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_suggestions.is_empty() {
            return;
        }

        let suggestion = if let Some(index) = self.cmd_suggestion_index {
            self.cmd_suggestions.get(index).cloned()
        } else {
            self.cmd_suggestions.first().cloned()
        };

        if let Some(cmd) = suggestion {
            self.cmd_input_state.update(cx, |state, cx| {
                state.set_value(SharedString::from(cmd), window, cx);
            });
            self.cmd_suggestions.clear();
            self.cmd_suggestion_index = None;
            cx.notify();
        }
    }
    fn execute_command(&mut self, command: SharedString, cx: &mut Context<Self>) {
        if command.is_empty() {
            return;
        }
        if command == CMD_CLEAR {
            self.reset_cmd_state(cx);
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        cx.spawn(async move |handle, cx| {
            let command_clone = command.clone();
            let task = cx.background_spawn(async move {
                let parts: Vec<_> = command.split_whitespace().map(|s| s.to_string()).collect();
                if parts.is_empty() {
                    return Ok(SharedString::default());
                }
                let cmd_name = parts[0].clone();
                let args = parts[1..].to_vec();
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let data: redis::Value = cmd(&cmd_name).arg(&args).query_async(&mut conn).await?;
                Ok(redis_value_to_string(&data).into())
            });
            let result: Result<SharedString> = task.await;
            let content: SharedString = match result {
                Ok(result) => result,
                Err(e) => e.to_string().into(),
            };

            handle.update(cx, |this, cx| {
                this.cmd_outputs.extend(vec![
                    format!("{CMD_LABEL} {command_clone}").into(),
                    content,
                    SharedString::default(),
                ]);
                let scroll_handle = this.cmd_output_scroll_handle.clone();
                cx.notify();
                cx.defer(move |_cx| {
                    scroll_handle.scroll_to_bottom();
                });
            })
        })
        .detach();
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
            .setting_editor
            .get_or_insert_with(|| {
                debug!("Creating new settings view");
                cx.new(|cx| ZedisSettingEditor::new(window, cx))
            })
            .clone();
        div().child(settings)
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
    /// Render a loading skeleton screen with animated placeholders
    ///
    /// Displayed when the application is busy (e.g., connecting to Redis server,
    /// loading keys). Provides visual feedback that something is happening.
    fn render_loading(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .child(div().w(px(LOADING_SKELETON_WIDTH)).child(SkeletonLoading::new()))
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

        // Lazily initialize key tree - reuse existing or create new
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
            if let Some(true) = self.should_focus_cmd_input.take() {
                self.cmd_input_state.update(cx, |this, cx| this.focus(window, cx));
            }
            let font_family: SharedString = get_font_family().into();
            let handle_suggestion_key_down = cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                if this.cmd_suggestions.is_empty() {
                    return;
                }
                let keystroke = &event.keystroke;
                let max = this.cmd_suggestions.len() - 1;
                let new_index = match keystroke.key.as_str() {
                    "down" => {
                        if let Some(current) = this.cmd_suggestion_index {
                            Some((current + 1).min(max))
                        } else {
                            Some(0)
                        }
                    }
                    "up" => {
                        if let Some(current) = this.cmd_suggestion_index {
                            if current > 0 { Some(current - 1) } else { Some(max) }
                        } else {
                            Some(max)
                        }
                    }
                    _ => None,
                };
                if let Some(new_index) = new_index {
                    this.cmd_suggestion_index = Some(new_index);
                    cx.notify();
                    cx.stop_propagation();
                }
            });

            v_flex()
                .w_full()
                .h_full()
                .child(
                    div()
                        .id("cmd-output-scrollable-container")
                        .track_scroll(&self.cmd_output_scroll_handle)
                        .flex_1()
                        .w_full()
                        .overflow_y_scroll()
                        .child(
                            v_flex().p_2().gap_1().children(
                                self.cmd_outputs
                                    .iter()
                                    .map(|line| div().child(Label::new(line.clone()).font_family(font_family.clone()))),
                            ),
                        ),
                )
                .child(
                    v_flex()
                        .w_full()
                        .when(!self.cmd_suggestions.is_empty(), |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .bg(cx.theme().background)
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .p_1()
                                    .child(v_flex().gap_0p5().children(self.cmd_suggestions.iter().enumerate().map(
                                        |(idx, cmd)| {
                                            let is_selected = self.cmd_suggestion_index == Some(idx);
                                            let text = format!("{}: {cmd}", idx + 1);

                                            let (summary, syntax) = get_command_description(cmd).unwrap_or_default();
                                            let make_label = |text: SharedString| {
                                                Label::new(text)
                                                    .font_family(font_family.clone())
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                            };
                                            div()
                                                .px_2()
                                                .py_1()
                                                .rounded_sm()
                                                .when(is_selected, |this| this.bg(cx.theme().selection))
                                                .child(
                                                    Label::new(text)
                                                        .font_family(font_family.clone())
                                                        .text_color(cx.theme().foreground),
                                                )
                                                .child(make_label(syntax))
                                                .child(make_label(summary))
                                        },
                                    ))),
                            )
                        })
                        .child(
                            div()
                                .w_full()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .on_key_down(handle_suggestion_key_down)
                                .child(
                                    Input::new(&self.cmd_input_state)
                                        .font_family(font_family)
                                        .prefix(Label::new(CMD_LABEL).text_color(cx.theme().yellow))
                                        .appearance(false),
                                ),
                        ),
                )
                .into_any_element()
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
                // Left panel: Resizable key tree
                resizable_panel()
                    .size(key_tree_width)
                    .size_range(min_width..max_width)
                    .child(key_tree),
            )
            .child(right_panel.child(right_panel_content))
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
        if let Some(true) = self.should_focus.take() {
            self.focus_handle.focus(window);
        }
        let base = v_flex()
            .id("main-container")
            .track_focus(&self.focus_handle)
            .flex_1()
            .h_full();

        // Route 1: Server management view
        match route {
            Route::Home => base.child(self.render_servers(window, cx)).into_any_element(),
            Route::Settings => base.child(self.render_settings(window, cx)).into_any_element(),
            Route::Protos => base.child(self.render_proto_editor(window, cx)).into_any_element(),
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
