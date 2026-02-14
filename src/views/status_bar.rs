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
    connection::{RedisClientDescription, get_server},
    helpers::humanize_keystroke,
    states::{
        ErrorMessage, GlobalEvent, ServerEvent, ServerTask, ViewMode, ZedisGlobalStore, ZedisServerState, i18n_common,
        i18n_sidebar, i18n_status_bar,
    },
};
use gpui::{Entity, Hsla, SharedString, Subscription, Task, TextAlign, Window, div, prelude::*};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    tooltip::Tooltip,
};
use std::{sync::Arc, time::Duration};
use tracing::{debug, info};

/// Formats the database size and scan count string "count/total".
#[inline]
fn format_size(dbsize: Option<u64>, scan_count: usize) -> SharedString {
    if let Some(dbsize) = dbsize {
        format!("{scan_count}/{dbsize}")
    } else {
        "--".to_string()
    }
    .into()
}
/// Formats the latency string and determines the color based on the delay.
#[inline]
fn format_latency(latency: Option<Duration>, cx: &Context<ZedisStatusBar>) -> (SharedString, Hsla) {
    if let Some(latency) = latency {
        let ms = latency.as_millis();
        let theme = cx.theme();
        // Determine color based on latency thresholds
        let color = if ms < 50 {
            theme.green
        } else if ms < 500 {
            theme.yellow
        } else {
            theme.red
        };
        // Format string
        if ms < 1000 {
            (format!("{ms}ms").into(), color)
        } else {
            (format!("{:.2}s", ms as f64 / 1000.0).into(), color)
        }
    } else {
        ("--".to_string().into(), cx.theme().primary)
    }
}

/// Formats the node count and version information.
#[inline]
fn format_nodes(nodes: (usize, usize), version: &str) -> SharedString {
    format!("{} / {} (v{})", nodes.0, nodes.1, version).into()
}

#[inline]
fn format_nodes_description(description: Arc<RedisClientDescription>, cx: &Context<ZedisStatusBar>) -> SharedString {
    let t = i18n_sidebar(cx, "server_type");
    let master_nodes = i18n_sidebar(cx, "master_nodes");
    let slave_nodes = i18n_sidebar(cx, "slave_nodes");
    let mut messages = Vec::with_capacity(3);
    messages.push(format!("{t}: {}", description.server_type.as_str()));
    messages.push(format!("{master_nodes}: {}", description.master_nodes));
    if !description.slave_nodes.is_empty() {
        messages.push(format!("{slave_nodes}: {}", description.slave_nodes));
    }
    messages.join("\n").into()
}

// --- Local State ---

#[derive(Default)]
struct StatusBarServerState {
    supports_db_selection: bool,
    server_id: SharedString,
    size: SharedString,
    latency: (SharedString, Hsla),
    used_memory: SharedString,
    clients: SharedString,
    nodes: SharedString,
    scan_finished: bool,
    soft_wrap: bool,
    nodes_description: SharedString,
}

#[derive(Debug, Clone)]
struct DbInfo {
    label: SharedString,
    db: usize,
}

impl SelectItem for DbInfo {
    type Value = usize;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.db
    }
}

/// Local state for the status bar to cache formatted strings and colors.
/// This prevents re-calculating strings on every render frame.
#[derive(Default)]
struct StatusBarState {
    server_state: StatusBarServerState,
    data_format: Option<SharedString>,
    error: Option<ErrorMessage>,
}

pub struct ZedisStatusBar {
    state: StatusBarState,

    viewer_mode_state: Entity<SelectState<SearchableVec<SharedString>>>,
    db_state: Entity<SelectState<Vec<DbInfo>>>,
    should_reset_viewer_mode: Option<bool>,
    should_reset_db: Option<bool>,
    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    readonly: bool,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize state from the current server state
        // Read only necessary fields to avoid cloning the entire state if it's large

