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

//! Render half of the memory analysis view: toolbar, AI panel,
//! recommendations, fragmentation chart, TTL histogram, table
//! sections and the root `Render` impl. Split out of
//! `memory_analysis.rs` to keep the scan/analysis half readable.

use super::*;

impl ZedisMemoryAnalysis {
    pub(super) fn render_toolbar_functions(&self, cx: &mut gpui::Context<Self>) -> ZedisDivider {
        let is_running = self.status == AnalysisStatus::Running;
        let is_idle = self.status == AnalysisStatus::Idle;
        let stat_item = |cx: &mut gpui::Context<Self>, key: &'static str, value: SharedString| {
            h_flex()
                .gap_1()
                .child(
                    Label::new(i18n_memory_analysis(cx, key))
                        .text_color(cx.theme().muted_foreground)
                        .text_sm(),
                )
                .child(Label::new(value).text_sm().font_weight(gpui::FontWeight::MEDIUM))
        };

        ZedisDivider::new()
            .gap_4()
            // Read-only data information display
            .child(
                h_flex()
                    .gap_4() // Use moderate spacing inside the data group
                    .items_center()
                    // DB Size
                    .when_some(self.dbsize, |this, dbsize| {
                        this.child(stat_item(cx, "dbsize", format_thousands(dbsize).into()))
                    })
                    // Estimated commands
                    .when(self.est_commands > 0, |this| {
                        this.child(stat_item(
                            cx,
                            "est_commands",
                            format!("~{}", format_thousands(self.est_commands)).into(),
                        ))
                    })
                    // Active maxmemory-policy chip — explains which heat
                    // metric the Heat column is showing.
                    .when(!self.policy.is_empty(), |this| {
                        this.child(stat_item(cx, "policy", self.policy.clone()))
                    })
                    // Progress
                    .when(!is_idle, |this| {
                        this.child(stat_item(cx, "progress", self.progress.clone()))
                    }),
            )
            // Ranking for the single-key TopN table. A dropdown rather than one
            // button per mode: the toolbar is already crowded, and this is a
            // single-valued choice — exactly what a select is for.
            //
            // Wrapped in a fixed-width box: `Select`'s outer element is
            // `size_full`, so on its own it stretches to fill the row (its own
            // `.w()` only refines the inner input).
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(i18n_memory_analysis(cx, "rank_by"))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    )
                    .child(
                        div()
                            .w(px(RANK_SELECT_WIDTH))
                            .flex_none()
                            .child(Select::new(&self.sort_state).small()),
                    ),
            )
            // ─── User interaction operation area ───
            .child(
                h_flex()
                    .gap_3() // Input box and button are closely related, spacing is smaller
                    .items_center()
                    // Scan Count input
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(i18n_memory_analysis(cx, "scan_count"))
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            )
                            .child(
                                Input::new(&self.scan_count_input_state)
                                    .small()
                                    .w(px(70.))
                                    .disabled(is_running),
                            ),
                    )
                    // Sample Ratio input
                    .when_some(self.dbsize, |this, _| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Label::new(i18n_memory_analysis(cx, "sample_ratio"))
                                        .text_color(cx.theme().muted_foreground)
                                        .text_sm(),
                                )
                                .child(
                                    Input::new(&self.ratio_input_state)
                                        .small()
                                        .w(px(70.))
                                        .disabled(is_running),
                                ),
                        )
                    })
                    // Start / Stop Button
                    .child(if is_running {
                        Button::new("stop-analysis")
                            .danger()
                            .small()
                            .label(i18n_memory_analysis(cx, "stop"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.stop_analysis(cx);
                            }))
                    } else {
                        Button::new("start-analysis")
                            .primary()
                            .small()
                            .disabled(self.dbsize.is_none())
                            .label(i18n_memory_analysis(cx, "start"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.start_analysis(cx);
                            }))
                    }),
            )
    }
    /// Render the AI advice panel. Hidden until an AI request has been
    /// started; then shows a loading line, the model's Markdown advice,
    /// or an error message, with a button to dismiss it.
    pub(super) fn render_ai_panel(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        if self.ai_status == AiStatus::Idle {
            return None;
        }
        // Theme colors must be copied out before the `cx.listener`
        // closure below (can't borrow `cx` across it).
        let border = cx.theme().border;
        let panel_bg = cx.theme().muted.opacity(0.4);
        let muted_fg = cx.theme().muted_foreground;
        let danger = cx.theme().red;

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::Bot))
                    .child(Label::new(i18n_memory_analysis(cx, "ai_panel_title")).font_semibold()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    // Copy the model's Markdown reply to the clipboard.
                    .when(self.ai_status == AiStatus::Done, |this| {
                        this.child(
                            Button::new("ai-copy-reply")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some(reply) = this.ai_output.clone() else {
                                        return;
                                    };
                                    cx.write_to_clipboard(ClipboardItem::new_string(reply.to_string()));
                                    window.push_notification(
                                        Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(
                        Button::new("ai-panel-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.clear_ai_result(cx);
                            })),
                    ),
            );

        let content = match self.ai_status {
            AiStatus::Running => Label::new(i18n_memory_analysis(cx, "ai_running"))
                .text_color(muted_fg)
                .into_any_element(),
            AiStatus::Done => {
                TextView::markdown("memory-analysis-ai-result", self.ai_output.clone().unwrap_or_default())
                    .style(ai_markdown_style())
                    .into_any_element()
            }
            AiStatus::Error => Label::new(self.ai_output.clone().unwrap_or_default())
                .text_color(danger)
                .into_any_element(),
            AiStatus::Idle => return None,
        };

        Some(
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(panel_bg)
                .child(header)
                .child(content)
                .into_any_element(),
        )
    }

    /// Map a recommendation to its localized `(title, subject, detail)`. The
    /// subject is the language-neutral concrete fact (key/prefix name, sizes,
    /// percentages) composed in Rust; title/detail come from the locale files.
    pub(super) fn reco_text(
        &self,
        reco: &Recommendation,
        cx: &gpui::App,
    ) -> (SharedString, Option<SharedString>, SharedString) {
        let k = |key: &str| i18n_memory_analysis(cx, key);
        match &reco.kind {
            RecoKind::BigKey { key, key_type, bytes } => (
                k("reco_big_key_title"),
                Some(format!("{key} · {key_type} · {}", format_memory(*bytes)).into()),
                k("reco_big_key_detail"),
            ),
            RecoKind::UnevictableKeys { no_ttl_pct, policy } => (
                k("reco_unevictable_title"),
                Some(format!("{policy} · {no_ttl_pct}%").into()),
                k("reco_unevictable_detail"),
            ),
            RecoKind::NoEvictionPolicy => (
                k("reco_noeviction_title"),
                Some("noeviction".into()),
                k("reco_noeviction_detail"),
            ),
            RecoKind::HighFragmentation { ratio, waste_bytes } => (
                k("reco_fragmentation_title"),
                Some(format!("{ratio:.2}× · {}", format_memory(*waste_bytes)).into()),
                k("reco_fragmentation_detail"),
            ),
            RecoKind::ManySmallStrings {
                prefix,
                keys,
                avg_bytes,
            } => (
                k("reco_small_strings_title"),
                Some(
                    format!(
                        "{prefix} · {} · ~{}",
                        format_thousands(*keys),
                        format_memory(*avg_bytes)
                    )
                    .into(),
                ),
                k("reco_small_strings_detail"),
            ),
            RecoKind::DominantPrefix { prefix, pct } => (
                k("reco_dominant_prefix_title"),
                Some(format!("{prefix} · {pct}%").into()),
                k("reco_dominant_prefix_detail"),
            ),
        }
    }

    /// Flatten the current recommendations into a plain-text block for the
    /// clipboard — one localized `[SEVERITY] Title (subject)` line plus its
    /// detail per finding.
    pub(super) fn recommendations_plaintext(&self, cx: &gpui::App) -> String {
        let mut s = String::new();
        for reco in &self.recommendations {
            let (title, subject, detail) = self.reco_text(reco, cx);
            let sev = match reco.severity {
                RecoSeverity::Critical => "[CRITICAL]",
                RecoSeverity::Warning => "[WARNING]",
                RecoSeverity::Info => "[INFO]",
            };
            s.push_str(sev);
            s.push(' ');
            s.push_str(&title);
            if let Some(sub) = subject {
                s.push_str(" (");
                s.push_str(&sub);
                s.push(')');
            }
            s.push('\n');
            s.push_str(&detail);
            s.push_str("\n\n");
        }
        s
    }

    /// The offline rule engine's verdict, shown automatically once a scan
    /// finishes. A green "healthy" line when there are no findings, otherwise
    /// a severity-colored list. Hidden entirely until a scan completes.
    pub(super) fn render_recommendations_panel(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        if self.status != AnalysisStatus::Finished {
            return None;
        }
        // Copy theme colors out before any `cx.listener` closure below.
        let border = cx.theme().border;
        let panel_bg = cx.theme().muted.opacity(0.4);
        let muted_fg = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let c_critical = cx.theme().danger;
        let c_warning = cx.theme().warning;
        let c_info = cx.theme().blue;
        let sev_color = move |s: RecoSeverity| match s {
            RecoSeverity::Critical => c_critical,
            RecoSeverity::Warning => c_warning,
            RecoSeverity::Info => c_info,
        };
        let sev_icon = |s: RecoSeverity| match s {
            RecoSeverity::Critical => IconName::CircleX,
            RecoSeverity::Warning => IconName::TriangleAlert,
            RecoSeverity::Info => IconName::Info,
        };

        let count = self.recommendations.len();
        // The AI deep-dive trigger lives here in the panel header (next to
        // Copy), not in the toolbar — it surfaces exactly when a finished
        // scan has data worth sending to the model.
        let has_data = self.prefix_count > 0 || self.single_count > 0;
        let ai_running = self.ai_status == AiStatus::Running;
        let header =
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(CustomIconName::ListCheck))
                        .child(Label::new(i18n_memory_analysis(cx, "reco_panel_title")).font_semibold())
                        .when(count > 0, |this| {
                            this.child(Label::new(format!("{count}")).text_xs().text_color(muted_fg))
                        }),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        // AI advice: send the report (key names / sizes / TTLs
                        // only) to the configured OpenAI-compatible endpoint.
                        .when(has_data, |this| {
                            this.child(
                                Button::new("reco-ai-analysis")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Bot)
                                    .disabled(ai_running)
                                    .label(if ai_running {
                                        i18n_memory_analysis(cx, "ai_analyzing")
                                    } else {
                                        i18n_memory_analysis(cx, "ai_analyze")
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_ai_analysis(window, cx);
                                    })),
                            )
                        })
                        .when(count > 0, |this| {
                            this.child(Button::new("reco-copy").ghost().small().icon(IconName::Copy).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let text = this.recommendations_plaintext(cx);
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    window.push_notification(
                                        Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                        cx,
                                    );
                                }),
                            ))
                        }),
                );

        let body = if count == 0 {
            h_flex()
                .gap_2()
                .items_center()
                .child(Icon::new(CustomIconName::CircleCheckBig).text_color(green))
                .child(
                    Label::new(i18n_memory_analysis(cx, "reco_healthy"))
                        .text_sm()
                        .text_color(muted_fg),
                )
                .into_any_element()
        } else {
            let mut list = v_flex().w_full().gap_2();
            for reco in &self.recommendations {
                let (title, subject, detail) = self.reco_text(reco, cx);
                let color = sev_color(reco.severity);
                list = list.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(sev_icon(reco.severity)).text_color(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Label::new(title).font_medium().text_color(color))
                                        .when_some(subject, |this, sub| {
                                            this.child(Label::new(sub).text_xs().text_color(muted_fg))
                                        }),
                                )
                                .child(Label::new(detail).text_sm().text_color(muted_fg)),
                        ),
                );
            }
            list.into_any_element()
        };

        Some(
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(panel_bg)
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    /// Pull the metrics history kept by the status bar heartbeat and
    /// render a fragmentation-ratio line chart. Returns `None` when
    /// there are fewer than two data points — a single sample isn't
    /// a "trend" yet, so don't waste vertical space.
    ///
    /// Color encodes severity using BOTH the ratio and the absolute
    /// wasted bytes (RSS - used). At very small dataset sizes the
    /// ratio is noisy (jemalloc fixed overhead is ~100MB regardless
    /// of `used_memory`) so a 6× ratio on a 20MB DB is normal, not
    /// a fire — we keep it green until the absolute waste is big
    /// enough to be a real cost.
    ///
    /// - `< FRAG_FLOOR_BYTES` waste → always green (noise floor)
    /// - waste ≥ floor AND ratio ≥ 2.0 → red
    /// - waste ≥ floor AND ratio ≥ 1.5 → yellow
    /// - otherwise → green
    pub(super) fn render_fragmentation_chart(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        // 200MB. Below this much absolute overhead, jemalloc's fixed
        // costs dominate and the ratio carries no signal. Any modern
        // server can absorb a few hundred MB of allocator slack.
        const FRAG_FLOOR_BYTES: i64 = 200 * 1024 * 1024;

        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return None;
        }
        let history = get_metrics_cache().list_metrics(&server_id);
        // Filter out zero ratios — INFO emits 0 before sampling finishes.
        let samples: Vec<(i64, f64, i64)> = history
            .iter()
            .filter(|m| m.mem_fragmentation_ratio > 0.0)
            .map(|m| {
                // fragmentation_bytes = RSS - used; saturating sub
                // because in rare cases RSS can momentarily be
                // smaller than used (RSS lags by one sampling tick).
                let frag_bytes = (m.used_memory_rss as i64).saturating_sub(m.used_memory as i64);
                (m.timestamp_ms, m.mem_fragmentation_ratio, frag_bytes)
            })
            .collect();
        if samples.len() < 2 {
            return None;
        }
        let dates: Vec<SharedString> = samples.iter().map(|(ts, _, _)| format_timestamp_ms(*ts)).collect();
        let values: Vec<f64> = samples.iter().map(|(_, v, _)| *v).collect();
        let latest_ratio = *values.last().unwrap_or(&1.0);
        let latest_frag_bytes = samples.last().map(|(_, _, b)| *b).unwrap_or(0);
        // Pad y_max slightly above the peak so the line doesn't touch
        // the top edge; clamp the floor at 2.0 so a flat-healthy chart
        // still has room for a future spike.
        let raw_max = values.iter().cloned().fold(f64::MIN, f64::max);
        let y_max = (raw_max * 1.1).max(2.0);

        let theme = cx.theme();
        // Severity needs BOTH a bad ratio AND a meaningful absolute
        // waste — see the constant doc above for why.
        let stroke = if latest_frag_bytes < FRAG_FLOOR_BYTES {
            theme.green
        } else if latest_ratio >= 2.0 {
            theme.red
        } else if latest_ratio >= 1.5 {
            theme.yellow
        } else {
            theme.green
        };
        // Format the absolute waste so users can sanity-check the
        // ratio. "6× ratio · 100MB waste" is much less scary than
        // just "6× ratio" alone.
        let waste_str = if latest_frag_bytes > 0 {
            humansize::format_size(
                latest_frag_bytes as u64,
                humansize::FormatSizeOptions::default().decimal_places(0),
            )
        } else {
            "0 B".to_string()
        };
        let label_text = format!(
            "{} · {}: {:.2}× ({} {})",
            i18n_memory_analysis(cx, "fragmentation_chart_title"),
            i18n_memory_analysis(cx, "fragmentation_chart_latest"),
            latest_ratio,
            waste_str,
            i18n_memory_analysis(cx, "fragmentation_chart_waste"),
        );

        // Aim for at most ~5 X-axis labels. Lower than the metrics
        // view's ~10 because this chart sits in a body that's the
        // user's full content width *but* can shrink with the window.
        // On a 600px-wide window with 100+ samples, 8 labels still
        // produced <5px gaps between adjacent HH:MM:SS strings — the
        // first label's right edge overlapped the second's left edge.
        // 5 labels gives comfortable spacing even at narrow widths.
        const TARGET_X_LABELS: usize = 5;
        let tick_margin = samples.len().div_ceil(TARGET_X_LABELS).max(1);
        let params = ChartParams {
            dates: Arc::new(dates),
            y_max,
            y_format: Box::new(|v| format!("{v:.2}")),
            tick_margin,
            border: theme.border,
            muted_fg: theme.muted_foreground,
        };
        let chart = make_line_canvas(params, Arc::new(values), stroke, false);

        Some(
            v_flex()
                // `w_full` is critical — without it the card collapses
                // to the label's natural width (~200px) and the canvas
                // inherits that, jamming HH:MM:SS x-axis labels on top
                // of each other. `flex_none` prevents vertical squeeze
                // when the body has many siblings.
                .w_full()
                .flex_none()
                .h(px(180.0))
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius_lg)
                .p_3()
                .child(div().font_semibold().child(label_text).mb_2())
                .child(chart)
                .into_any_element(),
        )
    }

    /// Render the TTL distribution body — a 6-bar histogram plus a
    /// summary line. Bars share the canvas helpers used by the Metrics
    /// panel so visual styling stays consistent. `ratio` is folded into
    /// the summary so users see both the sampled count and an estimated
    /// total ("12,345 sampled → ~123,450 estimated") which matters when
    /// they ran with `ratio < 1.0` and the absolute bar height alone
    /// doesn't reveal cluster impact.
    pub(super) fn render_ttl_histogram_body(&self, cx: &mut gpui::Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let h = &self.ttl_histogram;
        let total = h.total();

        // Bucket order = visual left→right = expiry urgency: imminent
        // first, "no TTL" last. Same i18n key naming pattern so the
        // localised label sits next to its raw bucket name in the source.
        let buckets: [(&'static str, u64); 6] = [
            ("ttl_bucket_lt_1m", h.lt_1m),
            ("ttl_bucket_lt_1h", h.lt_1h),
            ("ttl_bucket_lt_1d", h.lt_1d),
            ("ttl_bucket_lt_7d", h.lt_7d),
            ("ttl_bucket_gte_7d", h.gte_7d),
            ("ttl_bucket_no_ttl", h.no_ttl),
        ];

        let dates: Vec<SharedString> = buckets.iter().map(|(key, _)| i18n_memory_analysis(cx, key)).collect();
        let values: Vec<f64> = buckets.iter().map(|(_, count)| *count as f64).collect();

        // Pad y_max 10 % above the peak so the tallest bar doesn't
        // touch the top edge. Floor at 1.0 because zero values would
        // collapse the chart to a degenerate scale.
        let raw_max = values.iter().cloned().fold(0.0_f64, f64::max);
        let y_max = (raw_max * 1.1).max(1.0);

        // 6 buckets and the chart is usually wide → label every bar.
        let params = ChartParams {
            dates: Arc::new(dates),
            y_max,
            y_format: Box::new(|v| format!("{v:.0}")),
            tick_margin: 1,
            border: theme.border,
            muted_fg: muted,
        };

        // Pick fill colour by aggregate urgency: if the leftmost two
        // buckets (≤1h) dominate the histogram, paint amber to draw
        // the eye to the eviction cliff. Otherwise the standard chart_2.
        let imminent = h.lt_1m + h.lt_1h;
        let fill_color = if total > 0 && imminent * 2 > total {
            theme.yellow
        } else {
            theme.chart_2
        };
        let chart = make_bar_canvas(params, Arc::new(values), fill_color);

        // Summary line: sampled total + (if ratio<1) estimated full
        // population + no-TTL share (the "are we leaking?" signal).
        let summary_text: SharedString = {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            let estimated = if self.ratio > 0.0 && self.ratio < 1.0 {
                ((total as f64) / self.ratio as f64) as u64
            } else {
                total
            };
            let no_ttl_pct = if total > 0 {
                (h.no_ttl as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            rust_i18n::t!(
                "memory_analysis.ttl_summary_label",
                sampled = group_thousands(total),
                estimated = group_thousands(estimated),
                no_ttl_pct = format!("{no_ttl_pct:.1}"),
                locale = locale
            )
            .to_string()
            .into()
        };

        // Dominant-bucket callout: which bucket has the most keys? Helps
        // users spot the "everyone expires in the same hour" landmine
        // at a glance without parsing every bar.
        let dominant_label: Option<SharedString> =
            buckets
                .iter()
                .max_by_key(|(_, c)| *c)
                .filter(|(_, c)| *c > 0)
                .map(|(key, count)| {
                    let bucket_name = i18n_memory_analysis(cx, key);
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    rust_i18n::t!(
                        "memory_analysis.ttl_dominant_label",
                        bucket = bucket_name.as_ref(),
                        count = group_thousands(*count),
                        locale = locale
                    )
                    .to_string()
                    .into()
                });

        v_flex()
            .w_full()
            .flex_none()
            .gap_2()
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .h(px(220.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .p_3()
                    .child(
                        div()
                            .font_semibold()
                            .child(i18n_memory_analysis(cx, "ttl_histogram_title"))
                            .mb_2(),
                    )
                    .child(chart),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_2()
                    .child(Label::new(summary_text).text_sm().text_color(muted))
                    .when_some(dominant_label, |this, d| {
                        this.child(Label::new(d).text_xs().text_color(muted))
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_table_section(
        &mut self,
        title_key: &'static str,
        count: usize,
        table_view: impl IntoElement,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .h(px(SECTION_TITLE_HEIGHT))
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(i18n_memory_analysis(cx, title_key))
                            .text_color(cx.theme().foreground)
                            .text_sm(),
                    )
                    .child(
                        Label::new(format!("(Top {})", count))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    ),
            )
            .child(div().w_full().h(table_height(count)).child(table_view))
    }
}

impl gpui::Render for ZedisMemoryAnalysis {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Sync ratio InputState when changed programmatically
        if self.ratio_dirty {
            self.ratio_dirty = false;
            let ratio_text = format!("{:.4}", self.ratio);
            self.ratio_input_state
                .update(cx, |s, cx| s.set_value(ratio_text, window, cx));
        }

        // The heat probe resolved (or the server changed): Hot/Cold appear or
        // disappear from the "rank by" dropdown. Rebuilding drops the selection,
        // so re-select the active mode — which `start_analysis` has already
        // reset to Size when the probe came back empty.
        if self.should_rebuild_sort_items.take().is_some() {
            let options = sort_options(self.heat != HeatProbe::None, cx);
            let selected = options.iter().position(|o| o.mode == self.sort_mode).unwrap_or(0);
            self.sort_state.update(cx, |state, cx| {
                state.set_items(options, window, cx);
                state.set_selected_index(Some(IndexPath::new(selected)), window, cx);
            });
        }

        let is_running = self.status == AnalysisStatus::Running;
        let has_prefix = self.prefix_count > 0;
        let has_single = self.single_count > 0;
        let has_data = has_prefix || has_single;
        let has_ttl_data = self.ttl_histogram.total() > 0;

        // Lay the toolbar out as a single non-wrapping row inside a
        // horizontal scroll container. Modern IDEs (Zed included) keep dense
        // toolbars on one line and let the overflow scroll rather than
        // wrapping or stacking — it stays readable at any window width.
        let nav = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("memory-analysis-back")
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
            .child(Icon::new(CustomIconName::MemoryStick))
            .child(Label::new(i18n_memory_analysis(cx, "title")).text_color(cx.theme().foreground))
            .child(help_popover("memory-analysis-help", i18n_memory_analysis(cx, "help")));
        let functions = self.render_toolbar_functions(cx);

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .gap_2()
            // ── Toolbar: single row, horizontal-scroll on overflow ──
            // The h_flex is itself the scroll viewport (mirrors gpui-component's
            // tab_bar). `nav`/`functions` are `flex_none` so they keep their
            // natural width and overflow the row instead of being compressed —
            // that overflow is what the scroll container actually scrolls. The
            // `flex_1` spacer only grows when there is leftover space, pushing
            // the functions group to the right edge when everything fits.
            .child(
                h_flex()
                    .id("memory-analysis-toolbar")
                    .w_full()
                    .flex_none()
                    .h(px(40.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .overflow_x_scroll()
                    .child(nav.flex_none())
                    .child(div().flex_1())
                    .child(functions.flex_none()),
            )
            // ── Body ──
            .child({
                let mut body = v_flex()
                    .flex_1()
                    .w_full()
                    .p_2()
                    .min_h_0()
                    .gap_2()
                    .id("memory-analysis-body")
                    .overflow_y_scroll();

                // Offline recommendations — the local rule engine's verdict,
                // shown automatically once a scan finishes (no AI, no config).
                if let Some(panel) = self.render_recommendations_panel(cx) {
                    body = body.child(panel);
                }

                // AI advice panel — pinned to the top so the result is
                // visible immediately after the request completes.
                if let Some(panel) = self.render_ai_panel(cx) {
                    body = body.child(panel);
                }

                // Scan-failure banner — surfaces a SCAN error (which would
                // otherwise have been hidden behind a fake "100% / Finished")
                // while keeping any partial results visible below it.
                if self.status == AnalysisStatus::Error
                    && let Some(message) = self.scan_error.clone()
                {
                    let theme = cx.theme();
                    body = body.child(
                        div()
                            .w_full()
                            .p_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .bg(theme.danger.opacity(0.1))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(Icon::new(IconName::CircleX).text_color(theme.danger))
                                    .child(Label::new(message).text_sm().text_color(theme.danger)),
                            ),
                    );
                }

                // Combined dashboard — no tab selection. One SCAN feeds every
                // section, so they stack in a single scroll view: fragmentation
                // trend, TTL distribution, then the prefix and single-key tables.

                // Fragmentation trend chart (pulls from METRICS_CACHE populated
                // by the status_bar heartbeat). Always attempted — even before
                // the user clicks "Analyse" it shows the running
                // mem_fragmentation_ratio, so it doubles as ambient diagnostic.
                if let Some(chart) = self.render_fragmentation_chart(cx) {
                    body = body.child(chart);
                }

                // Unified empty state: nothing sampled yet and not running.
                if !has_data && !has_ttl_data && !is_running {
                    body = body.child(div().size_full().flex().items_center().justify_center().child(
                        Label::new(i18n_memory_analysis(cx, "no_data")).text_color(cx.theme().muted_foreground),
                    ));
                }

                // TTL distribution histogram (same scan — no extra round-trip).
                if has_ttl_data {
                    body = body.child(self.render_ttl_histogram_body(cx));
                }

                // Prefix groups table
                if has_prefix {
                    let table = DataTable::new(&self.prefix_table)
                        .stripe(true)
                        .bordered(true)
                        .scrollbar_visible(false, false);

                    body = body.child(self.render_table_section(
                        "prefix_table_title",
                        self.prefix_count,
                        table,
                        window,
                        cx,
                    ));
                }

                // Single keys table
                if has_single {
                    let table = DataTable::new(&self.single_table)
                        .stripe(true)
                        .bordered(true)
                        .scrollbar_visible(false, false);

                    body = body.child(self.render_table_section(
                        "single_table_title",
                        self.single_count,
                        table,
                        window,
                        cx,
                    ));
                }

                body
            })
            .into_any_element()
    }
}
