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

//! Hot-key tracking (`HOTKEYS`, Redis 8.6) — "which keys are burning the
//! server".
//!
//! Start a collection (CPU time and/or network bytes, top-K), watch the two
//! ranked lists fill live on a poll, stop, read, reset. On a cluster the
//! tracking runs on every master and the lists merge (slots are disjoint).
//! Start/stop/reset are gated by [`Capability::HotkeysControl`]; the report
//! itself is a read.

use crate::assets::CustomIconName;
use crate::connection::{
    Capability, HotkeyEntry, HotkeysReport, ServerDb, hotkeys_report, hotkeys_reset, hotkeys_start, hotkeys_stop,
};
use crate::error::Error;
use crate::helpers::{get_mono_font_family, now_datetime, pacing};
use crate::states::{
    ReportFinding, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, i18n_common, i18n_hotkeys,
    i18n_monitor, i18n_slowlog_editor, i18n_troubleshoot,
};
use crate::views::panel_poll::{PanelPoll, summary_chip};
use crate::views::unavailable_chip;
use gpui::{App, ClipboardItem, Entity, ScrollHandle, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    v_flex,
};
use std::time::Duration;
use zedis_core::report::{Block, Section};
use zedis_core::string::format_duration;

type Result<T, E = Error> = std::result::Result<T, E>;

/// `HOTKEYS GET` is a tiny reply; poll fast enough that the duration and
/// the filling top lists feel live while a collection runs.
const POLL_SECS: u64 = pacing::HOTKEYS_POLL_SECS;
const NUM_COL: f32 = 96.0;
const PCT_COL: f32 = 64.0;
const BAR_COL: f32 = 80.0;
const TOP_K_CHOICES: [u64; 3] = [10, 20, 50];

pub struct ZedisHotkeys {
    server_state: Entity<ZedisServerState>,
    /// `None` until the first poll answers.
    report: Option<HotkeysReport>,
    error: Option<SharedString>,
    track_cpu: bool,
    track_net: bool,
    top_k: u64,
    /// A start/stop/reset round-trip is in flight — controls stay inert.
    busy: bool,
    poll: Option<PanelPoll>,
    pending_notification: Option<Notification>,
    scroll: ScrollHandle,
}

