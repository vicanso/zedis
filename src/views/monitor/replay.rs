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

use super::{RAW_LINE_CELL, ZedisMonitor};
use crate::assets::CustomIconName;
use crate::connection::{
    MonitorReplayReport, MonitorReplaySpeed, ServerDb, get_servers, partition_replay_commands, run_monitor_replay_paced,
};
use crate::helpers::group_thousands;
use crate::states::{ZedisGlobalStore, i18n_common, i18n_monitor};
use gpui::{AnyElement, Context, Render, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, label::Label, v_flex,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use zedis_core::benchmark::format_latency_us;
use zedis_ui::ZedisDialog;

#[derive(Clone, Debug)]
pub(super) struct MonitorReplayViewResult {
    pub(super) target_label: SharedString,
    pub(super) speed: MonitorReplaySpeed,
    pub(super) report: MonitorReplayReport,
}

pub(super) struct MonitorReplayDialog {
    pub(super) servers: Vec<(SharedString, SharedString)>,
    pub(super) selected_server_idx: usize,
    pub(super) speed: MonitorReplaySpeed,
    pub(super) read_only_count: usize,
    pub(super) skipped_write_count: usize,
}

impl MonitorReplayDialog {
    pub(super) fn new(current_server_id: &str, read_only_count: usize, skipped_write_count: usize) -> Self {
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.id.is_empty() && !s.is_high_risk_tag())
            .map(|s| {
                let label = if s.name.is_empty() {
                    format!("{}:{}", s.host, s.port)
                } else {
                    format!("{} ({}:{})", s.name, s.host, s.port)
                };
                (s.id.into(), label.into())
            })
            .collect();
        let selected_server_idx = servers
            .iter()
            .position(|(id, _)| id.as_ref() == current_server_id)
            .unwrap_or(0);
        Self {
            servers,
            selected_server_idx,
            speed: MonitorReplaySpeed::RealTime1x,
            read_only_count,
            skipped_write_count,
        }
    }

    pub(super) fn selected_target(&self) -> Option<(SharedString, SharedString)> {
        self.servers.get(self.selected_server_idx).cloned()
    }
}

