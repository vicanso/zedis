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

//! Sentinel dialog bodies: add a master (`SENTINEL MONITOR`) and edit a
//! master's settings (`SENTINEL SET`).
//!
//! Form bodies only — the Topology panel wraps each in a
//! [`zedis_ui::ZedisDialog`] and reads the values on OK. They are view
//! entities because a dialog body that holds an `Input` has to be one (the
//! dialog rebuilds inline elements every frame; see CLAUDE.md).

use crate::connection::SentinelMaster;
use crate::states::i18n_topology;
use gpui::{Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable, h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};

/// What `SENTINEL MONITOR` needs.
pub struct SentinelMonitorInput {
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub quorum: u32,
}

pub struct ZedisSentinelMonitorDialog {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    quorum: Entity<InputState>,
    error: Option<SharedString>,
}

impl ZedisSentinelMonitorDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("mymaster")),
            host: cx.new(|cx| InputState::new(window, cx).placeholder("10.0.0.5")),
            port: cx.new(|cx| InputState::new(window, cx).default_value("6379")),
            quorum: cx.new(|cx| InputState::new(window, cx).default_value("2")),
            error: None,
        }
    }

    /// The form's values, or `None` with the error shown in the body.
    pub fn validate(&mut self, cx: &mut Context<Self>) -> Option<SentinelMonitorInput> {
        let name: SharedString = self.name.read(cx).value().trim().to_string().into();
        let host: SharedString = self.host.read(cx).value().trim().to_string().into();
        let port = self.port.read(cx).value().trim().parse::<u16>().ok().filter(|p| *p > 0);
        let quorum = self
            .quorum
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|q| *q > 0);
        match (name.is_empty() || host.is_empty(), port, quorum) {
            (false, Some(port), Some(quorum)) => {
                self.error = None;
                Some(SentinelMonitorInput {
                    name,
                    host,
                    port,
                    quorum,
                })
            }
            _ => {
                self.error = Some(i18n_topology(cx, "sentinel_form_invalid"));
                cx.notify();
                None
            }
        }
    }
}

impl Render for ZedisSentinelMonitorDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        v_flex()
            .w_full()
            .gap_3()
            .child(field(i18n_topology(cx, "sentinel_field_name"), &self.name, None, muted))
            .child(field(i18n_topology(cx, "sentinel_field_host"), &self.host, None, muted))
            .child(
                h_flex()
                    .gap_3()
                    .child(field(
                        i18n_topology(cx, "sentinel_field_port"),
                        &self.port,
                        Some(120.),
                        muted,
                    ))
                    .child(field(
                        i18n_topology(cx, "sentinel_field_quorum"),
                        &self.quorum,
                        Some(120.),
                        muted,
                    )),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(danger))
            })
    }
}

pub struct ZedisSentinelSetDialog {
    master: SentinelMaster,
    quorum: Entity<InputState>,
    down_after: Entity<InputState>,
    failover_timeout: Entity<InputState>,
    parallel_syncs: Entity<InputState>,
    auth_pass: Entity<InputState>,
    error: Option<SharedString>,
}

impl ZedisSentinelSetDialog {
    pub fn new(master: SentinelMaster, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let number = |value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let placeholder = i18n_topology(cx, "sentinel_field_auth_pass_placeholder");
        Self {
            quorum: number(master.quorum.to_string(), window, cx),
            down_after: number(master.down_after_ms.to_string(), window, cx),
            failover_timeout: number(master.failover_timeout_ms.to_string(), window, cx),
            parallel_syncs: number(master.parallel_syncs.to_string(), window, cx),
            auth_pass: cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(true)),
            master,
            error: None,
        }
    }

    /// The `SENTINEL SET` option pairs that differ from what the sentinel
    /// reported (the password whenever one was typed), or `None` with the
    /// error shown when a number does not parse.
    pub fn changed_options(&mut self, cx: &mut Context<Self>) -> Option<Vec<(String, String)>> {
        let read = |state: &Entity<InputState>| state.read(cx).value().trim().to_string();
        let parse = |text: &str| text.parse::<u64>().ok();
        let quorum = parse(&read(&self.quorum)).filter(|q| *q > 0);
        let down_after = parse(&read(&self.down_after)).filter(|ms| *ms > 0);
        let failover_timeout = parse(&read(&self.failover_timeout)).filter(|ms| *ms > 0);
        let parallel_syncs = parse(&read(&self.parallel_syncs)).filter(|n| *n > 0);
        let (Some(quorum), Some(down_after), Some(failover_timeout), Some(parallel_syncs)) =
            (quorum, down_after, failover_timeout, parallel_syncs)
        else {
            self.error = Some(i18n_topology(cx, "sentinel_form_invalid"));
            cx.notify();
            return None;
        };
        self.error = None;
        let mut options = Vec::new();
        let mut push = |changed: bool, option: &str, value: u64| {
            if changed {
                options.push((option.to_string(), value.to_string()));
            }
        };
        push(quorum != u64::from(self.master.quorum), "quorum", quorum);
        push(
            down_after != self.master.down_after_ms,
            "down-after-milliseconds",
            down_after,
        );
        push(
            failover_timeout != self.master.failover_timeout_ms,
            "failover-timeout",
            failover_timeout,
        );
        push(
            parallel_syncs != u64::from(self.master.parallel_syncs),
            "parallel-syncs",
            parallel_syncs,
        );
        let auth_pass = read(&self.auth_pass);
        if !auth_pass.is_empty() {
            options.push(("auth-pass".to_string(), auth_pass));
        }
        Some(options)
    }
}

impl Render for ZedisSentinelSetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(field(
                        i18n_topology(cx, "sentinel_field_quorum"),
                        &self.quorum,
                        Some(120.),
                        muted,
                    ))
                    .child(field(
                        i18n_topology(cx, "sentinel_field_parallel_syncs"),
                        &self.parallel_syncs,
                        Some(120.),
                        muted,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(field(
                        i18n_topology(cx, "sentinel_field_down_after"),
                        &self.down_after,
                        Some(160.),
                        muted,
                    ))
                    .child(field(
                        i18n_topology(cx, "sentinel_field_failover_timeout"),
                        &self.failover_timeout,
                        Some(160.),
                        muted,
                    )),
            )
            .child(field(
                i18n_topology(cx, "sentinel_field_auth_pass"),
                &self.auth_pass,
                None,
                muted,
            ))
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(danger))
            })
    }
}

/// A labelled input: the label above the box, in the muted tone.
fn field(label: SharedString, state: &Entity<InputState>, width: Option<f32>, muted: gpui::Hsla) -> impl IntoElement {
    let input = Input::new(state).small();
    v_flex()
        .gap_1()
        .child(Label::new(label).text_xs().text_color(muted))
        .child(match width {
            Some(w) => input.w(px(w)).into_any_element(),
            None => input.w_full().into_any_element(),
        })
}
