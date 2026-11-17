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

//! Placeholder shown in a tool route whose panel cannot work on this server:
//! names the panel, the command it needs and why that command is unusable
//! (missing on the server vs. denied to this user), with a re-probe and a way
//! back. `content.rs` swaps it in for the real panel while
//! `ZedisServerState::panel_block` says so, and drops it when the feature
//! matrix changes.

use crate::connection::{CommandStatus, ServerCommand};
use crate::states::{ServerView, ZedisGlobalStore, ZedisServerState, i18n_common, i18n_features, server_view_title};
use gpui::{Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, rems};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    v_flex,
};
use rust_i18n::t;

pub struct ZedisUnsupportedPanel {
    server_state: Entity<ZedisServerState>,
    view: ServerView,
    command: ServerCommand,
    status: CommandStatus,
}

impl ZedisUnsupportedPanel {
    pub fn new(
        server_state: Entity<ZedisServerState>,
        view: ServerView,
        command: ServerCommand,
        status: CommandStatus,
    ) -> Self {
        Self {
            server_state,
            view,
            command,
            status,
        }
    }
}

impl Render for ZedisUnsupportedPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let panel = server_view_title(cx, self.view);
        let title: SharedString = t!("features.panel_unavailable_title", panel = panel, locale = &locale)
            .to_string()
            .into();
        let reason = i18n_features(cx, self.status.i18n_key());
        let body: SharedString = t!(
            "features.panel_unavailable_body",
            command = self.command.label(),
            reason = reason,
            locale = &locale
        )
        .to_string()
        .into();
        let hint = match self.status {
            CommandStatus::Missing => Some(i18n_features(cx, "hint_missing")),
            CommandStatus::Denied => Some(i18n_features(cx, "hint_denied")),
            CommandStatus::Available | CommandStatus::Unknown => None,
        };
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let max_body_width = rems(32.5);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                div()
                    .size_10()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius_lg)
                    .bg(warning.alpha(0.1))
                    .border_1()
                    .border_color(warning.alpha(0.25))
                    .child(Icon::new(IconName::TriangleAlert).size_5().text_color(warning)),
            )
            .child(Label::new(title).font_semibold().text_lg().text_center())
            .child(
                Label::new(body)
                    .text_sm()
                    .text_color(muted)
                    .text_center()
                    .whitespace_normal()
                    .max_w(max_body_width),
            )
            .when_some(hint, |this, hint| {
                this.child(
                    Label::new(hint)
                        .text_xs()
                        .text_color(muted)
                        .text_center()
                        .whitespace_normal()
                        .max_w(max_body_width),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .mt_2()
                    .child(
                        Button::new("unsupported-panel-reprobe")
                            .outline()
                            .label(i18n_features(cx, "reprobe"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| state.reprobe_features(cx));
                            })),
                    )
                    .child(
                        Button::new("unsupported-panel-back")
                            .primary()
                            .label(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _window, cx| {
                                cx.global::<ZedisGlobalStore>()
                                    .clone()
                                    .update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, ElementId, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};

    #[gpui::test]
    fn unsupported_panel_renders_reprobe_and_back_actions(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let command = ServerView::Acl.required_commands()[0];
        let handle: AnyWindowHandle = cx
            .open_window(size(px(960.), px(680.)), |window, cx| {
                let view = cx.new(|_| {
                    ZedisUnsupportedPanel::new(server_state.clone(), ServerView::Acl, command, CommandStatus::Denied)
                });
                Root::new(view, window, cx)
            })
            .into();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let reprobe = ElementId::Name("unsupported-panel-reprobe".into());
            let back = ElementId::Name("unsupported-panel-back".into());
            let elements = window.elements();
            assert!(elements.iter().any(|el| el.path().contains(&reprobe)));
            assert!(elements.iter().any(|el| el.path().contains(&back)));
        })
        .expect("render unsupported panel");
    }
}
