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

//! Batch SET membership check dialog backed by `SMISMEMBER` (Redis 6.2+).

use crate::connection::{ServerDb, set_ismember};
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_set_editor};
use gpui::{App, AppContext, Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt, h_flex,
    input::{Textarea, TextareaState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    spinner::Spinner,
    v_flex,
};
use rust_i18n::t;
use zedis_ui::ZedisDialog;

struct SetIsmemberDialog {
    at: ServerDb,
    key: SharedString,
    members_input: Entity<TextareaState>,
    loading: bool,
    results: Option<Result<Vec<(SharedString, bool)>, SharedString>>,
    _task: Option<Task<()>>,
}

impl SetIsmemberDialog {
    fn new(at: ServerDb, key: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let members_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(i18n_set_editor(cx, "check_members_placeholder"))
                .auto_grow(3, 6)
        });
        Self {
            at,
            key,
            members_input,
            loading: false,
            results: None,
            _task: None,
        }
    }

    fn run_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.members_input.read(cx).value().to_string();
        let candidates: Vec<String> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        if candidates.is_empty() {
            window.push_notification(Notification::error(i18n_set_editor(cx, "check_members_empty")), cx);
            return;
        }
        let at = self.at.clone();
        let key = self.key.to_string();
        self.loading = true;
        self.results = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let refs: Vec<&[u8]> = candidates.iter().map(String::as_bytes).collect();
            let outcome = set_ismember(&at, &key, &refs)
                .await
                .map(|flags| {
                    candidates
                        .into_iter()
                        .zip(flags)
                        .map(|(m, present)| (SharedString::from(m), present))
                        .collect::<Vec<_>>()
                })
                .map_err(|e| SharedString::from(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.results = Some(outcome);
                cx.notify();
            });
        }));
    }
}

impl Render for SetIsmemberDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let success = theme.success;
        let danger = theme.danger;
        let border = theme.border;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        v_flex()
            .gap_3()
            .w_full()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_set_editor(cx, "check_members_hint"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Textarea::new(&self.members_input)),
            )
            .when(self.loading, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(Label::new(i18n_common(cx, "loading")).text_xs().text_color(muted)),
                )
            })
            .when_some(self.results.clone(), |this, outcome| match outcome {
                Err(err) => this.child(Label::new(err).text_xs().text_color(danger)),
                Ok(rows) => {
                    let present_count = rows.iter().filter(|(_, ok)| *ok).count();
                    let summary: SharedString = t!(
                        "set_editor.check_members_summary",
                        present = present_count,
                        total = rows.len(),
                        locale = &locale
                    )
                    .to_string()
                    .into();
                    let present_label = i18n_set_editor(cx, "member_present");
                    let absent_label = i18n_set_editor(cx, "member_absent");
                    this.child(Label::new(summary).text_xs().text_color(muted)).child(
                        v_flex()
                            .id("set-ismember-results")
                            .w_full()
                            .h(px(180.))
                            .p_2()
                            .gap_1()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .overflow_y_scrollbar()
                            .children(rows.into_iter().map(|(member, present)| {
                                let fg = if present { success } else { muted };
                                let status = if present {
                                    present_label.clone()
                                } else {
                                    absent_label.clone()
                                };
                                let icon = if present { IconName::Check } else { IconName::Close };
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .py_0p5()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(Label::new(member).text_xs().font_family(get_mono_font_family())),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .items_center()
                                            .child(Icon::new(icon).xsmall().text_color(fg))
                                            .child(Label::new(status).text_xs().text_color(fg)),
                                    )
                            })),
                    )
                }
            })
    }
}

pub(crate) fn open_set_ismember_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let (at, Some(key)) = ({
        let state = server_state.read(cx);
        (state.at(), state.key())
    }) else {
        return;
    };

    let view = cx.new(|cx| SetIsmemberDialog::new(at, key, window, cx));
    let body = view.clone();
    let input = view.read(cx).members_input.clone();
    let ok_label = i18n_set_editor(cx, "check_members_run");
    let cancel_label = i18n_common(cx, "close");

    ZedisDialog::new(i18n_set_editor(cx, "check_members_title"))
        .w(px(440.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, window, cx| {
            view.update(cx, |dialog, cx| dialog.run_check(window, cx));
            false
        })
        .open(window, cx);
    input.update(cx, |field, cx| field.focus(window, cx));
}
