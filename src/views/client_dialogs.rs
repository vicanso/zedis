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

//! Dialog bodies of the Clients panel: `CLIENT PAUSE` (duration + mode)
//! and a filtered `CLIENT KILL` (ids, addresses, user, type, age).
//!
//! Form bodies only — the panel wraps each in a [`zedis_ui::ZedisDialog`]
//! and reads the result on OK. View entities, because a dialog body that
//! holds an `Input` has to be one (see CLAUDE.md).

use crate::connection::{KillFilter, PauseMode, kill_filter_commands, kill_filter_summary};
use crate::states::i18n_clients_manager;
use gpui::{Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    radio::{Radio, RadioGroup},
    v_flex,
};

pub struct ZedisClientPauseDialog {
    timeout: Entity<InputState>,
    mode: PauseMode,
    /// `WRITE` / `ALL` exist from Redis 6.2; before that a pause is `ALL`.
    mode_supported: bool,
    error: Option<SharedString>,
}

impl ZedisClientPauseDialog {
    pub fn new(mode_supported: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            timeout: cx.new(|cx| InputState::new(window, cx).default_value("5000")),
            mode: if mode_supported {
                PauseMode::Write
            } else {
                PauseMode::All
            },
            mode_supported,
            error: None,
        }
    }

    /// The duration in milliseconds and the mode, or `None` with the error
    /// shown.
    pub fn validate(&mut self, cx: &mut Context<Self>) -> Option<(u64, PauseMode)> {
        match self.timeout.read(cx).value().trim().parse::<u64>() {
            Ok(ms) if ms > 0 => {
                self.error = None;
                Some((ms, self.mode))
            }
            _ => {
                self.error = Some(i18n_clients_manager(cx, "pause_invalid"));
                cx.notify();
                None
            }
        }
    }
}

impl Render for ZedisClientPauseDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let selected = match self.mode {
            PauseMode::Write => 0,
            PauseMode::All => 1,
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(field(
                i18n_clients_manager(cx, "pause_timeout"),
                &self.timeout,
                Some(140.),
                muted,
            ))
            .child(
                RadioGroup::vertical("client-pause-mode")
                    .selected_index(Some(selected))
                    .child(
                        Radio::new("client-pause-write")
                            .label(i18n_clients_manager(cx, "pause_mode_write"))
                            .disabled(!self.mode_supported),
                    )
                    .child(Radio::new("client-pause-all").label(i18n_clients_manager(cx, "pause_mode_all")))
                    .on_click(cx.listener(|this, index: &usize, _window, cx| {
                        this.mode = if *index == 0 { PauseMode::Write } else { PauseMode::All };
                        cx.notify();
                    })),
            )
            .when(!self.mode_supported, |this| {
                this.child(
                    Label::new(i18n_clients_manager(cx, "pause_mode_write_hint"))
                        .text_xs()
                        .text_color(muted),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(danger))
            })
    }
}

/// Which `CLIENT KILL` filters this server takes.
#[derive(Clone, Copy, Debug, Default)]
pub struct KillFilterSupport {
    pub user: bool,
    pub laddr: bool,
    pub maxage: bool,
}

/// What the kill dialog produced: the commands to run and the text the
/// confirm prompt quotes.
pub struct KillFilterPlan {
    pub commands: Vec<Vec<String>>,
    pub summary: String,
}

/// `TYPE` choices, in radio order; `None` is "any".
const KILL_TYPES: [Option<&str>; 5] = [None, Some("normal"), Some("master"), Some("replica"), Some("pubsub")];

pub struct ZedisClientKillFilterDialog {
    ids: Entity<InputState>,
    addr: Entity<InputState>,
    laddr: Entity<InputState>,
    user: Entity<InputState>,
    maxage: Entity<InputState>,
    kind: usize,
    skipme: bool,
    support: KillFilterSupport,
    error: Option<SharedString>,
}