impl ZedisHotkeys {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            server_state,
            report: None,
            error: None,
            track_cpu: true,
            track_net: true,
            top_k: TOP_K_CHOICES[0],
            busy: false,
            poll: None,
            pending_notification: None,
            scroll: ScrollHandle::new(),
        };
        this.start_polling(cx);
        this
    }

    /// Sample now rather than at the end of the interval.
    fn refresh_now(&self) {
        if let Some(poll) = &self.poll {
            poll.refresh_now();
        }
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self.poll = Some(PanelPoll::start(
            cx,
            self.server_state.clone(),
            Duration::from_secs(POLL_SECS),
            fetch_report,
            |this, result, cx| {
                match result {
                    Ok(report) => {
                        this.report = Some(report);
                        this.error = None;
                        this.publish_report_finding(cx);
                    }
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            },
        ));
    }

    /// Leave the latest collection with the server state for a
    /// troubleshooting report (ADR 19): this page is dropped as soon as
    /// another one opens. An empty one leaves what an earlier one found.
    fn publish_report_finding(&self, cx: &mut Context<Self>) {
        let Some(report) = self.report.as_ref().filter(|report| !report.is_empty()) else {
            return;
        };
        let section = report_section(report, cx);
        self.server_state.update(cx, |state, _| {
            state.set_report_finding(ReportFinding::Hotkeys, section);
        });
    }

    fn tracking_active(&self) -> bool {
        self.report.as_ref().is_some_and(|r| r.tracking_active)
    }

    /// Run one control subcommand off the UI thread, toast the outcome, and
    /// force the next poll so the status flips without waiting a beat.
    fn run_control(&mut self, action: ControlAction, cx: &mut Context<Self>) {
        if self.busy || !self.server_state.read(cx).can(Capability::HotkeysControl) {
            return;
        }
        self.busy = true;
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        let (cpu, net, top_k) = (self.track_cpu, self.track_net, self.top_k);
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_handle, cx| {
            let task = cx.background_spawn(async move {
                let at = ServerDb::new(&*server_id, db);
                match action {
                    ControlAction::Start => hotkeys_start(&at, cpu, net, top_k).await,
                    ControlAction::Stop => hotkeys_stop(&at).await,
                    ControlAction::Reset => hotkeys_reset(&at).await,
                }
            });
            let result = task.await;
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        let key = match action {
                            ControlAction::Start => "started_ok",
                            ControlAction::Stop => "stopped_ok",
                            ControlAction::Reset => "reset_ok",
                        };
                        this.pending_notification = Some(Notification::success(i18n_hotkeys(cx, key)));
                        if action == ControlAction::Reset {
                            this.report = Some(HotkeysReport::default());
                        }
                        this.refresh_now();
                    }
                    Err(e) => {
                        this.pending_notification = Some(Notification::error(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
        self.pending_notification = Some(Notification::info(i18n_hotkeys(cx, "key_copied")));
        cx.notify();
    }

    fn render_summary(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(report) = &self.report else {
            return div().into_any_element();
        };
        let (status_key, status_color) = if report.tracking_active {
            ("status_tracking", theme.success)
        } else if report.is_empty() {
            ("status_idle", muted)
        } else {
            ("status_stopped", theme.foreground)
        };
        let (duration, cpu_total, net_total) = summary_values(report);
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_4()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(summary_chip(
                i18n_hotkeys(cx, "sum_status"),
                i18n_hotkeys(cx, status_key),
                status_color,
                muted,
            ))
            .child(summary_chip(
                i18n_hotkeys(cx, "sum_duration"),
                duration,
                theme.foreground,
                muted,
            ))
            .child(summary_chip(
                i18n_hotkeys(cx, "sum_cpu_total"),
                cpu_total,
                theme.foreground,
                muted,
            ))
            .child(summary_chip(
                i18n_hotkeys(cx, "sum_net_total"),
                net_total,
                theme.foreground,
                muted,
            ))
            .when(report.sample_ratio > 1, |this| {
                this.child(summary_chip(
                    i18n_hotkeys(cx, "sum_sample"),
                    format!("1/{}", report.sample_ratio),
                    theme.foreground,
                    muted,
                ))
            })
            .into_any_element()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let can_control = self.server_state.read(cx).can(Capability::HotkeysControl);
        let tracking = self.tracking_active();
        let has_report = self.report.as_ref().is_some_and(|r| !r.is_empty());
        let no_metric = !self.track_cpu && !self.track_net;

        let mut bar = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .flex_wrap()
            .border_b_1()
            .border_color(theme.border)
            // Metric toggles + top-K apply at START time, so they freeze
            // while a collection runs.
            .child(
                Button::new("hk-metric-cpu")
                    .xsmall()
                    .when(self.track_cpu, |b| b.primary())
                    .when(!self.track_cpu, |b| b.outline())
                    .label("CPU")
                    .tooltip(i18n_hotkeys(cx, "track_cpu_tooltip"))
                    .disabled(tracking)
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.track_cpu = !this.track_cpu;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("hk-metric-net")
                    .xsmall()
                    .when(self.track_net, |b| b.primary())
                    .when(!self.track_net, |b| b.outline())
                    .label("NET")
                    .tooltip(i18n_hotkeys(cx, "track_net_tooltip"))
                    .disabled(tracking)
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.track_net = !this.track_net;
                        cx.notify();
                    })),
            );
        for k in TOP_K_CHOICES {
            bar = bar.child(
                Button::new(SharedString::from(format!("hk-top-{k}")))
                    .xsmall()
                    .when(self.top_k == k, |b| b.primary())
                    .when(self.top_k != k, |b| b.outline())
                    .label(format!("Top {k}"))
                    .disabled(tracking)
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.top_k = k;
                        cx.notify();
                    })),
            );
        }
        bar = bar.child(div().flex_1());
        if can_control {
            bar = bar
                .child(
                    Button::new("hk-start")
                        .primary()
                        .small()
                        .label(i18n_hotkeys(cx, "start"))
                        .tooltip(i18n_hotkeys(
                            cx,
                            if no_metric { "pick_metric_hint" } else { "start_tooltip" },
                        ))
                        .disabled(self.busy || tracking || no_metric)
                        .on_click(cx.listener(|this, _, _w, cx| this.run_control(ControlAction::Start, cx))),
                )
                .child(
                    Button::new("hk-stop")
                        .outline()
                        .small()
                        .label(i18n_hotkeys(cx, "stop"))
                        .tooltip(i18n_hotkeys(cx, "stop_tooltip"))
                        .disabled(self.busy || !tracking)
                        .on_click(cx.listener(|this, _, _w, cx| this.run_control(ControlAction::Stop, cx))),
                )
                .child(
                    Button::new("hk-reset")
                        .ghost()
                        .small()
                        .label(i18n_hotkeys(cx, "reset"))
                        .tooltip(i18n_hotkeys(cx, "reset_tooltip"))
                        .disabled(self.busy || tracking || !has_report)
                        .on_click(cx.listener(|this, _, _w, cx| this.run_control(ControlAction::Reset, cx))),
                );
        }
        bar = bar
            .when_some(
                self.server_state.read(cx).blocked_by(Capability::HotkeysControl),
                |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
            )
            .child(
                Button::new("hk-refresh")
                    .outline()
                    .small()
                    .icon(Icon::new(CustomIconName::RotateCw))
                    .tooltip(i18n_hotkeys(cx, "refresh_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.refresh_now();
                        cx.notify();
                    })),
            );
        bar.into_any_element()
    }

    /// One ranked list ("by CPU" / "by NET"): rank, key (click copies),
    /// share bar, value, share %.
    fn render_top_list(
        &self,
        id: &'static str,
        title_key: &'static str,
        entries: &[HotkeyEntry],
        total: u64,
        format_value: fn(u64) -> String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let warning = theme.warning;
        let stripe_bg = theme.table_even;

        let mut list = v_flex()
            .flex_1()
            .min_w_0()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Label::new(i18n_hotkeys(cx, title_key)).text_sm().font_semibold()),
            );
        if entries.is_empty() {
            return list
                .child(
                    div()
                        .p_3()
                        .child(Label::new(i18n_hotkeys(cx, "list_empty")).text_xs().text_color(muted)),
                )
                .into_any_element();
        }
        for (ix, entry) in entries.iter().enumerate() {
            let share = share_of(entry, total);
            let key: SharedString = entry.key.clone().into();
            let copy_key = key.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-row-{ix}")))
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .when(ix % 2 != 0, |this| this.bg(stripe_bg))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _w, cx| this.copy_key(copy_key.clone(), cx)))
                    .child(
                        div()
                            .w(px(28.))
                            .child(Label::new(format!("{}", ix + 1)).text_xs().text_color(muted)),
                    )
                    .child(div().flex_1().min_w_0().child(Label::new(key).text_xs().truncate()))
                    .child(
                        div()
                            .w(px(BAR_COL))
                            .h(px(6.))
                            .rounded_full()
                            .bg(muted.opacity(0.15))
                            .child(
                                div()
                                    .w(px((BAR_COL * share as f32).clamp(2.0, BAR_COL)))
                                    .h_full()
                                    .rounded_full()
                                    .bg(warning),
                            ),
                    )
                    .child(
                        h_flex()
                            .w(px(NUM_COL))
                            .justify_end()
                            .child(Label::new(format_value(entry.value)).text_xs().font_semibold()),
                    )
                    .child(
                        h_flex()
                            .w(px(PCT_COL))
                            .justify_end()
                            .child(Label::new(format!("{:.1}%", share * 100.0)).text_xs().text_color(muted)),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        if let Some(err) = self.error.clone() {
            return div()
                .p_4()
                .child(Label::new(err).text_sm().text_color(theme.danger))
                .into_any_element();
        }
        let Some(report) = &self.report else {
            return div()
                .p_4()
                .child(Label::new(i18n_hotkeys(cx, "loading")).text_sm().text_color(muted))
                .into_any_element();
        };
        if report.is_empty() && !report.tracking_active {
            return div()
                .p_4()
                .child(Label::new(i18n_hotkeys(cx, "idle_hint")).text_sm().text_color(muted))
                .into_any_element();
        }
        let mut lists = h_flex().w_full().items_start().gap_3().p_3();
        // A metric absent from the collection has an empty list *and* a zero
        // total — hide its card entirely instead of showing "no data" noise
        // (unless both are absent, then both empty cards explain themselves).
        let show_cpu = !report.by_cpu.is_empty() || report.by_net.is_empty();
        let show_net = !report.by_net.is_empty() || report.by_cpu.is_empty();
        if show_cpu {
            lists = lists.child(self.render_top_list(
                "hk-cpu",
                "by_cpu_title",
                &report.by_cpu,
                report.total_cpu_us,
                format_cpu,
                cx,
            ));
        }
        if show_net {
            lists = lists.child(self.render_top_list(
                "hk-net",
                "by_net_title",
                &report.by_net,
                report.total_net_bytes,
                format_net,
                cx,
            ));
        }
        lists.into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ControlAction {
    Start,
    Stop,
    Reset,
}

impl Render for ZedisHotkeys {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        let body = self.render_body(cx);
        let summary = self.render_summary(cx);
        let toolbar = self.render_toolbar(cx);

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("hotkeys-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Label::new(i18n_hotkeys(cx, "title")).font_semibold())
                    .child(div().flex_1())
                    .map(|this| {
                        let state = self.server_state.read(cx);
                        let supports_monitor = state.supports_view(ServerView::Monitor);
                        let supports_slowlog = state.supports_view(ServerView::Slowlog);
                        if !supports_monitor && !supports_slowlog {
                            return this;
                        }
                        this.child(
                            Label::new(i18n_common(cx, "see_also"))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .when(supports_monitor, |this| {
                            this.child(
                                Button::new("hotkeys-goto-monitor")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(CustomIconName::Activity))
                                    .label(i18n_monitor(cx, "title"))
                                    .on_click(|_, _w, cx| {
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| {
                                                state.go_to_view(ServerView::Monitor, cx);
                                            });
                                        });
                                    }),
                            )
                        })
                        .when(supports_slowlog, |this| {
                            this.child(
                                Button::new("hotkeys-goto-slowlog")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(CustomIconName::Snail))
                                    .label(i18n_slowlog_editor(cx, "panel_title"))
                                    .on_click(|_, _w, cx| {
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| {
                                                state.go_to_view(ServerView::Slowlog, cx);
                                            });
                                        });
                                    }),
                            )
                        })
                    })
                    .child(
                        Label::new(i18n_hotkeys(cx, "help_hint"))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(summary)
            .child(toolbar)
            .child(
                div()
                    .id("hotkeys-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }
}

fn format_cpu(us: u64) -> String {
    format!("{us} µs")
}

fn format_net(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::DECIMAL)
}

fn share_of(entry: &HotkeyEntry, total: u64) -> f64 {
    if total > 0 {
        entry.value as f64 / total as f64
    } else {
        0.0
    }
}

/// The collection's length and totals as the summary shows them.
fn summary_values(report: &HotkeysReport) -> (String, String, String) {
    let duration = if report.collection_duration_ms > 0 {
        format_duration(Duration::from_millis(report.collection_duration_ms))
    } else {
        "—".to_string()
    };
    let cpu_total = if report.total_cpu_us > 0 {
        format!("{:.1} ms", report.total_cpu_us as f64 / 1000.0)
    } else {
        "—".to_string()
    };
    let net_total = if report.total_net_bytes > 0 {
        format_net(report.total_net_bytes)
    } else {
        "—".to_string()
    };
    (duration, cpu_total, net_total)
}

/// A collection as a troubleshooting report's section: the summary, then
/// each ranked list the collection has.
fn report_section(report: &HotkeysReport, cx: &App) -> Section {
    let k = |key: &str| i18n_hotkeys(cx, key).to_string();
    let (duration, cpu_total, net_total) = summary_values(report);
    let status = if report.tracking_active {
        "status_tracking"
    } else {
        "status_stopped"
    };
    let mut facts = vec![
        (i18n_troubleshoot(cx, "fact_recorded").to_string(), now_datetime()),
        (k("sum_status"), k(status)),
        (k("sum_duration"), duration),
        (k("sum_cpu_total"), cpu_total),
        (k("sum_net_total"), net_total),
    ];
    if report.sample_ratio > 1 {
        facts.push((k("sum_sample"), format!("1/{}", report.sample_ratio)));
    }
    let mut blocks = vec![Block::Facts(facts)];
    blocks.extend(ranked_list(
        k("by_cpu_title"),
        &report.by_cpu,
        report.total_cpu_us,
        format_cpu,
        cx,
    ));
    blocks.extend(ranked_list(
        k("by_net_title"),
        &report.by_net,
        report.total_net_bytes,
        format_net,
        cx,
    ));
    Section {
        title: k("title"),
        blocks,
    }
}

/// One ranked list as the report shows it, or nothing for a metric the
/// collection did not track.
fn ranked_list(
    title: String,
    entries: &[HotkeyEntry],
    total: u64,
    format_value: fn(u64) -> String,
    cx: &App,
) -> Vec<Block> {
    if entries.is_empty() {
        return Vec::new();
    }
    let columns = ["col_rank", "col_key", "col_value", "col_share"]
        .iter()
        .map(|key| i18n_troubleshoot(cx, key).to_string())
        .collect();
    let rows = entries
        .iter()
        .enumerate()
        .map(|(ix, entry)| {
            vec![
                (ix + 1).to_string(),
                entry.key.clone(),
                format_value(entry.value),
                format!("{:.1}%", share_of(entry, total) * 100.0),
            ]
        })
        .collect();
    vec![Block::Heading(title), Block::Table { columns, rows }]
}

async fn fetch_report(server_id: String, db: usize) -> Result<HotkeysReport> {
    Ok(hotkeys_report(&ServerDb::new(server_id, db)).await?)
}
