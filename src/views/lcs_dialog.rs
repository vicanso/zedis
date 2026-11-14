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

//! Dialog for comparing the active String key against another key on the same
//! DB via Redis `LCS` (Redis 7.0+ / Valkey 7.2+).

use crate::connection::{ServerDb, string_lcs};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_editor};
use gpui::{App, AppContext, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    spinner::Spinner,
    v_flex,
};
use rust_i18n::t;
use zedis_ui::ZedisDialog;

struct LcsDialog {
    server_state: Entity<ZedisServerState>,
    at: ServerDb,
    source_key: SharedString,
    other_key: Entity<InputState>,
    loading: bool,
    outcome: Option<Result<SharedString, SharedString>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LcsDialog {
    fn new(
        server_state: Entity<ZedisServerState>,
        at: ServerDb,
        source_key: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let other_key =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_editor(cx, "lcs_other_key_placeholder")));
        let sub = cx.subscribe_in(&other_key, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.run_compare(window, cx);
            }
        });
        Self {
            server_state,
            at,
            source_key,
            other_key,
            loading: false,
            outcome: None,
            _task: None,
            _subscriptions: vec![sub],
        }
    }

    fn run_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let other = self.other_key.read(cx).value().trim().to_string();
        if other.is_empty() {
            window.push_notification(Notification::error(i18n_editor(cx, "lcs_other_key_empty")), cx);
            return;
        }
        let at = self.at.clone();
        let key1 = self.source_key.to_string();
        self.loading = true;
        self.outcome = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let res = string_lcs(&at, &key1, &other).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.outcome = Some(match res {
                    Ok(text) => Ok(SharedString::from(text)),
                    Err(err) => {
                        let app_err = Error::from(err);
                        let msg = SharedString::from(app_err.to_string());
                        this.server_state
                            .update(cx, |state, cx| state.note_command_error(&app_err, cx));
                        Err(msg)
                    }
                });
                cx.notify();
            });
        }));
    }
}

impl Render for LcsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let border = theme.border;
        let bg = theme.secondary;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        let source_desc: SharedString = t!(
            "editor.lcs_source_key",
            key = self.source_key.as_ref(),
            locale = &locale
        )
        .to_string()
        .into();

        v_flex()
            .gap_3()
            .w_full()
            .child(Label::new(source_desc).text_xs().text_color(muted))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_editor(cx, "lcs_other_key_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&self.other_key).small()),
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
            .when_some(self.outcome.clone(), |this, outcome| match outcome {
                Err(err) => this.child(Label::new(err).text_xs().text_color(danger)),
                Ok(lcs) => {
                    let summary: SharedString = if lcs.is_empty() {
                        i18n_editor(cx, "lcs_empty_result")
                    } else {
                        t!("editor.lcs_result_summary", bytes = lcs.len(), locale = &locale)
                            .to_string()
                            .into()
                    };
                    let copy_val = lcs.clone();
                    this.child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(Label::new(summary).text_xs().text_color(muted))
                            .when(!lcs.is_empty(), |row| {
                                row.child(
                                    Button::new("lcs-copy-result")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Copy)
                                        .label(i18n_editor(cx, "lcs_copy"))
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_val.to_string()));
                                            window.push_notification(
                                                Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                                cx,
                                            );
                                        }),
                                )
                            }),
                    )
                    .when(!lcs.is_empty(), |col| {
                        col.child(
                            v_flex()
                                .id("lcs-result-scroll")
                                .w_full()
                                .h(px(160.))
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(border)
                                .bg(bg)
                                .overflow_y_scrollbar()
                                .child(Label::new(lcs).text_xs().font_family(get_mono_font_family())),
                        )
                    })
                }
            })
    }
}

pub(crate) fn open_lcs_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let (at, Some(source_key)) = ({
        let state = server_state.read(cx);
        (state.at(), state.key())
    }) else {
        return;
    };

    let view = cx.new(|cx| LcsDialog::new(server_state, at, source_key, window, cx));
    let body = view.clone();
    let input = view.read(cx).other_key.clone();
    let ok_label = i18n_editor(cx, "lcs_run");
    let cancel_label = i18n_common(cx, "close");

    ZedisDialog::new(i18n_editor(cx, "lcs_title"))
        .w(px(460.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, window, cx| {
            view.update(cx, |dialog, cx| dialog.run_compare(window, cx));
            false
        })
        .open(window, cx);
    input.update(cx, |field, cx| field.focus(window, cx));
}
