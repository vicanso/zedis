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

//! The Latency tab: `LATENCY LATEST` polled while the tab shows, the event
//! list with its detail pane, and enabling / resetting latency tracking.
//!
//! Split out of `slowlog_editor.rs`; the methods are `ZedisSlowlogEditor`'s as before.

use super::*;

impl ZedisSlowlogEditor {
    /// Fetch `LATENCY LATEST` and the `latency-monitor-threshold`
    /// config in one task. Threshold tells the user why LATEST might
    /// be empty (tracking disabled).
    pub(super) fn fetch_latency(&mut self, cx: &mut gpui::Context<Self>) {
        if self.latency_loading {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        // The probe already knows LATENCY is missing / denied here: show the
        // unsupported state without a round trip (and without a NOPERM toast).
        if self
            .server_state
            .read(cx)
            .command_block(ServerCommand::LatencyLatest)
            .is_some()
        {
            self.latency_unsupported = true;
            self.latency_loading = false;
            cx.notify();
            return;
        }
        self.latency_loading = true;
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let listing = latency_latest(&mut conn).await?;
                let threshold = if listing.unsupported {
                    0
                } else {
                    // Best-effort — CONFIG GET may be ACL-restricted;
                    // we treat any failure as "unknown" = 0.
                    latency_monitor_threshold(&mut conn).await.unwrap_or(0)
                };
                Ok::<_, Error>((listing, threshold))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.latency_loading = false;
                match result {
                    Ok((listing, threshold)) => {
                        this.latency_unsupported = listing.unsupported;
                        this.latency_events = listing.events;
                        this.latency_threshold_ms = threshold;
                        // Drop stale caches so a refresh always reflects
                        // the freshly fetched events.
                        this.event_histories.clear();
                        this.expanded_event = None;
                        // Rebuild slow-log rows so their correlation
                        // chips reflect the freshly fetched events.
                        // We rebuild from `server_state.slow_logs()`
                        // rather than re-decorating `all_rows` so that
                        // a slowlog refresh between latency fetches
                        // doesn't leave us with stale rows.
                        this.rebuild_rows_with_correlations(cx);
                    }
                    Err(_) => {
                        // Errors fall through silently — they'll be
                        // logged by the spawn helper. UI shows previous
                        // data rather than blanking.
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Switch to the Latency tab and pin the named event as expanded so
    /// the user lands on its detail block. Called when the user clicks
    /// the correlation chip on a slow-log row.
    pub(super) fn jump_to_latency_event(&mut self, event: SharedString, cx: &mut gpui::Context<Self>) {
        self.set_tab(PerformanceTab::Latency, cx);
        // Different event from currently expanded one (or none) — set
        // it and fetch detail. Same event already expanded → leave it
        // alone (no toggle off, which the generic expand_event does).
        if self.expanded_event.as_ref() != Some(&event) {
            self.expanded_event = Some(event.clone());
            self.fetch_event_detail(event, cx);
        }
        // If we never populated LATEST yet (user came here via a chip
        // before opening the Latency tab manually) — kick a fetch.
        if self.latency_events.is_empty() && !self.latency_loading {
            self.fetch_latency(cx);
        }
        cx.notify();
    }

    /// Kick a background loop that re-fetches LATENCY every 5 seconds
    /// while the user is on the Latency tab. The Task handle lives in
    /// `_latency_poll_task` — dropping it (via `stop_latency_polling`
    /// or view teardown) cancels the loop.
    pub(super) fn start_latency_polling(&mut self, cx: &mut gpui::Context<Self>) {
        // Already running — don't stack a second loop on top.
        if self._latency_poll_task.is_some() {
            return;
        }
        // Kick an immediate fetch so the panel doesn't sit empty for
        // the first 5 seconds on entry.
        if self.latency_events.is_empty() && !self.latency_loading {
            self.fetch_latency(cx);
        }
        self._latency_poll_task = Some(cx.spawn(async move |handle, cx| {
            loop {
                cx.background_executor().timer(pacing::LATENCY_POLL_INTERVAL).await;
                let still_active = handle
                    .update(cx, |this, cx| {
                        if this.current_tab == PerformanceTab::Latency {
                            this.fetch_latency(cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !still_active {
                    break;
                }
            }
        }));
    }

    pub(super) fn stop_latency_polling(&mut self) {
        self._latency_poll_task = None;
    }

    /// Issue `CONFIG SET latency-monitor-threshold 100` to flip on
    /// latency tracking with a sensible default (100ms — quiet enough
    /// to skip routine commands, loud enough to catch fork/AOF stalls).
    /// PROD-tagged servers route through the standard confirm dialog so
    /// nobody flips runtime config on a live cluster by accident.
    pub(super) fn enable_latency_tracking(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        if high_risk {
            self.open_enable_confirm(window, cx);
        } else {
            self.run_enable_latency_tracking(cx);
        }
    }

    pub(super) fn run_enable_latency_tracking(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                // 100ms — the same default Redis docs suggest. Users
                // can dial it down via CLI/Config panel later.
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("latency-monitor-threshold")
                    .arg("100")
                    .query_async::<()>(&mut conn)
                    .await?;
                Ok::<_, Error>(())
            });
            let _ = task.await;
            let _ = handle.update(cx, |this, cx| {
                // Immediate refetch so the threshold banner flips from
                // "disabled" to "100 ms" without waiting for the next
                // auto-poll tick.
                this.fetch_latency(cx);
            });
        }));
    }

    /// `LATENCY RESET` without args — wipes every event. After reset
    /// we re-fetch so the UI reflects the now-empty state.
    pub(super) fn reset_latency(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                latency_reset(&mut conn, &[]).await
            });
            let _ = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.latency_events.clear();
                this.event_histories.clear();
                this.expanded_event = None;
                this.fetch_latency(cx);
            });
        }));
    }

    /// The Latency tab body. Shows threshold context, the LATEST
    /// event table, and inline drill-down (LATENCY GRAPH + recent
    /// HISTORY samples) for the expanded row.
    pub(super) fn render_latency_body(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let theme_yellow = cx.theme().yellow;

        // Unsupported / disabled / loading empty states first.
        if self.latency_unsupported {
            return centered_message(i18n_slowlog_editor(cx, "latency_unsupported"), muted).into_any_element();
        }
        if self.latency_loading && self.latency_events.is_empty() {
            return centered_message(i18n_common(cx, "loading"), muted).into_any_element();
        }

        // Banner: explains threshold state. Yellow when 0 (disabled),
        // muted otherwise.
        let threshold_label: SharedString = if self.latency_threshold_ms == 0 {
            i18n_slowlog_editor(cx, "latency_threshold_disabled")
        } else {
            SharedString::from(format!(
                "{}: {} ms",
                i18n_slowlog_editor(cx, "latency_threshold_label"),
                self.latency_threshold_ms
            ))
        };
        let banner_color = if self.latency_threshold_ms == 0 {
            theme_yellow
        } else {
            muted
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.latency_events.len());
        for ev in &self.latency_events {
            rows.push(self.render_latency_row(ev.clone(), cx).into_any_element());
        }

        let event_label = i18n_slowlog_editor(cx, "latency_event");
        let latest_label = i18n_slowlog_editor(cx, "latency_latest_ms");
        let max_label = i18n_slowlog_editor(cx, "latency_max_ms");
        let when_label = i18n_slowlog_editor(cx, "latency_when");

        // Column header. Widths chosen to roughly match data column
        // contents below; not a real DataTable to keep the inline
        // drill-down cheap to render.
        let header = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(220.0))
                    .child(Label::new(event_label).text_xs().text_color(muted)),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(latest_label).text_xs().text_color(muted)),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(max_label).text_xs().text_color(muted)),
            )
            .child(div().flex_1().child(Label::new(when_label).text_xs().text_color(muted)));

        // Tracking-disabled banner gets an inline "Enable tracking"
        // button so users can flip the toggle without bouncing to the
        // Config panel. Hidden when tracking is already on or the
        // server pre-dates LATENCY altogether.
        let show_enable_button = self.latency_threshold_ms == 0 && !self.latency_unsupported;
        // The server's own reading of the events below, where it has the
        // command.
        let show_doctor = !self.latency_unsupported
            && self
                .server_state
                .read(cx)
                .command_block(ServerCommand::LatencyDoctor)
                .is_none();

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .child(Label::new(threshold_label).text_xs().text_color(banner_color).flex_1())
                    .when(show_enable_button, |this| {
                        this.child(
                            Button::new("latency-enable-tracking")
                                .primary()
                                .xsmall()
                                .label(i18n_slowlog_editor(cx, "enable_tracking_button"))
                                .on_click(cx.listener(|this, _, w, cx| this.enable_latency_tracking(w, cx))),
                        )
                    })
                    .when(show_doctor, |this| {
                        this.child(
                            Button::new("latency-doctor")
                                .outline()
                                .xsmall()
                                .label(i18n_slowlog_editor(cx, "latency_doctor"))
                                .tooltip(i18n_slowlog_editor(cx, "latency_doctor_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    open_server_report_dialog(
                                        this.server_state.clone(),
                                        ServerReport::Latency,
                                        window,
                                        cx,
                                    )
                                })),
                        )
                    }),
            )
            .child(header)
            .when(self.latency_events.is_empty(), |this| {
                this.child(centered_message(i18n_slowlog_editor(cx, "latency_no_events"), muted))
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().children(rows)),
            )
            .text_color(foreground)
            .into_any_element()
    }

    pub(super) fn render_latency_row(&self, ev: LatencyEvent, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let event = ev.event.clone();
        let event_for_toggle = event.clone();
        let event_for_jump = event.clone();
        let event_ts = ev.timestamp;
        let is_expanded = self.expanded_event.as_deref() == Some(event.as_str());
        let when_str = format_unix_seconds(ev.timestamp);
        let id_hash: u32 = djb2_hash(event.as_ref());

        // Cross-tab chip: count of slow-log rows that fired within the
        // correlation window around this event. When > 0, give the user
        // a one-click "jump back to SlowLog filtered to that window".
        let slow_count = correlated_slowlog_count_for_event(ev.timestamp, &self.all_rows);
        let jump_chip: Option<gpui::AnyElement> = if slow_count > 0 {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            let label: SharedString = rust_i18n::t!(
                "slowlog_editor.chip_slow_nearby",
                count = slow_count.to_string(),
                locale = locale
            )
            .to_string()
            .into();
            Some(
                Button::new(("latency-jump-slow", id_hash))
                    .outline()
                    .xsmall()
                    .label(label)
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.jump_to_slowlog_window(event_for_jump.clone().into(), event_ts, cx);
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };

        // Color the latest/max numbers — yellow at > 100ms, red at > 1s.
        let latest_color = severity_color(ev.latest_ms, cx);
        let max_color = severity_color(ev.max_ms, cx);

        let history = self.event_histories.get(event.as_str()).cloned();

        let row = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().w(px(220.0)).child(Label::new(event.clone()).text_sm()))
            .child(
                div().w(px(110.0)).child(
                    Label::new(format!("{} ms", ev.latest_ms))
                        .text_sm()
                        .text_color(latest_color),
                ),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(format!("{} ms", ev.max_ms)).text_sm().text_color(max_color)),
            )
            .child(div().flex_1().child(Label::new(when_str).text_xs().text_color(muted)))
            .when_some(jump_chip, |this, chip| this.child(chip))
            .child(
                Button::new(("latency-toggle", id_hash))
                    .ghost()
                    .xsmall()
                    .label(if is_expanded {
                        i18n_slowlog_editor(cx, "latency_hide_graph")
                    } else {
                        i18n_slowlog_editor(cx, "latency_show_graph")
                    })
                    .on_click(
                        cx.listener(move |this, _, _w, cx| this.expand_event(event_for_toggle.clone().into(), cx)),
                    ),
            );

        // Inline drill-down block: GPU sparkline (from HISTORY) +
        // tail of HISTORY samples.
        let detail: Option<gpui::AnyElement> = if is_expanded {
            Some(self.render_latency_detail(history, cx).into_any_element())
        } else {
            None
        };

        v_flex().child(row).when_some(detail, |this, d| this.child(d))
    }

    pub(super) fn render_latency_detail(
        &self,
        history: Option<Vec<LatencySample>>,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        // Sparkline block: native GPU line chart sourced from LATENCY
        // HISTORY. Replaces the previous monospace ASCII `LATENCY
        // GRAPH` block — server-side ASCII art doesn't scale with the
        // panel and clashes visually with the Metrics view's canvases.
        let graph_block: gpui::AnyElement = match history.as_deref() {
            // No samples yet → render the loading placeholder so the
            // expanded row has something while the background fetch is
            // in flight.
            None => div()
                .px_3()
                .py_2()
                .child(Label::new(i18n_common(cx, "loading")).text_xs().text_color(muted))
                .into_any_element(),
            // Empty Vec — render the explicit "no samples yet"
            // affordance instead of an empty chart frame.
            Some([]) => div()
                .px_3()
                .py_2()
                .child(
                    Label::new(i18n_slowlog_editor(cx, "sparkline_no_history"))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element(),
            Some(h) => {
                // LATENCY HISTORY timestamps are unix-seconds; the
                // chart helper formats unix-millis, so multiply by 1000
                // to reuse it without forking a second formatter.
                let dates: Vec<SharedString> = h.iter().map(|s| format_timestamp_ms(s.timestamp * 1000)).collect();
                let values: Vec<f64> = h.iter().map(|s| s.latency_ms as f64).collect();
                // Floor y_max at 0.01 — `make_line_canvas` divides by
                // y_max for scale, so 0 would produce NaN.
                let y_max = values.iter().copied().fold(0.01_f64, f64::max);
                // Roughly 4 x-axis labels evenly spaced; clamp at 1 so
                // a single-sample history still renders a tick.
                let tick_margin = (h.len() / 4).max(1);
                let params = ChartParams {
                    dates: Arc::new(dates),
                    y_max,
                    y_format: Box::new(|v| format!("{:.0} ms", v)),
                    tick_margin,
                    border: theme.border,
                    muted_fg: muted,
                };
                div()
                    .h(px(140.))
                    .px_3()
                    .py_2()
                    .child(make_line_canvas(params, Arc::new(values), theme.chart_2, false))
                    .into_any_element()
            }
        };

        // Tail of raw history samples for users who want exact numbers.
        // Limit so a 160-point history doesn't drown the panel — the
        // sparkline above carries the shape.
        const HISTORY_PREVIEW: usize = 12;
        let samples: Vec<gpui::AnyElement> = history
            .map(|h| {
                h.iter()
                    .rev()
                    .take(HISTORY_PREVIEW)
                    .map(|s| {
                        h_flex()
                            .gap_2()
                            .child(Label::new(format_unix_seconds(s.timestamp)).text_xs().text_color(muted))
                            .child(
                                Label::new(format!("{} ms", s.latency_ms))
                                    .text_xs()
                                    .text_color(severity_color(s.latency_ms, cx)),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();

        v_flex()
            .gap_2()
            .px_4()
            .py_2()
            .bg(cx.theme().muted.opacity(0.15))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(graph_block)
            .when(!samples.is_empty(), |this| {
                this.child(
                    Label::new(i18n_slowlog_editor(cx, "latency_history_label"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(v_flex().gap_1().children(samples))
            })
    }
}
