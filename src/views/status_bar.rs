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
use crate::states::ZedisServerState;
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
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::label::Label;
use std::time::Duration;

#[inline]
fn format_size_description(dbsize: Option<u64>, scan_count: usize) -> SharedString {
    if let Some(dbsize) = dbsize {
        format!("{scan_count}/{dbsize}")
    } else {
        "--".to_string()
    }
    .into()
}
#[inline]
fn format_latency_description(
    latency: Option<Duration>,
    cx: &mut Context<ZedisStatusBar>,
) -> (SharedString, Hsla) {
    if let Some(latency) = latency {
        let ms = latency.as_millis();
        let theme = cx.theme();
        let color = if ms < 50 {
            theme.green
        } else if ms < 500 {
            theme.yellow
        } else {
            theme.red
        };
        if ms < 1000 {
            (format!("{ms}ms").into(), color)
        } else {
            (format!("{:.2}s", ms as f64 / 1000.0).into(), color)
        }
    } else {
        ("--".to_string().into(), cx.theme().primary)
    }
}

#[inline]
fn format_nodes_description(nodes: (usize, usize), version: &str) -> SharedString {
    format!("{} / {} (v{})", nodes.0, nodes.1, version).into()
}

pub struct ZedisStatusBar {
    // state
    _server: SharedString,
    _size: SharedString,
    _latency: (SharedString, Hsla),
    _nodes: SharedString,
    _scan_finished: bool,
    _error: Option<ErrorMessage>,

    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let state = server_state.read(cx).clone();
        let dbsize = state.dbsize();
        let scan_count = state.scan_count();
        let mut subscriptions = vec![];
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| {
                match event {
                    ServerEvent::Heartbeat(latency) => {
                        this._latency = format_latency_description(Some(*latency), cx);
                    }
                    ServerEvent::SelectServer(server) => {
                        this.reset(cx);
                        this._server = server.clone();
                    }
                    ServerEvent::ServerUpdated(_) => {
                        let state = server_state.read(cx);
                        this._nodes = format_nodes_description(state.nodes(), state.version());
                        this._latency = format_latency_description(state.latency(), cx);
                    }
                    ServerEvent::ScanStart(_) => {
                        this._scan_finished = false;
                    }
                    ServerEvent::ScanFinish(_) => {
                        let state = server_state.read(cx);
                        this._size = format_size_description(state.dbsize(), state.scan_count());
                        this._scan_finished = true;
                    }
                    ServerEvent::ScanNext(_) => {
                        let state = server_state.read(cx);
                        this._size = format_size_description(state.dbsize(), state.scan_count());
                    }
                    ServerEvent::Error(error) => {
                        this._error = Some(error.clone());
                    }
                    ServerEvent::Spawn(_) => {
                        this._error = None;
                    }
                    _ => {
                        return;
                    }
                }
                cx.notify();
            }),
        );
        let mut this = Self {
            heartbeat_task: None,
            server_state: server_state.clone(),
            _subscriptions: subscriptions,
            _size: format_size_description(dbsize, scan_count),
            _server: state.server().to_string().into(),
            _latency: format_latency_description(None, cx),
            _nodes: format_nodes_description(state.nodes(), state.version()),
            _scan_finished: state.scan_completed(),
            _error: None,
        };
        this.start_heartbeat(server_state, cx);
        this
    }
    fn reset(&mut self, cx: &mut Context<Self>) {
        self._server = SharedString::default();
        self._nodes = SharedString::default();
        self._latency = format_latency_description(None, cx);
        self._size = SharedString::default();
        self._error = None;
    }

    fn start_heartbeat(&mut self, server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) {
        // start task
        self.heartbeat_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                let _ = server_state.update(cx, |state, cx| {
                    state.ping(cx);
                });
            }
        }));
    }

    fn render_server_status(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_completed = self._scan_finished;
        h_flex()
            .items_center()
            .child(
                Button::new("zedis-status-bar-scan-more")
                    .outline()
                    .small()
                    .disabled(is_completed)
                    .tooltip(if is_completed {
                        i18n_status_bar(cx, "scan_completed").to_string()
                    } else {
                        i18n_status_bar(cx, "scan_more_keys").to_string()
                    })
                    .mr_1()
                    .icon(CustomIconName::ChevronsDown)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            state.scan_next(cx);
                        });
                    })),
            )
            .child(Label::new(self._size.clone()).mr_4())
            .child(
                Icon::new(CustomIconName::Network)
                    .text_color(cx.theme().primary)
                    .mr_1(),
            )
            .child(Label::new(self._nodes.clone()).mr_4())
            .child(
                Button::new("zedis-status-bar-letency")
                    .ghost()
                    .disabled(true)
                    .tooltip(i18n_status_bar(cx, "latency").to_string())
                    .icon(
                        Icon::new(CustomIconName::ChevronsLeftRightEllipsis)
                            .text_color(cx.theme().primary)
                            .mr_1(),
                    ),
            )
            .child(
                Label::new(self._latency.0.clone())
                    .text_color(self._latency.1)
                    .mr_4(),
            )
    }

    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = &self._error else {
            return h_flex();
        };
        // 记录出错的显示
        h_flex().child(
            Label::new(data.message.clone())
                .text_xs()
                .text_color(cx.theme().red),
        )
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!("render status bar view");
        if self._server.is_empty() {
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
            .child(self.render_errors(window, cx))
    }
}
