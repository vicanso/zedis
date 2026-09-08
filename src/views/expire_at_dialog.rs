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

//! Absolute-expiry picker for the selected key (`EXPIREAT`).
//!
//! Reached from the editor key bar's TTL dropdown. A duration field answers
//! "keep this for an hour"; this one answers "drop this at midnight", which
//! is arithmetic nobody should do by hand and which goes stale while they
//! type it.
//!
//! `DatePicker` is date-only, so the instant is composed here from a picked
//! `NaiveDate` plus a typed `HH:MM[:SS]`, both read in the Settings time
//! zone like every other timestamp in the app. The preview line resolves
//! them the same way the OK button will, so what the dialog says is what
//! the server gets — including the refusal to accept an instant that has
//! already passed, because `EXPIREAT` in the past *deletes the key* and
//! that belongs to the delete button, not to a mistyped hour.

use crate::helpers::{
    format_duration_units, format_unix_secs, parse_clock_input, unix_secs_from_parts, unix_secs_to_parts, unix_ts,
};
use crate::states::{ZedisGlobalStore, i18n_editor, i18n_expire_at};
use chrono::{Duration as ChronoDuration, NaiveDate, NaiveTime};
use gpui::{Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::calendar::Matcher;
use gpui_kit::component::date_picker::{DatePicker, DatePickerState};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::Button,
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use rust_i18n::t;
use std::time::Duration;

/// One "…from now" / "…at" shortcut. `apply` returns the instant it means,
/// resolved against the current clock at click time rather than at render.
struct ExpirePreset {
    id: &'static str,
    i18n_key: &'static str,
    offset: ChronoDuration,
    /// Truncate to the start of the day after applying the offset — how
    /// "tonight" and "tomorrow" differ from "+24h".
    midnight: bool,
}

const PRESETS: &[ExpirePreset] = &[
    ExpirePreset {
        id: "1h",
        i18n_key: "preset_1h",
        offset: ChronoDuration::hours(1),
        midnight: false,
    },
    ExpirePreset {
        id: "1d",
        i18n_key: "preset_1d",
        offset: ChronoDuration::days(1),
        midnight: false,
    },
    ExpirePreset {
        id: "7d",
        i18n_key: "preset_7d",
        offset: ChronoDuration::days(7),
        midnight: false,
    },
    ExpirePreset {
        id: "midnight",
        i18n_key: "preset_midnight",
        offset: ChronoDuration::days(1),
        midnight: true,
    },
];

pub struct ZedisExpireAtDialog {
    date_state: Entity<DatePickerState>,
    time_state: Entity<InputState>,
}

impl ZedisExpireAtDialog {
    /// `current` is the key's existing expiry (unix seconds), used to
    /// prefill both controls so nudging a deadline is an edit and not a
    /// retype. A key with no expiry starts an hour out — a deadline the
    /// user is about to replace anyway, and never an empty form.
    pub fn new(current: Option<i64>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let seed = current.filter(|at| *at > unix_ts()).unwrap_or_else(|| unix_ts() + 3600);
        let (date, time) =
            unix_secs_to_parts(seed).unwrap_or_else(|| (chrono::Local::now().date_naive(), NaiveTime::MIN));
        // Yesterday and earlier are greyed out: no time of day can rescue a
        // date that is already over, and the preview would only be able to
        // say so after the fact. Today stays selectable — a later hour
        // today is a perfectly good deadline.
        let today = unix_secs_to_parts(unix_ts()).map(|(date, _)| date);
        let date_state = cx.new(|cx| {
            let mut state = DatePickerState::new(window, cx).date_format("%Y-%m-%d");
            if let Some(today) = today {
                state = state.disabled_matcher(Matcher::custom(move |day: &NaiveDate| *day < today));
            }
            state.set_date(date, window, cx);
            state
        });
        let time_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("14:30:00")
                .default_value(time.format("%H:%M:%S").to_string())
        });
        Self { date_state, time_state }
    }

    /// The picked date, if the calendar has one.
    fn date(&self, cx: &gpui::App) -> Option<NaiveDate> {
        self.date_state.read(cx).date().start()
    }

    /// The composed instant, or `None` when the date is missing, the clock
    /// field does not parse, or the zone has no such wall-clock moment.
    pub fn expire_at(&self, cx: &gpui::App) -> Option<i64> {
        let date = self.date(cx)?;
        let time = parse_clock_input(&self.time_state.read(cx).value())?;
        unix_secs_from_parts(date, time)
    }

    fn apply_preset(&mut self, preset_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = PRESETS.iter().find(|p| p.id == preset_id) else {
            return;
        };
        let Some(target) = chrono::DateTime::from_timestamp(unix_ts(), 0) else {
            return;
        };
        let target = target + preset.offset;
        let Some((date, mut time)) = unix_secs_to_parts(target.timestamp()) else {
            return;
        };
        // "Tomorrow 00:00" is the offset's date at the start of the day —
        // which is a different instant from "+24h" whenever it is not
        // already midnight, and the one people actually mean.
        if preset.midnight {
            time = NaiveTime::MIN;
        }
        self.date_state.update(cx, |state, cx| state.set_date(date, window, cx));
        self.time_state.update(cx, |state, cx| {
            state.set_value(time.format("%H:%M:%S").to_string(), window, cx);
        });
        cx.notify();
    }
}

impl Render for ZedisExpireAtDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let now = unix_ts();
        // The preview resolves exactly what OK will send, so a form that
        // reads fine here can never surprise on submit.
        let (preview, preview_color) = match self.expire_at(cx) {
            None => (i18n_expire_at(cx, "invalid"), cx.theme().danger),
            Some(at) if at <= now => (i18n_expire_at(cx, "in_past"), cx.theme().danger),
            Some(at) => {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                let absolute = format_unix_secs(at).unwrap_or_default();
                let ago = format_duration_units(Duration::from_secs((at - now).max(0) as u64));
                let relative = t!("expire_at.in_label", ago = ago, locale = locale);
                (
                    SharedString::from(format!(
                        "{} {absolute} · {relative}",
                        i18n_editor(cx, "expires_at_label")
                    )),
                    muted,
                )
            }
        };

        let mut presets = h_flex().gap_2().flex_wrap();
        for preset in PRESETS {
            let id = preset.id;
            presets = presets.child(
                Button::new(SharedString::from(format!("expire-at-preset-{id}")))
                    .small()
                    .outline()
                    .label(i18n_expire_at(cx, preset.i18n_key))
                    .on_click(cx.listener(move |this, _, window, cx| this.apply_preset(id, window, cx))),
            );
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new(i18n_expire_at(cx, "date")).text_xs().text_color(muted))
                            .child(DatePicker::new(&self.date_state).small().w(px(180.))),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new(i18n_expire_at(cx, "time")).text_xs().text_color(muted))
                            // Height pinned: an Input paints `size_full` and
                            // its state carries `flex_1`, which inflates in a
                            // column that has room to give.
                            .child(Input::new(&self.time_state).small().w(px(120.)).h(px(32.))),
                    ),
            )
            .child(presets)
            .child(Label::new(preview).text_xs().text_color(preview_color))
    }
}