        let mut subscriptions = vec![];
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            match event {
                ServerEvent::ServerSelected(server_id) => {
                    this.reset(server_id.clone());
                }
                ServerEvent::ServerRedisInfoUpdated => {
                    this.fill_state(server_state, cx);
                }
                ServerEvent::ServerInfoUpdated => {
                    this.readonly = server_state.read(cx).readonly();
                    server_state.update(cx, |state, cx| {
                        state.refresh_redis_info(cx);
                    });
                }
                ServerEvent::KeyScanStarted => {
                    this.state.server_state.scan_finished = false;
                }
                ServerEvent::KeyScanFinished => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
                    this.state.server_state.scan_finished = true;
                }
                ServerEvent::KeyScanPaged => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
                }
                ServerEvent::ErrorOccurred(error) => {
                    debug!(
                        message = error.message.as_str(),
                        category = error.category.as_str(),
                        "error occurred"
                    );
                    this.state.error = Some(error.clone());
                }
                ServerEvent::TaskStarted(task) => {
                    // Clear error when a new task starts (except background ping)
                    if *task != ServerTask::RefreshRedisInfo {
                        this.state.error = None;
                    }
                }
                ServerEvent::ValueLoaded => {
                    let state = server_state.read(cx);
                    this.should_reset_viewer_mode = Some(true);
                    if let Some(value) = state.value().and_then(|item| item.bytes_value()) {
                        let mut format = value.format.as_str().to_string();
                        if let Some(mime) = &value.mime {
                            format = format!("{}({})", format, mime);
                        }
                        this.state.data_format = Some(format.into());
                    } else {
                        this.state.data_format = None;
                    }
                }
                _ => {
                    return;
                }
            }
            cx.notify();
        }));
        let viewer_mode_state = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![
                    ViewMode::Auto.as_str().into(),
                    ViewMode::Plain.as_str().into(),
                    ViewMode::Hex.as_str().into(),
                ]),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });

        subscriptions.push(cx.subscribe_in(
            &viewer_mode_state,
            window,
            |view, _state, event: &SelectEvent<SearchableVec<SharedString>>, _window, cx| match event {
                SelectEvent::Confirm(value) => {
                    if let Some(selected_value) = value {
                        view.server_state.update(cx, |state, cx| {
                            state.update_bytes_value_view_mode(selected_value.clone(), cx);
                        });
                    }
                }
            },
        ));

        let db_items = (0..16)
            .map(|db| DbInfo {
                label: format!("DB: {}", db).into(),
                db,
            })
            .collect::<Vec<_>>();
        let db_state = cx.new(|cx| SelectState::new(db_items, Some(IndexPath::new(0)), window, cx));
        subscriptions.push(cx.subscribe_in(
            &db_state,
            window,
            |view, _state, event: &SelectEvent<Vec<DbInfo>>, _window, cx| match event {
                SelectEvent::Confirm(value) => {
                    let Some(db) = *value else {
                        return;
                    };
                    let server_id = view.server_state.read(cx).server_id().to_string();
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.set_selected_server((server_id, db), cx);
                        });
                    });
                }
            },
        ));
        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global_state, event, _cx| {
            if let GlobalEvent::ServerSelected(_, _) = event {
                this.should_reset_db = Some(true);
            }
        }));
        let readonly = server_state.read(cx).readonly();

        let mut this = Self {
            heartbeat_task: None,
            viewer_mode_state,
            db_state,
            should_reset_db: None,
            server_state: server_state.clone(),
            _subscriptions: subscriptions,
            should_reset_viewer_mode: None,
            state: StatusBarState { ..Default::default() },
            readonly,
        };
        this.fill_state(server_state.clone(), cx);
        this.start_heartbeat(server_state, cx);

        info!("Creating new status bar view");
        this
    }
    fn reset(&mut self, server_id: SharedString) {
        if self.state.server_state.server_id != server_id {
            self.state.server_state = StatusBarServerState::default();
            self.state.server_state.server_id = server_id;
        } else {
            self.state.server_state.size = SharedString::default();
        }
        self.state.data_format = None;
        self.state.error = None;
    }
    fn fill_state(&mut self, server_state: Entity<ZedisServerState>, cx: &Context<Self>) {
        let state = server_state.read(cx);
        let Some(redis_info) = state.redis_info() else {
            return;
        };
        let clients = if redis_info.connected_clients == 0 {
            "--".to_string()
        } else {
            format!("{} / {}", redis_info.blocked_clients, redis_info.connected_clients)
        };
        let used_memory = if redis_info.used_memory == 0 {
            "--".to_string()
        } else {
            redis_info.used_memory_human.clone()
        };
        self.state.server_state = StatusBarServerState {
            supports_db_selection: state.supports_db_selection(),
            server_id: state.server_id().to_string().into(),
            size: format_size(state.dbsize(), state.scan_count()),
            latency: format_latency(Some(redis_info.latency), cx),
            used_memory: used_memory.into(),
            clients: clients.into(),
            nodes: format_nodes(state.nodes(), state.version()),
            scan_finished: state.scan_completed(),
            soft_wrap: state.soft_wrap(),
            nodes_description: format_nodes_description(state.nodes_description().clone(), cx),
        };
    }
    /// Start the heartbeat task
    fn start_heartbeat(&mut self, server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) {
        // start task
        self.heartbeat_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let _ = server_state.update(cx, |state, cx| {
                    state.refresh_redis_info(cx);
                });
            }
        }));
    }
    /// Render a vertical divider line
    fn render_divider(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().h_4().w_px().flex_none().bg(cx.theme().muted_foreground).mx_4()
    }
    /// Render the server status
    fn render_server_status(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let is_completed = server_state.scan_finished;
        let nodes_description = server_state.nodes_description.clone();
        let terminal_tooltip = format!(
            "{} ({})",
            i18n_status_bar(cx, "toggle_terminal_tooltip"),
            humanize_keystroke("cmd-j")
        );
        let readonly_tooltip = i18n_status_bar(cx, "toggle_readonly_tooltip");
        h_flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("zedis-status-bar-server-terminal")
                    .outline()
                    .small()
                    .tooltip(terminal_tooltip)
                    .icon(IconName::SquareTerminal)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            state.toggle_terminal(cx);
                        });
                    })),
            )
            .when(server_state.supports_db_selection, |this| {
                this.child(Select::new(&self.db_state).mt_1().small())
            })
            .child(
                Button::new("zedis-status-bar-server-toggle-readonly")
                    .outline()
                    .small()
                    .tooltip(readonly_tooltip)
                    .when(self.readonly, |this| this.icon(Icon::new(CustomIconName::Lock)))
                    .when(!self.readonly, |this| this.icon(Icon::new(CustomIconName::LockOpen)))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            state.toggle_readonly(cx);
                        });
                    })),
            )
            .child(self.render_divider(window, cx))
            .child(
                Button::new("zedis-status-bar-scan-more")
                    .outline()
                    .small()
                    .disabled(is_completed)
                    .tooltip(if is_completed {
                        i18n_status_bar(cx, "scan_completed")
                    } else {
                        i18n_status_bar(cx, "scan_more_keys")
                    })
                    .mr_1()
                    .icon(CustomIconName::ChevronsDown)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            state.scan_next(cx);
                        });
                    })),
            )
            .child(Label::new(server_state.size.clone()).mr_2())
            .child(
                div()
                    .child(
                        h_flex()
                            .child(Icon::new(CustomIconName::Network).text_color(cx.theme().primary).mr_1())
                            .child(Label::new(server_state.nodes.clone())),
                    )
                    .id("zedis-servers")
                    .tooltip(move |window, cx| Tooltip::new(nodes_description.clone()).build(window, cx)),
            )
            .child(self.render_divider(window, cx))
            .child(
                Button::new("zedis-status-bar-letency")
                    .ghost()
                    .px_1()
                    .disabled(true)
                    .tooltip(i18n_common(cx, "latency"))
                    .icon(Icon::new(CustomIconName::ChevronsLeftRightEllipsis).text_color(cx.theme().primary))
                    .text_color(server_state.latency.1)
                    .label(server_state.latency.0.clone()),
            )
            .child(
                Button::new("zedis-status-bar-used-memory")
                    .ghost()
                    .px_1()
                    .disabled(true)
                    .tooltip(i18n_common(cx, "used_memory"))
                    .icon(Icon::new(CustomIconName::MemoryStick))
                    .text_color(cx.theme().primary)
                    .label(server_state.used_memory.clone()),
            )
            .child(
                Button::new("zedis-status-bar-clients")
                    .ghost()
                    .px_1()
                    .disabled(true)
                    .text_color(cx.theme().primary)
                    .tooltip(i18n_common(cx, "clients"))
                    .icon(Icon::new(CustomIconName::AudioWaveform))
                    .label(server_state.clients.clone()),
            )
    }
    fn render_editor_settings(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(server_state.soft_wrap, |this| this.icon(IconName::Check))
            .tooltip(i18n_status_bar(cx, "soft_wrap_tooltip"))
            .label(i18n_status_bar(cx, "soft_wrap"))
            .on_click(cx.listener(|this, _, _window, cx| {
                let soft_wrap = !this.state.server_state.soft_wrap;
                this.state.server_state.soft_wrap = soft_wrap;
                if let Ok(mut server) = get_server(this.state.server_state.server_id.as_str()) {
                    server.soft_wrap = Some(soft_wrap);
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.upsert_server(server, cx);
                        });
                    });
                }
                this.server_state.update(cx, |state, cx| {
                    state.set_soft_wrap(soft_wrap, cx);
                });

                cx.notify();
            }))
    }
    fn render_data_format(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data_format) = self.state.data_format.clone() else {
            return h_flex().into_any_element();
        };
        Button::new("data-format")
            .ghost()
            .disabled(true)
            .text_color(cx.theme().primary)
            .tooltip(i18n_status_bar(cx, "data_format_tooltip"))
            .icon(Icon::new(CustomIconName::Binary))
            .label(data_format)
            .into_any_element()
    }
    fn render_viewer_mode(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.data_format.is_none() {
            return h_flex();
        };
        let label = i18n_status_bar(cx, "viewer");
        h_flex()
            .child(Label::new(label).mr_1())
            .child(Select::new(&self.viewer_mode_state).appearance(false))
    }

    /// Render the error message
    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = &self.state.error else {
            return h_flex().flex_1();
        };
        // error message is always on the right
        h_flex().flex_1().child(
            Label::new(data.message.clone())
                .mr_2()
                .w_full()
                .text_xs()
                .text_color(cx.theme().red)
                .text_align(TextAlign::Right),
        )
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!("render status bar view");
        if self.state.server_state.server_id.is_empty() {
            return h_flex();
        }
        if let Some(true) = self.should_reset_viewer_mode.take() {
            self.viewer_mode_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(0)), window, cx);
            });
        }
        if let Some(true) = self.should_reset_db.take() {
            let db = cx
                .global::<ZedisGlobalStore>()
                .read(cx)
                .selected_server()
                .map(|(_, db)| *db)
                .unwrap_or_default();
            self.db_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(db)), window, cx);
            });
        }
        h_flex()
            .justify_between()
            .text_sm()
            .py_1p5()
            .px_4()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .child(self.render_server_status(window, cx))
            .child(self.render_divider(window, cx))
            .child(self.render_editor_settings(window, cx))
            .when(self.state.data_format.is_some(), |this| {
                this.child(self.render_divider(window, cx))
            })
            .child(self.render_data_format(window, cx))
            .child(self.render_viewer_mode(window, cx))
            .child(self.render_errors(window, cx))
    }
}
