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
    connection::RedisClientDescription,
    states::{
        ErrorMessage, ServerEvent, ServerTask, ViewMode, ZedisServerState, i18n_common, i18n_sidebar, i18n_status_bar,
    },
};
use gpui::{Entity, Hsla, SharedString, Subscription, Task, TextAlign, Window, div, prelude::*};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    tooltip::Tooltip,
};
use std::{sync::Arc, time::Duration};
use tracing::info;

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
    should_reset_viewer_mode: bool,
    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize state from the current server state
        // Read only necessary fields to avoid cloning the entire state if it's large

        let mut subscriptions = vec![];
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            match event {
                ServerEvent::ServerSelected(_) => {
                    this.state.data_format = None;
                }
                ServerEvent::ServerRedisInfoUpdated(_) => {
                    this.fill_state(server_state, cx);
                }
                ServerEvent::ServerInfoUpdated(_) => {
                    server_state.update(cx, |state, cx| {
                        state.refresh_redis_info(cx);
                    });
                }
                ServerEvent::KeyScanStarted(_) => {
                    this.state.server_state.scan_finished = false;
                }
                ServerEvent::KeyScanFinished(_) => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
                    this.state.server_state.scan_finished = true;
                }
                ServerEvent::KeyScanPaged(_) => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
                }
                ServerEvent::ErrorOccurred(error) => {
                    this.state.error = Some(error.clone());
                }
                ServerEvent::TaskStarted(task) => {
                    // Clear error when a new task starts (except background ping)
                    if *task != ServerTask::RefreshRedisInfo {
                        this.state.error = None;
                    }
                }
                ServerEvent::ValueLoaded(_) => {
                    let state = server_state.read(cx);
                    this.should_reset_viewer_mode = true;
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
        let mut this = Self {
            heartbeat_task: None,
            viewer_mode_state,
            server_state: server_state.clone(),
            _subscriptions: subscriptions,
            should_reset_viewer_mode: false,
            state: StatusBarState { ..Default::default() },
        };
        this.fill_state(server_state.clone(), cx);
        this.start_heartbeat(server_state, cx);

        info!("Creating new status bar view");
        this
    }
    fn fill_state(&mut self, server_state: Entity<ZedisServerState>, cx: &Context<Self>) {
        let state = server_state.read(cx);
        let Some(redis_info) = state.redis_info() else {
            return;
        };
        self.state.server_state = StatusBarServerState {
            server_id: state.server_id().to_string().into(),
            size: format_size(state.dbsize(), state.scan_count()),
            latency: format_latency(Some(redis_info.latency), cx),
            used_memory: redis_info.used_memory_human.clone().into(),
            clients: format!("{} / {}", redis_info.blocked_clients, redis_info.connected_clients).into(),
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
    /// Render the server status
    fn render_server_status(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let is_completed = server_state.scan_finished;
        let nodes_description = server_state.nodes_description.clone();
        h_flex()
            .items_center()
            .child(
                Button::new("zedis-status-bar-key-collapse")
                    .outline()
                    .small()
                    .tooltip(i18n_status_bar(cx, "collapse_keys"))
                    .icon(CustomIconName::ListChecvronsDownUp)
                    .mr_1()
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            state.collapse_keys(cx);
                        });
                    })),
            )
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
            .child(Label::new(server_state.size.clone()).mr_4())
            .child(
                div()
                    .child(
                        h_flex()
                            .child(Icon::new(CustomIconName::Network).text_color(cx.theme().primary).mr_1())
                            .child(Label::new(server_state.nodes.clone()).mr_4()),
                    )
                    .id("zedis-servers")
                    .tooltip(move |window, cx| Tooltip::new(nodes_description.clone()).build(window, cx)),
            )
            .child(
                Button::new("zedis-status-bar-letency")
                    .ghost()
                    .disabled(true)
                    .tooltip(i18n_common(cx, "latency"))
                    .icon(
                        Icon::new(CustomIconName::ChevronsLeftRightEllipsis)
                            .text_color(cx.theme().primary)
                            .mr_1(),
                    ),
            )
            .child(
                Label::new(server_state.latency.0.clone())
                    .text_color(server_state.latency.1)
                    .mr_4(),
            )
            .child(
                Button::new("zedis-status-bar-used-memory")
                    .ghost()
                    .disabled(true)
                    .tooltip(i18n_common(cx, "used_memory"))
                    .icon(Icon::new(CustomIconName::MemoryStick))
                    .text_color(cx.theme().primary)
                    .label(server_state.used_memory.clone()),
            )
            .child(
                Button::new("zedis-status-bar-clients")
                    .ghost()
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
                this.state.server_state.soft_wrap = !this.state.server_state.soft_wrap;
                this.server_state.update(cx, |state, cx| {
                    state.set_soft_wrap(this.state.server_state.soft_wrap, cx);
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
        if self.should_reset_viewer_mode {
            self.viewer_mode_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(0)), window, cx);
            });
            self.should_reset_viewer_mode = false;
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
            .child(self.render_editor_settings(window, cx))
            .child(self.render_data_format(window, cx))
            .child(self.render_viewer_mode(window, cx))
            .child(self.render_errors(window, cx))
    }
}
