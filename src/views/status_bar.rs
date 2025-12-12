// Copyright 2025 Tree xie.
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

use crate::assets::CustomIconName;
use crate::states::ErrorMessage;
use crate::states::ServerEvent;
use crate::states::ServerTask;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_status_bar;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Task;
use gpui::Window;
use gpui::prelude::*;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::label::Label;
use std::time::Duration;
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
fn format_latency(latency: Option<Duration>, cx: &mut Context<ZedisStatusBar>) -> (SharedString, Hsla) {
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

// --- Local State ---

/// Local state for the status bar to cache formatted strings and colors.
/// This prevents re-calculating strings on every render frame.
#[derive(Default)]
struct StatusBarState {
    server_id: SharedString,
    size: SharedString,
    latency: (SharedString, Hsla),
    nodes: SharedString,
    scan_finished: bool,
    soft_wrap: bool,
    error: Option<ErrorMessage>,
}

pub struct ZedisStatusBar {
    state: StatusBarState,

    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize state from the current server state
        // Read only necessary fields to avoid cloning the entire state if it's large
        let (dbsize, scan_count, server_id, nodes, version, latency, scan_completed, soft_wrap) = {
            let state = server_state.read(cx);
            (
                state.dbsize(),
                state.scan_count(),
                state.server_id().to_string(),
                state.nodes(),
                state.version().to_string(),
                state.latency(),
                state.scan_completed(),
                state.soft_wrap(),
            )
        };

        let mut subscriptions = vec![];
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            match event {
                ServerEvent::HeartbeatReceived(latency) => {
                    this.state.latency = format_latency(Some(*latency), cx);
                }
                ServerEvent::ServerSelected(server_id) => {
                    this.reset();
                    this.state.server_id = server_id.clone();
                    this.state.soft_wrap = server_state.read(cx).soft_wrap();
                }
                ServerEvent::ServerInfoUpdated(_) => {
                    let state = server_state.read(cx);
                    this.state.nodes = format_nodes(state.nodes(), state.version());
                    this.state.latency = format_latency(state.latency(), cx);
                }
                ServerEvent::KeyScanStarted(_) => {
                    this.state.scan_finished = false;
                }
                ServerEvent::KeyScanFinished(_) => {
                    let state = server_state.read(cx);
                    this.state.size = format_size(state.dbsize(), state.scan_count());
                    this.state.scan_finished = true;
                }
                ServerEvent::KeyScanPaged(_) => {
                    let state = server_state.read(cx);
                    this.state.size = format_size(state.dbsize(), state.scan_count());
                }
                ServerEvent::ErrorOccurred(error) => {
                    this.state.error = Some(error.clone());
                }
                ServerEvent::TaskStarted(task) => {
                    // Clear error when a new task starts (except background ping)
                    if *task != ServerTask::Ping {
                        this.state.error = None;
                    }
                }
                _ => {
                    return;
                }
            }
            cx.notify();
        }));
        let mut this = Self {
            heartbeat_task: None,
            server_state: server_state.clone(),
            _subscriptions: subscriptions,
            state: StatusBarState {
                size: format_size(dbsize, scan_count),
                server_id: server_id.into(),
                latency: format_latency(latency, cx),
                nodes: format_nodes(nodes, &version),
                scan_finished: scan_completed,
                soft_wrap,
                ..Default::default()
            },
        };
        this.start_heartbeat(server_state, cx);

        info!("Creating new status bar view");
        this
    }
    /// Reset the state to default
    fn reset(&mut self) {
        self.state = StatusBarState::default();
    }
    /// Start the heartbeat task
    fn start_heartbeat(&mut self, server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) {
        // start task
        self.heartbeat_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let _ = server_state.update(cx, |state, cx| {
                    state.ping(cx);
                });
            }
        }));
    }
    /// Render the server status
    fn render_server_status(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_completed = self.state.scan_finished;
        h_flex()
            .items_center()
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
            .child(Label::new(self.state.size.clone()).mr_4())
            .child(Icon::new(CustomIconName::Network).text_color(cx.theme().primary).mr_1())
            .child(Label::new(self.state.nodes.clone()).mr_4())
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
                Label::new(self.state.latency.0.clone())
                    .text_color(self.state.latency.1)
                    .mr_4(),
            )
    }
    fn render_editor_settings(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(self.state.soft_wrap, |this| this.icon(IconName::Check))
            .label(i18n_status_bar(cx, "soft_wrap"))
            .on_click(cx.listener(|this, _, _window, cx| {
                this.state.soft_wrap = !this.state.soft_wrap;
                this.server_state.update(cx, |state, cx| {
                    state.set_soft_wrap(this.state.soft_wrap, cx);
                });
                cx.notify();
            }))
    }
    /// Render the error message
    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = &self.state.error else {
            return h_flex().flex_1();
        };
        // 记录出错的显示
        h_flex()
            .flex_1()
            .child(Label::new(data.message.clone()).text_xs().text_color(cx.theme().red))
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!("render status bar view");
        if self.state.server_id.is_empty() {
            return h_flex();
        }
        h_flex()
            .justify_between()
            .text_sm()
            .py_1p5()
            .px_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .child(self.render_server_status(window, cx))
            .child(self.render_editor_settings(window, cx))
            .child(self.render_errors(window, cx))
    }
}
