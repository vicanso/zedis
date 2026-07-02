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

//! Command-stats diagnostics — "why is Redis busy".
//!
//! Polls `INFO commandstats` on a heartbeat (aggregated across all cluster
//! masters) and shows a per-command **call-rate** table computed from the
//! delta between two samples — cumulative counters are meaningless on their
//! own. A counter reset (`CONFIG RESETSTAT`, Δ < 0) reads as zero rather than
//! a spike. (Per-key hotness lives in the Memory Analyzer's "Hottest" sort,
//! so it is intentionally not duplicated here.)

use crate::connection::{CommandStat, get_connection_manager};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{ServerView, ZedisGlobalStore, ZedisServerState, i18n_common, i18n_server_load};
use gpui::{Context, Entity, ScrollHandle, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    v_flex,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::{Duration, Instant};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Poll interval between command-stats samples.
const POLL_SECS: u64 = 3;
/// Fixed width of each numeric column.
const NUM_COL: f32 = 96.0;

/// Which column the table is sorted by.
#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Command,
    Rate,
    AvgUs,
    Calls,
}

/// One command-stats display row (delta-derived).
struct CmdRow {
    name: SharedString,
    /// Calls per second over the last sampling interval.
    rate: f64,
    /// Average µs/call (interval if available, else cumulative).
    avg_us: f64,
    /// Cumulative call count.
    calls: u64,
}

pub struct ZedisServerLoad {
    server_id: String,
    db: usize,
    cmd_rows: Vec<CmdRow>,
    cmd_error: Option<SharedString>,
    /// Previous sample (command → (calls, usec)) for delta computation.
    prev: HashMap<String, (u64, u64)>,
    last_sample_at: Option<Instant>,
    poll_task: Option<Task<()>>,
    sort_by: SortBy,
    sort_desc: bool,
    scroll: ScrollHandle,
}

