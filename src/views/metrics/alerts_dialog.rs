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

//! Threshold alert rules dialog for a server connection.
//!
//! Strictly opt-in (`enabled = false` by default). Lets the user toggle
//! alert evaluation for the active server, configure each metric's
//! threshold, sustained duration, and cooldown, reset to defaults, and
//! inspect/clear recent triggered alerts.

use crate::connection::get_server;
use crate::helpers::{format_unix_millis_with, get_mono_font_family};
use crate::states::{ZedisServerState, alert_metric_label, dialog_button_props, i18n_common, i18n_metrics};
use gpui::{App, Entity, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use zedis_core::alerts::{AlertComparator, AlertMetric, AlertRule, TriggeredAlert, default_alert_rules};
use zedis_db::get_alert_rules_manager;
use zedis_ui::ZedisDialog;

pub(crate) const ALERTS_MASTER_CHECKBOX_ID: &str = "metrics-alerts-master-enable";
pub(crate) const ALERTS_RESET_BTN_ID: &str = "metrics-alerts-reset-defaults";
pub(crate) const ALERTS_CLEAR_HISTORY_BTN_ID: &str = "metrics-alerts-clear-history";

struct RuleRowState {
    metric: AlertMetric,
    comparator: AlertComparator,
    enabled: bool,
    threshold: Entity<InputState>,
    sustained_secs: Entity<InputState>,
    cooldown_secs: Entity<InputState>,
}

pub struct ZedisServerAlertsDialog {
    server_id: String,
    server_state: Entity<ZedisServerState>,
    enabled: bool,
    rows: Vec<RuleRowState>,
}

fn format_threshold(val: f64) -> String {
    if (val - val.round()).abs() < f64::EPSILON {
        format!("{val:.0}")
    } else {
        format!("{val:.2}")
    }
}

impl ZedisServerAlertsDialog {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_id = server_state.read(cx).server_id().to_string();
        let config_enabled = get_server(&server_id).ok().and_then(|s| s.alerts_enabled);
        let manager = get_alert_rules_manager();
        let enabled = manager.is_enabled(&server_id, config_enabled);
        let rules = manager.rules_for(&server_id);
        let rows = Self::build_rows(&rules, window, cx);
        Self {
            server_id,
            server_state,
            enabled,
            rows,
        }
    }

    fn build_rows(rules: &[AlertRule], window: &mut Window, cx: &mut Context<Self>) -> Vec<RuleRowState> {
        rules
            .iter()
            .map(|rule| {
                let threshold_str = format_threshold(rule.threshold);
                let sustained_str = rule.sustained_secs.to_string();
                let cooldown_str = rule.cooldown_secs.to_string();
                RuleRowState {
                    metric: rule.metric,
                    comparator: rule.comparator,
                    enabled: rule.enabled,
                    threshold: cx.new(|cx| InputState::new(window, cx).default_value(threshold_str)),
                    sustained_secs: cx.new(|cx| InputState::new(window, cx).default_value(sustained_str)),
                    cooldown_secs: cx.new(|cx| InputState::new(window, cx).default_value(cooldown_str)),
                }
            })
            .collect()
    }

    fn reset_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = default_alert_rules();
        for (row, def) in self.rows.iter_mut().zip(defaults.iter()) {
            row.enabled = def.enabled;
            row.comparator = def.comparator;
            let threshold_str = format_threshold(def.threshold);
            let sustained_str = def.sustained_secs.to_string();
            let cooldown_str = def.cooldown_secs.to_string();
            row.threshold.update(cx, |input, cx| {
                input.set_value(threshold_str, window, cx);
            });
            row.sustained_secs.update(cx, |input, cx| {
                input.set_value(sustained_str, window, cx);
            });
            row.cooldown_secs.update(cx, |input, cx| {
                input.set_value(cooldown_str, window, cx);
            });
        }
        cx.notify();
    }

    pub fn collect_config(&self, cx: &App) -> (bool, Vec<AlertRule>) {
        let defaults = default_alert_rules();
        let rules = self
            .rows
            .iter()
            .map(|row| {
                let fallback = defaults
                    .iter()
                    .find(|d| d.metric == row.metric)
                    .cloned()
                    .unwrap_or(AlertRule {
                        metric: row.metric,
                        enabled: true,
                        comparator: row.comparator,
                        threshold: 0.0,
                        sustained_secs: 0,
                        cooldown_secs: 300,
                    });
                let threshold = if row.metric == AlertMetric::LinkOffline {
                    0.5
                } else {
                    row.threshold
                        .read(cx)
                        .value()
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .unwrap_or(fallback.threshold)
                };
                let sustained_secs = row
                    .sustained_secs
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(fallback.sustained_secs)
                    .min(3600);
                let cooldown_secs = row
                    .cooldown_secs
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(fallback.cooldown_secs)
                    .clamp(10, 86_400);
                AlertRule {
                    metric: row.metric,
                    enabled: row.enabled,
                    comparator: row.comparator,
                    threshold,
                    sustained_secs,
                    cooldown_secs,
                }
            })
            .collect();
        (self.enabled, rules)
    }

    fn render_rule_row(&self, ix: usize, row: &RuleRowState, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let metric_name = alert_metric_label(cx, row.metric);
        let unit = row.metric.unit();
        let is_link_offline = row.metric == AlertMetric::LinkOffline;
        let row_disabled = !self.enabled || !row.enabled;

        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex().w(px(230.)).items_center().gap_2().child(
                    Checkbox::new(("alert-rule-enable", ix))
                        .checked(row.enabled)
                        .disabled(!self.enabled)
                        .label(metric_name)
                        .on_click(cx.listener(move |this, checked: &bool, _window, cx| {
                            if let Some(r) = this.rows.get_mut(ix) {
                                r.enabled = *checked;
                                cx.notify();
                            }
                        })),
                ),
            )
            .child(
                h_flex()
                    .w(px(140.))
                    .items_center()
                    .gap_1()
                    .when(!is_link_offline, |this| {
                        this.child(
                            Label::new(row.comparator.symbol())
                                .text_xs()
                                .text_color(muted)
                                .font_family(get_mono_font_family()),
                        )
                        .child(
                            div()
                                .w(px(80.))
                                .h(px(28.))
                                .overflow_hidden()
                                .child(Input::new(&row.threshold).xsmall().disabled(row_disabled)),
                        )
                        .when(!unit.is_empty(), |this| {
                            this.child(Label::new(unit).text_xs().text_color(muted))
                        })
                    })
                    .when(is_link_offline, |this| {
                        this.child(Label::new("--").text_xs().text_color(muted))
                    }),
            )
            .child(
                h_flex()
                    .w(px(110.))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(72.))
                            .h(px(28.))
                            .overflow_hidden()
                            .child(Input::new(&row.sustained_secs).xsmall().disabled(row_disabled)),
                    )
                    .child(Label::new("s").text_xs().text_color(muted)),
            )
            .child(
                h_flex()
                    .w(px(110.))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(72.))
                            .h(px(28.))
                            .overflow_hidden()
                            .child(Input::new(&row.cooldown_secs).xsmall().disabled(row_disabled)),
                    )
                    .child(Label::new("s").text_xs().text_color(muted)),
            )
    }

    fn render_recent_alerts(&self, recent: &[TriggeredAlert], cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().yellow;
        let border = cx.theme().border;
        let metric_labels: Vec<(AlertMetric, SharedString)> = recent
            .iter()
            .rev()
            .take(20)
            .map(|alert| (alert.metric, alert_metric_label(cx, alert.metric)))
            .collect();

        v_flex()
            .w_full()
            .gap_1p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        Label::new(i18n_metrics(cx, "alerts_recent_title"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .when(!recent.is_empty(), |this| {
                        this.child(
                            Button::new(ALERTS_CLEAR_HISTORY_BTN_ID)
                                .ghost()
                                .xsmall()
                                .label(i18n_metrics(cx, "alerts_clear_history"))
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.server_state.update(cx, |state, cx| {
                                        state.clear_recent_alerts(cx);
                                    });
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(if recent.is_empty() {
                div()
                    .w_full()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_metrics(cx, "alerts_recent_empty"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .id("metrics-alerts-recent-scroll")
                    .w_full()
                    .h(px(108.))
                    .overflow_y_scrollbar()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(border)
                    .p_2()
                    .gap_1()
                    .children(
                        recent
                            .iter()
                            .rev()
                            .take(20)
                            .zip(metric_labels)
                            .map(|(alert, (_, label))| {
                                let time_str = format_unix_millis_with(alert.triggered_at_ms, "%H:%M:%S")
                                    .unwrap_or_else(|| "--:--:--".to_string());
                                let line = if alert.metric == AlertMetric::LinkOffline {
                                    format!("[{time_str}] {label}")
                                } else {
                                    let unit = alert.metric.unit();
                                    format!(
                                        "[{time_str}] {label}: {:.1}{unit} ({} {:.1}{unit})",
                                        alert.observed,
                                        alert.comparator.symbol(),
                                        alert.threshold
                                    )
                                };
                                Label::new(SharedString::from(line))
                                    .text_xs()
                                    .text_color(warning)
                                    .font_family(get_mono_font_family())
                            }),
                    )
                    .into_any_element()
            })
    }
}

impl Render for ZedisServerAlertsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let recent: Vec<TriggeredAlert> = self.server_state.read(cx).recent_alerts().iter().cloned().collect();

        let rows_view: Vec<_> = (0..self.rows.len())
            .map(|ix| {
                let row = &self.rows[ix];
                self.render_rule_row(ix, row, cx).into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(i18n_metrics(cx, "alerts_enable_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .child(
                        Checkbox::new(ALERTS_MASTER_CHECKBOX_ID)
                            .checked(self.enabled)
                            .label(i18n_metrics(cx, "alerts_enable_label"))
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.enabled = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(ALERTS_RESET_BTN_ID)
                            .ghost()
                            .xsmall()
                            .label(i18n_metrics(cx, "alerts_reset_defaults"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_defaults(window, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .border_b_1()
                            .border_color(border)
                            .bg(cx.theme().muted)
                            .child(
                                div().w(px(230.)).child(
                                    Label::new(i18n_metrics(cx, "alerts_col_rule"))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                            )
                            .child(
                                div().w(px(140.)).child(
                                    Label::new(i18n_metrics(cx, "alerts_col_threshold"))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                            )
                            .child(
                                div().w(px(110.)).child(
                                    Label::new(i18n_metrics(cx, "alerts_col_sustained"))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                            )
                            .child(
                                div().w(px(110.)).child(
                                    Label::new(i18n_metrics(cx, "alerts_col_cooldown"))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("metrics-alerts-rules-scroll")
                            .w_full()
                            .h(px(260.))
                            .overflow_y_scrollbar()
                            .children(rows_view),
                    ),
            )
            .child(self.render_recent_alerts(&recent, cx))
    }
}

pub fn open_server_alerts_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let server_id = server_state.read(cx).server_id().to_string();
    if server_id.is_empty() {
        return;
    }
    let view = cx.new(|cx| ZedisServerAlertsDialog::new(server_state.clone(), window, cx));
    let view_child = view.clone();
    let view_ok = view;
    let save_label = i18n_common(cx, "save");
    let cancel_label = i18n_common(cx, "cancel");

    ZedisDialog::new(i18n_metrics(cx, "alerts_dialog_title"))
        .w(px(680.))
        .ok_text(save_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(save_label).cancel_text(cancel_label))
        .child(move || view_child.clone())
        .on_ok(move |_, _window, cx| {
            let (enabled, rules) = view_ok.read(cx).collect_config(cx);
            let server_id = view_ok.read(cx).server_id.clone();
            let _ = get_alert_rules_manager().save_config(&server_id, enabled, rules);
            let saved_msg = i18n_metrics(cx, "alerts_saved");
            server_state.update(cx, |state, cx| {
                state.notify_alert_config_changed(enabled, cx);
                state.emit_info_notification(saved_msg, cx);
            });
            true
        })
        .open(window, cx);
}