impl ZedisClientKillFilterDialog {
    pub fn new(support: KillFilterSupport, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            ids: input("42, 43", window, cx),
            addr: input("10.0.0.9:50000", window, cx),
            laddr: input("10.0.0.1:6379", window, cx),
            user: input("app", window, cx),
            maxage: input("3600", window, cx),
            kind: 0,
            skipme: true,
            support,
            error: None,
        }
    }

    /// The commands the form describes, or `None` with the error shown:
    /// nothing set, or a number that is not one.
    pub fn validate(&mut self, cx: &mut Context<Self>) -> Option<KillFilterPlan> {
        let text = |state: &Entity<InputState>| state.read(cx).value().trim().to_string();
        let optional = |value: String| (!value.is_empty()).then_some(value);
        let ids_text = text(&self.ids);
        let mut ids = Vec::new();
        for part in ids_text.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
            match part.parse::<u64>() {
                Ok(id) => ids.push(id),
                Err(_) => return self.fail("kill_filter_invalid", cx),
            }
        }
        let maxage_text = text(&self.maxage);
        let maxage_secs = if maxage_text.is_empty() {
            None
        } else {
            match maxage_text.parse::<u64>() {
                Ok(secs) if self.support.maxage => Some(secs),
                _ => return self.fail("kill_filter_invalid", cx),
            }
        };
        let filter = KillFilter {
            ids,
            addr: optional(text(&self.addr)),
            laddr: self.support.laddr.then(|| optional(text(&self.laddr))).flatten(),
            user: self.support.user.then(|| optional(text(&self.user))).flatten(),
            kind: KILL_TYPES.get(self.kind).copied().flatten().map(str::to_string),
            maxage_secs,
            skipme: self.skipme,
        };
        if filter.is_empty() {
            return self.fail("kill_filter_empty", cx);
        }
        self.error = None;
        let commands = kill_filter_commands(&filter);
        let summary = kill_filter_summary(&commands);
        Some(KillFilterPlan { commands, summary })
    }

    fn fail<T>(&mut self, key: &str, cx: &mut Context<Self>) -> Option<T> {
        self.error = Some(i18n_clients_manager(cx, key));
        cx.notify();
        None
    }
}

impl Render for ZedisClientKillFilterDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let unsupported = i18n_clients_manager(cx, "kill_filter_unsupported");
        // A filter the server lacks stays visible but inert, with the reason
        // beside its label — the form reads the same on every version.
        let gated = |label: SharedString, supported: bool| -> SharedString {
            if supported {
                label
            } else {
                format!("{label} · {unsupported}").into()
            }
        };
        let type_labels: [SharedString; 5] = [
            i18n_clients_manager(cx, "kill_filter_type_any"),
            i18n_clients_manager(cx, "flag_normal"),
            i18n_clients_manager(cx, "flag_master"),
            i18n_clients_manager(cx, "flag_replica"),
            i18n_clients_manager(cx, "flag_pubsub"),
        ];
        let mut types = RadioGroup::horizontal("client-kill-type").selected_index(Some(self.kind));
        for (ix, label) in type_labels.into_iter().enumerate() {
            types = types.child(Radio::new(("client-kill-type", ix)).label(label));
        }
        v_flex()
            .w_full()
            .gap_3()
            .child(field(
                i18n_clients_manager(cx, "kill_filter_ids"),
                &self.ids,
                None,
                muted,
            ))
            .child(
                h_flex()
                    .gap_3()
                    .child(field(
                        i18n_clients_manager(cx, "kill_filter_addr"),
                        &self.addr,
                        Some(200.),
                        muted,
                    ))
                    .child(gated_field(
                        gated(i18n_clients_manager(cx, "kill_filter_laddr"), self.support.laddr),
                        &self.laddr,
                        Some(200.),
                        self.support.laddr,
                        muted,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(gated_field(
                        gated(i18n_clients_manager(cx, "kill_filter_user"), self.support.user),
                        &self.user,
                        Some(200.),
                        self.support.user,
                        muted,
                    ))
                    .child(gated_field(
                        gated(i18n_clients_manager(cx, "kill_filter_maxage"), self.support.maxage),
                        &self.maxage,
                        Some(200.),
                        self.support.maxage,
                        muted,
                    )),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_clients_manager(cx, "kill_filter_type"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(types.on_click(cx.listener(|this, index: &usize, _window, cx| {
                        this.kind = *index;
                        cx.notify();
                    }))),
            )
            .child(
                Checkbox::new("client-kill-skipme")
                    .label(i18n_clients_manager(cx, "kill_filter_skipme"))
                    .checked(self.skipme)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.skipme = *checked;
                        cx.notify();
                    })),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(danger))
            })
    }
}

/// A labelled input: the label above the box, in the muted tone.
fn field(label: SharedString, state: &Entity<InputState>, width: Option<f32>, muted: gpui::Hsla) -> impl IntoElement {
    gated_field(label, state, width, true, muted)
}

fn gated_field(
    label: SharedString,
    state: &Entity<InputState>,
    width: Option<f32>,
    enabled: bool,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let input = Input::new(state).small().disabled(!enabled);
    v_flex()
        .gap_1()
        .child(Label::new(label).text_xs().text_color(muted))
        .child(match width {
            Some(w) => input.w(px(w)).into_any_element(),
            None => input.w_full().into_any_element(),
        })
}