impl ZedisServerLoad {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let mut this = Self {
            server_id,
            db,
            cmd_rows: Vec::new(),
            cmd_error: None,
            prev: HashMap::new(),
            last_sample_at: None,
            poll_task: None,
            sort_by: SortBy::Rate,
            sort_desc: true,
            scroll: ScrollHandle::new(),
        };
        this.start_polling(cx);
        this
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let result = fetch_command_stats(server_id.clone(), db).await;
                if this
                    .update(cx, |this, cx| this.apply_command_stats(result, cx))
                    .is_err()
                {
                    break; // view dropped
                }
                cx.background_executor().timer(Duration::from_secs(POLL_SECS)).await;
            }
        }));
    }

    fn apply_command_stats(&mut self, result: Result<Vec<CommandStat>>, cx: &mut Context<Self>) {
        match result {
            Ok(stats) => {
                let now = Instant::now();
                let dt = self
                    .last_sample_at
                    .map(|t| now.duration_since(t).as_secs_f64())
                    .filter(|d| *d > 0.0);
                let rows: Vec<CmdRow> = stats
                    .iter()
                    .map(|s| {
                        let (pc, pu) = self.prev.get(&s.name).copied().unwrap_or((0, 0));
                        // saturating_sub → a counter reset (CONFIG RESETSTAT)
                        // reads as 0 this interval rather than a huge spike.
                        let dcalls = s.calls.saturating_sub(pc);
                        let dusec = s.usec.saturating_sub(pu);
                        let rate = dt.map(|d| dcalls as f64 / d).unwrap_or(0.0);
                        let avg_us = if dcalls > 0 {
                            dusec as f64 / dcalls as f64
                        } else if s.calls > 0 {
                            s.usec as f64 / s.calls as f64
                        } else {
                            0.0
                        };
                        CmdRow {
                            name: s.name.clone().into(),
                            rate,
                            avg_us,
                            calls: s.calls,
                        }
                    })
                    .collect();
                self.cmd_rows = rows;
                self.sort_rows();
                self.prev = stats.into_iter().map(|s| (s.name, (s.calls, s.usec))).collect();
                self.last_sample_at = Some(now);
                self.cmd_error = None;
            }
            Err(e) => self.cmd_error = Some(e.to_string().into()),
        }
        cx.notify();
    }

    /// Re-sort `cmd_rows` in place by the active column / direction.
    fn sort_rows(&mut self) {
        let desc = self.sort_desc;
        let dir = |o: Ordering| if desc { o.reverse() } else { o };
        match self.sort_by {
            SortBy::Command => self.cmd_rows.sort_by(|a, b| dir(a.name.cmp(&b.name))),
            SortBy::Rate => self
                .cmd_rows
                .sort_by(|a, b| dir(a.rate.partial_cmp(&b.rate).unwrap_or(Ordering::Equal))),
            SortBy::AvgUs => self
                .cmd_rows
                .sort_by(|a, b| dir(a.avg_us.partial_cmp(&b.avg_us).unwrap_or(Ordering::Equal))),
            SortBy::Calls => self.cmd_rows.sort_by(|a, b| dir(a.calls.cmp(&b.calls))),
        }
    }

    /// Header click: flip direction on the active column, otherwise switch to
    /// the clicked column (numeric columns default to descending — biggest
    /// first — the command name to ascending).
    fn set_sort(&mut self, col: SortBy, cx: &mut Context<Self>) {
        if self.sort_by == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_by = col;
            self.sort_desc = col != SortBy::Command;
        }
        self.sort_rows();
        cx.notify();
    }

    fn render_table(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        if let Some(err) = self.cmd_error.clone() {
            return div()
                .p_4()
                .child(Label::new(err).text_sm().text_color(cx.theme().danger))
                .into_any_element();
        }
        if self.cmd_rows.is_empty() {
            return div()
                .p_4()
                .child(Label::new(i18n_server_load(cx, "loading")).text_sm().text_color(muted))
                .into_any_element();
        }

        // Clickable header — click a column to sort by it, click the active
        // column again to flip direction. The active column shows an arrow
        // and brighter text.
        let fg = cx.theme().foreground;
        let active = self.sort_by;
        let desc = self.sort_desc;
        let arrow = |col: SortBy| {
            if active == col {
                if desc { " ↓" } else { " ↑" }
            } else {
                ""
            }
        };
        let head_color = |col: SortBy| if active == col { fg } else { muted };
        let header = h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("sl-hdr-command")
                    .flex_1()
                    .min_w_0()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| this.set_sort(SortBy::Command, cx)))
                    .child(
                        Label::new(format!(
                            "{}{}",
                            i18n_server_load(cx, "col_command"),
                            arrow(SortBy::Command)
                        ))
                        .text_xs()
                        .text_color(head_color(SortBy::Command)),
                    ),
            )
            .child(
                h_flex()
                    .id("sl-hdr-rate")
                    .w(px(NUM_COL))
                    .justify_end()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| this.set_sort(SortBy::Rate, cx)))
                    .child(
                        Label::new(format!("{}{}", i18n_server_load(cx, "col_rate"), arrow(SortBy::Rate)))
                            .text_xs()
                            .text_color(head_color(SortBy::Rate)),
                    ),
            )
            .child(
                h_flex()
                    .id("sl-hdr-avg")
                    .w(px(NUM_COL))
                    .justify_end()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| this.set_sort(SortBy::AvgUs, cx)))
                    .child(
                        Label::new(format!("{}{}", i18n_server_load(cx, "col_avg"), arrow(SortBy::AvgUs)))
                            .text_xs()
                            .text_color(head_color(SortBy::AvgUs)),
                    ),
            )
            .child(
                h_flex()
                    .id("sl-hdr-calls")
                    .w(px(NUM_COL))
                    .justify_end()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| this.set_sort(SortBy::Calls, cx)))
                    .child(
                        Label::new(format!("{}{}", i18n_server_load(cx, "col_calls"), arrow(SortBy::Calls)))
                            .text_xs()
                            .text_color(head_color(SortBy::Calls)),
                    ),
            );

        let stripe_bg = cx.theme().table_even;
        let mut list = v_flex().w_full().child(header);
        for (row_ix, r) in self.cmd_rows.iter().enumerate() {
            let is_stripe = row_ix % 2 != 0;
            list = list.child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .when(is_stripe, |this| this.bg(stripe_bg))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(r.name.clone()).text_xs().truncate()),
                    )
                    .child(
                        h_flex().w(px(NUM_COL)).justify_end().child(
                            Label::new(format!("{:.1}/s", r.rate))
                                .text_xs()
                                .font_semibold()
                                .text_color(cx.theme().foreground),
                        ),
                    )
                    .child(
                        h_flex()
                            .w(px(NUM_COL))
                            .justify_end()
                            .child(Label::new(format!("{:.0} µs", r.avg_us)).text_xs().text_color(muted)),
                    )
                    .child(
                        h_flex()
                            .w(px(NUM_COL))
                            .justify_end()
                            .child(Label::new(format_count(r.calls)).text_xs().text_color(muted)),
                    ),
            );
        }
        list.into_any_element()
    }
}

/// Compact a large call count: `12345 → 12.3k`, `4500000 → 4.5M`.
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

impl Render for ZedisServerLoad {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_table(cx);
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
                        Button::new("server-load-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Label::new(i18n_server_load(cx, "title")).font_semibold()),
            )
            .child(
                div()
                    .id("server-load-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }
}

async fn fetch_command_stats(server_id: String, db: usize) -> Result<Vec<CommandStat>> {
    let client = get_connection_manager().get_client(&server_id, db).await?;
    client.command_stats().await
}
