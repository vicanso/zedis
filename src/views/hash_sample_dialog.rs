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

//! Random field sampler dialog for HASH keys backed by `HRANDFIELD` (Redis 6.2+).

use crate::connection::{ServerDb, hash_rand_field};
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_hash_editor};
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
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
use zedis_ui::ZedisDialog;

const DEFAULT_SAMPLE_COUNT: isize = 10;
const MAX_SAMPLE_COUNT: isize = 100;

struct HashSampleDialog {
    at: ServerDb,
    key: SharedString,
    count_input: Entity<InputState>,
    loading: bool,
    pairs: Option<Result<Vec<(SharedString, SharedString)>, SharedString>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl HashSampleDialog {
    fn new(at: ServerDb, key: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let count_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(DEFAULT_SAMPLE_COUNT.to_string(), window, cx);
            input
        });
        let sub = cx.subscribe_in(&count_input, window, |this, _, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.sample(cx);
            }
        });
        let mut this = Self {
            at,
            key,
            count_input,
            loading: false,
            pairs: None,
            _task: None,
            _subscriptions: vec![sub],
        };
        this.sample(cx);
        this
    }

    fn sample(&mut self, cx: &mut Context<Self>) {
        let count = self
            .count_input
            .read(cx)
            .value()
            .trim()
            .parse::<isize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SAMPLE_COUNT)
            .min(MAX_SAMPLE_COUNT);
        let at = self.at.clone();
        let key = self.key.to_string();
        self.loading = true;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let res = hash_rand_field(&at, &key, count)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|(f, v)| {
                            (
                                SharedString::from(String::from_utf8_lossy(&f).into_owned()),
                                SharedString::from(String::from_utf8_lossy(&v).into_owned()),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(|e| SharedString::from(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.pairs = Some(res);
                cx.notify();
            });
        }));
    }
}

impl Render for HashSampleDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let border = theme.border;

        v_flex()
            .gap_3()
            .w_full()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Label::new(i18n_hash_editor(cx, "sample_count_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(div().w(px(96.)).child(Input::new(&self.count_input).small()))
                    .when(self.loading, |row| row.child(Spinner::new().small())),
            )
            .when_some(self.pairs.clone(), |this, outcome| match outcome {
                Err(err) => this.child(Label::new(err).text_xs().text_color(danger)),
                Ok(rows) if rows.is_empty() => this.child(
                    Label::new(i18n_hash_editor(cx, "sample_empty"))
                        .text_xs()
                        .text_color(muted),
                ),
                Ok(rows) => this.child(
                    v_flex()
                        .id("hash-sample-results")
                        .w_full()
                        .h(px(220.))
                        .p_2()
                        .gap_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .overflow_y_scrollbar()
                        .children(rows.into_iter().enumerate().map(|(ix, (field, value))| {
                            let copy_val = value.clone();
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .py_0p5()
                                .child(
                                    div()
                                        .w(px(140.))
                                        .truncate()
                                        .child(Label::new(field).text_xs().font_family(get_mono_font_family())),
                                )
                                .child(
                                    div().flex_1().min_w_0().truncate().child(
                                        Label::new(value)
                                            .text_xs()
                                            .text_color(muted)
                                            .font_family(get_mono_font_family()),
                                    ),
                                )
                                .child(
                                    Button::new(("hash-sample-copy", ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Copy)
                                        .tooltip(i18n_common(cx, "copy_cell_tooltip"))
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_val.to_string()));
                                            window.push_notification(
                                                Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                                cx,
                                            );
                                        }),
                                )
                        })),
                ),
            })
    }
}

pub(crate) fn open_hash_sample_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let (at, Some(key)) = ({
        let state = server_state.read(cx);
        (state.at(), state.key())
    }) else {
        return;
    };

    let view = cx.new(|cx| HashSampleDialog::new(at, key, window, cx));
    let body = view.clone();
    let ok_label = i18n_hash_editor(cx, "sample_resample");
    let cancel_label = i18n_common(cx, "close");

    ZedisDialog::new(i18n_hash_editor(cx, "sample_fields_title"))
        .w(px(500.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, _window, cx| {
            view.update(cx, |dialog, cx| dialog.sample(cx));
            false
        })
        .open(window, cx);
}
