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

//! The sorted set's score-window filter (`ZRANGEBYSCORE`).
//!
//! The keyword box matches member *names*, which is the one thing a sorted
//! set is not organised by. This asks the question the type exists to
//! answer — "everything scoring between these two" — and pages the result
//! server-side instead of filtering whatever happened to be loaded.
//!
//! Either end may be left blank for "no bound", and `(` before a number
//! makes that end exclusive, exactly as Redis spells it.

use crate::helpers::normalize_score_bound;
use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_zset_editor};
use gpui::{App, Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use zedis_ui::ZedisDialog;

pub struct ZedisScoreFilterDialog {
    min: Entity<InputState>,
    max: Entity<InputState>,
}

impl ZedisScoreFilterDialog {
    pub fn new(current: Option<(SharedString, SharedString)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // An open end is stored as `-inf` / `+inf`; show it as blank so
        // re-opening the dialog reads like the form that produced it.
        let shown = |bound: Option<SharedString>| match bound.as_deref() {
            Some("-inf") | Some("+inf") | None => String::new(),
            Some(other) => other.to_string(),
        };
        let (min, max) = match current {
            Some((min, max)) => (shown(Some(min)), shown(Some(max))),
            None => (String::new(), String::new()),
        };
        Self {
            min: cx.new(|cx| InputState::new(window, cx).default_value(min).placeholder("-inf")),
            max: cx.new(|cx| InputState::new(window, cx).default_value(max).placeholder("+inf")),
        }
    }

    /// The window as `ZRANGEBYSCORE` spells it: a blank or unparsable end
    /// becomes the open end rather than an error, because "from 100 upwards"
    /// is the common half-open case and should not need `+inf` typed out.
    pub fn range(&self, cx: &App) -> (SharedString, SharedString) {
        let min = normalize_score_bound(&self.min.read(cx).value()).unwrap_or_else(|| "-inf".to_string());
        let max = normalize_score_bound(&self.max.read(cx).value()).unwrap_or_else(|| "+inf".to_string());
        (min.into(), max.into())
    }
}

impl Render for ZedisScoreFilterDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(i18n_zset_editor(cx, "score_filter_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_zset_editor(cx, "score_filter_min"))
                            .text_xs()
                            .text_color(muted),
                    )
                    // Inputs paint `size_full` and their state carries
                    // `flex_1`; pin the height so they don't inflate here.
                    .child(Input::new(&self.min).small().h(px(32.))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_zset_editor(cx, "score_filter_max"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&self.max).small().h(px(32.))),
            )
    }
}

/// Open the score-window form and apply what it returns.
pub fn open_score_filter_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let current = server_state.read(cx).zset_score_range();
    let view = cx.new(|cx| ZedisScoreFilterDialog::new(current, window, cx));
    let view_child = view.clone();
    let view_ok = view.clone();
    ZedisDialog::new(i18n_zset_editor(cx, "score_filter"))
        .w(px(380.))
        .ok_text(i18n_common(cx, "submit"))
        .cancel_text(i18n_common(cx, "cancel"))
        .button_props(
            dialog_button_props(cx)
                .ok_text(i18n_common(cx, "submit"))
                .cancel_text(i18n_common(cx, "cancel")),
        )
        .child(move || view_child.clone())
        .on_ok(move |_, _window, cx| {
            let (min, max) = view_ok.read(cx).range(cx);
            server_state.update(cx, |state, cx| state.filter_zset_by_score(min, max, cx));
            true
        })
        .open(window, cx);
}