impl Render for MonitorReplayDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let preview_text = rust_i18n::t!(
            "monitor.replay_preview_summary",
            reads = group_thousands(self.read_only_count as u64),
            skipped = group_thousands(self.skipped_write_count as u64),
            locale = locale
        )
        .to_string();

        let server_buttons: Vec<AnyElement> = self
            .servers
            .iter()
            .enumerate()
            .map(|(idx, (_, label))| {
                let selected = idx == self.selected_server_idx;
                Button::new(("monitor-replay-target", idx))
                    .outline()
                    .small()
                    .selected(selected)
                    .label(label.clone())
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.selected_server_idx = idx;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        let speed = self.speed;
        v_flex()
            .w_full()
            .gap_3()
            .text_sm()
            .child(Label::new(preview_text).font_semibold())
            .child(
                Label::new(i18n_monitor(cx, "replay_safety_note"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Label::new(i18n_monitor(cx, "replay_target_server"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .map(|this| {
                        if server_buttons.is_empty() {
                            this.child(
                                Label::new(i18n_monitor(cx, "replay_no_non_prod_servers"))
                                    .text_xs()
                                    .text_color(cx.theme().warning),
                            )
                        } else {
                            this.child(h_flex().w_full().gap_2().flex_wrap().children(server_buttons))
                        }
                    }),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(Label::new(i18n_monitor(cx, "replay_speed")).text_xs().text_color(muted))
                    .child(
                        ButtonGroup::new("monitor-replay-speed")
                            .compact()
                            .small()
                            .outline()
                            .child(
                                Button::new("monitor-replay-speed-1x")
                                    .label(MonitorReplaySpeed::RealTime1x.label())
                                    .selected(speed == MonitorReplaySpeed::RealTime1x),
                            )
                            .child(
                                Button::new("monitor-replay-speed-2x")
                                    .label(MonitorReplaySpeed::Fast2x.label())
                                    .selected(speed == MonitorReplaySpeed::Fast2x),
                            )
                            .child(
                                Button::new("monitor-replay-speed-max")
                                    .label(MonitorReplaySpeed::MaxSpeed.label())
                                    .selected(speed == MonitorReplaySpeed::MaxSpeed),
                            )
                            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                                if let Some(ix) = clicks.first() {
                                    this.speed = match *ix {
                                        0 => MonitorReplaySpeed::RealTime1x,
                                        1 => MonitorReplaySpeed::Fast2x,
                                        _ => MonitorReplaySpeed::MaxSpeed,
                                    };
                                    cx.notify();
                                }
                            })),
                    ),
            )
    }
}

impl ZedisMonitor {
    pub(super) fn captured_replay_lines(&self, cx: &Context<Self>) -> Vec<String> {
        self.visible_rows_snapshot(cx)
            .into_iter()
            .rev()
            .filter_map(|cells| {
                if let Some(raw) = cells.get(RAW_LINE_CELL).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    return Some(raw.to_string());
                }
                let cmd = cells.get(4)?.trim();
                if cmd.is_empty() {
                    return None;
                }
                let args = cells.get(5).map(|s| s.trim()).unwrap_or_default();
                if args.is_empty() {
                    Some(cmd.to_string())
                } else {
                    Some(format!("{cmd} {args}"))
                }
            })
            .collect()
    }

    pub(super) fn open_replay_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.monitoring || self.replay_running {
            return;
        }
        let lines = self.captured_replay_lines(cx);
        if lines.is_empty() {
            return;
        }
        let (read_only, skipped_write_count) = partition_replay_commands(&lines);
        let read_only_count = read_only.len();
        let current_server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        let dialog_entity =
            cx.new(|_| MonitorReplayDialog::new(&current_server_id, read_only_count, skipped_write_count));
        let dialog_child = dialog_entity.clone();
        let dialog_ok = dialog_entity.clone();
        let monitor_entity = cx.entity().downgrade();

        let button_props = DialogButtonProps::default()
            .cancel_text(i18n_common(cx, "cancel"))
            .ok_text(i18n_monitor(cx, "replay_start"));
        ZedisDialog::new(i18n_monitor(cx, "replay_title"))
            .w(px(520.))
            .button_props(button_props)
            .child(move || dialog_child.clone())
            .on_ok(move |_, window, cx| {
                let (target_id, target_label, speed, count) = {
                    let view = dialog_ok.read(cx);
                    let Some((id, label)) = view.selected_target() else {
                        return false;
                    };
                    (id, label, view.speed, view.read_only_count)
                };
                if count == 0 {
                    return false;
                }
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                let confirm_body = rust_i18n::t!(
                    "monitor.replay_confirm_body",
                    count = group_thousands(count as u64),
                    server = target_label.as_ref(),
                    speed = speed.label(),
                    locale = locale
                )
                .to_string();
                let confirm_props = DialogButtonProps::default()
                    .cancel_text(i18n_common(cx, "cancel"))
                    .ok_text(i18n_monitor(cx, "replay_start"));
                let monitor_entity = monitor_entity.clone();
                let lines = lines.clone();
                window.close_dialog(cx);
                ZedisDialog::new_alert(i18n_monitor(cx, "replay_confirm_title"), confirm_body)
                    .button_props(confirm_props)
                    .on_ok(move |_, window, cx| {
                        if let Some(monitor) = monitor_entity.upgrade() {
                            monitor.update(cx, |this, cx| {
                                this.start_replay(
                                    target_id.clone(),
                                    target_label.clone(),
                                    db,
                                    speed,
                                    lines.clone(),
                                    cx,
                                );
                            });
                        }
                        window.close_dialog(cx);
                        true
                    })
                    .open(window, cx);
                false
            })
            .open(window, cx);
    }

    fn start_replay(
        &mut self,
        target_id: SharedString,
        target_label: SharedString,
        db: usize,
        speed: MonitorReplaySpeed,
        lines: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.replay_running {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.replay_running = true;
        self.replay_cancel = Some(cancel.clone());
        self.replay_result = None;
        self.replay_error = None;
        cx.notify();

        let at = ServerDb::new(target_id.to_string(), db);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    run_monitor_replay_paced(&at, &lines, speed, cancel, move |delay| executor.timer(delay)).await
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.replay_running = false;
                this.replay_cancel = None;
                match outcome {
                    Ok(report) => {
                        this.replay_result = Some(MonitorReplayViewResult {
                            target_label,
                            speed,
                            report,
                        });
                    }
                    Err(e) => {
                        this.replay_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn stop_replay(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.replay_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.replay_running = false;
        cx.notify();
    }

    pub(super) fn render_replay_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.replay_running {
            return h_flex()
                .id("monitor-replay-banner")
                .w_full()
                .flex_none()
                .items_center()
                .justify_between()
                .px_4()
                .py_1p5()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(
                    Label::new(i18n_monitor(cx, "replay_running"))
                        .text_xs()
                        .text_color(cx.theme().foreground),
                )
                .child(
                    Button::new("monitor-replay-stop-banner")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(CustomIconName::X))
                        .label(i18n_monitor(cx, "stop"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.stop_replay(cx);
                        })),
                )
                .into_any_element();
        }

        if let Some(err) = &self.replay_error {
            return h_flex()
                .id("monitor-replay-banner")
                .w_full()
                .flex_none()
                .items_center()
                .justify_between()
                .px_4()
                .py_1p5()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(Label::new(err.clone()).text_xs().text_color(cx.theme().red))
                .child(
                    Button::new("monitor-replay-dismiss-err")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.replay_error = None;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }

        let Some(res) = &self.replay_result else {
            return div().into_any_element();
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let report_line = rust_i18n::t!(
            "monitor.replay_report_summary",
            server = res.target_label.as_ref(),
            speed = res.speed.label(),
            replayed = group_thousands(res.report.replayed_count as u64),
            skipped = group_thousands(res.report.skipped_write_count as u64),
            errors = group_thousands(res.report.error_count as u64),
            rps = format!("{:.1}", res.report.summary.ops_per_sec),
            p50 = format_latency_us(res.report.summary.p50_us),
            p95 = format_latency_us(res.report.summary.p95_us),
            p99 = format_latency_us(res.report.summary.p99_us),
            locale = locale
        )
        .to_string();

        h_flex()
            .id("monitor-replay-banner")
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .px_4()
            .py_1p5()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        Label::new(i18n_monitor(cx, "replay_report_title"))
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().foreground),
                    )
                    .child(Label::new(report_line).text_xs().text_color(muted)),
            )
            .child(
                Button::new("monitor-replay-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.replay_result = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
