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
use crate::states::ZedisServerState;
use crate::states::i18n_status_bar;
use gpui::Entity;
use gpui::SharedString;
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

pub struct ZedisStatusBar {
    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
}
impl ZedisStatusBar {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut this = Self {
            server_state,
            heartbeat_task: None,
        };
        this.start_heartbeat(cx);
        this
    }

    fn start_heartbeat(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
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
        let server_state = self.server_state.read(cx);
        if server_state.server().is_empty() {
            return h_flex();
        }
        let dbsize = server_state.dbsize();
        let scan_count = server_state.scan_count();
        let text = if let Some(dbsize) = dbsize {
            format!("{scan_count}/{dbsize}")
        } else {
            "--".to_string()
        };
        let latency = server_state.latency();
        let (color, latency_text) = if let Some(latency) = latency {
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
                (color, format!("{ms}ms"))
            } else {
                (color, format!("{:.2}s", ms as f64 / 1000.0))
            }
        } else {
            (cx.theme().primary, "--".to_string())
        };
        let nodes = server_state.nodes();
        let nodes_description: SharedString = format!("{} / {}", nodes.0, nodes.1).into();
        let is_completed = server_state.scan_completed();
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
            .child(Label::new(text).mr_4())
            .child(
                Icon::new(CustomIconName::Network)
                    .text_color(cx.theme().primary)
                    .mr_1(),
            )
            .child(Label::new(nodes_description).mr_4())
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
            .child(Label::new(latency_text).text_color(color).mr_4())
    }

    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(data) = server_state.get_error_message() else {
            return h_flex();
        };
        // 记录出错的显示
        h_flex().child(
            Label::new(data.message)
                .text_xs()
                .text_color(cx.theme().red),
        )
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.server_state.read(cx).server().is_empty() {
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
