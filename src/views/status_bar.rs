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
use crate::states::ZedisAppState;
use crate::states::ZedisServerState;
use gpui::Entity;
use gpui::Window;
use gpui::prelude::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::label::Label;

pub struct ZedisStatusBar {
    app_state: Entity<ZedisAppState>,
    server_state: Entity<ZedisServerState>,
}
impl ZedisStatusBar {
    pub fn new(
        _window: &mut Window,
        _cx: &mut Context<Self>,
        app_state: Entity<ZedisAppState>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self {
            server_state,
            app_state,
        }
    }

    fn render_server_status(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        if server_state.server().is_empty() {
            return h_flex();
        }
        let dbsize = server_state.dbsize();
        let scan_count = server_state.scan_count();
        let text = if let Some(scan_count) = scan_count
            && let Some(dbsize) = dbsize
        {
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
            (color, format!("{:.2}s", ms as f64 / 1000.0))
        } else {
            (cx.theme().primary, "--".to_string())
        };
        h_flex()
            .items_center()
            .child(
                Icon::new(CustomIconName::Key)
                    .text_color(cx.theme().primary)
                    .mr_1(),
            )
            .child(Label::new(format!(": {text}")).mr_4())
            .child(
                Icon::new(CustomIconName::ChevronsLeftRightEllipsis)
                    .text_color(cx.theme().primary)
                    .mr_1(),
            )
            .child(
                h_flex()
                    .child(Label::new(":").mx_1())
                    .child(Label::new(latency_text).text_color(color)),
            )
    }

    fn render_soft_wrap_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(true, |this| this.icon(IconName::Check))
            .label("Soft Wrap")
            .on_click(cx.listener(|_this, _, _window, cx| {
                // this.soft_wrap = !this.soft_wrap;
                // this.editor.update(cx, |state, cx| {
                //     state.set_soft_wrap(this.soft_wrap, window, cx);
                // });
                cx.notify();
            }))
    }

    fn render_indent_guides_button(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new("indent-guides")
            .ghost()
            .xsmall()
            .when(true, |this| this.icon(IconName::Check))
            .label("Indent Guides")
            .on_click(cx.listener(|_this, _, _window, cx| {
                // this.indent_guides = !this.indent_guides;
                // this.editor.update(cx, |state, cx| {
                //     state.set_indent_guides(this.indent_guides, window, cx);
                // });
                cx.notify();
            }))
    }
    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 记录出错的显示
        h_flex()
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .justify_between()
            .text_sm()
            .py_1p5()
            .px_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .child(
                h_flex()
                    .gap_3()
                    .child(self.render_server_status(window, cx))
                    .child(self.render_soft_wrap_button(window, cx))
                    .child(self.render_indent_guides_button(window, cx)),
            )
            .child(self.render_errors(window, cx))
    }
}
