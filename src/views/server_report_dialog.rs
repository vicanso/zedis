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

//! The server's own diagnostics on request: `MEMORY DOCTOR` with the
//! `MEMORY STATS` numbers behind it, and `LATENCY DOCTOR`. One dialog for
//! both — the prose the server answers, plus a metric table when there is
//! one — because what a GUI adds here is the button, not the analysis.

use crate::connection::{NodeReply, get_connection_manager, latency_doctor, memory_doctor, memory_stats};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_metrics, i18n_slowlog_editor};
use gpui::{App, Entity, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme,
    label::Label,
    scroll::ScrollableElement,
    table::{DataTable, TableState},
    v_flex,
};
use humansize::{DECIMAL, format_size};
use zedis_ui::{TextColumn, ZedisDialog, ZedisTextTable};

const DIALOG_WIDTH: f32 = 720.0;
const TABLE_HEIGHT: f32 = 320.0;
const NODE_WIDTH: f32 = 170.0;
/// The scroll viewport: what the window leaves for it, within these.
const MIN_BODY_HEIGHT: f32 = 160.0;
const MAX_BODY_HEIGHT: f32 = 640.0;
/// Window height the dialog chrome (title, footer, margins) takes.
const DIALOG_CHROME_HEIGHT: f32 = 220.0;
/// Rough text metrics for sizing the viewport to short content: characters
/// per wrapped line at the dialog's width, and the line height.
const CHARS_PER_LINE: usize = 95;
const LINE_HEIGHT: f32 = 20.0;
const METRIC_WIDTH: f32 = 300.0;
const VALUE_WIDTH: f32 = 220.0;

/// Which report the dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerReport {
    /// `MEMORY DOCTOR` + `MEMORY STATS`.
    Memory,
    /// `LATENCY DOCTOR`.
    Latency,
}

/// One node's prose and, for memory, its rows of `MEMORY STATS`. A single
/// server is one entry with an empty node; a cluster answers per master.
struct NodeReport {
    node: String,
    text: String,
    stats: Vec<(String, String)>,
}

type ReportContent = Vec<NodeReport>;

pub struct ZedisServerReportDialog {
    report: ServerReport,
    /// `None` until the server answers.
    content: Option<Result<ReportContent, String>>,
    /// Built on first render — a table needs the window.
    table: Option<Entity<TableState<ZedisTextTable>>>,
}

/// A byte count next to its raw value, for the metrics that are one. The
/// names are RedisJSON's: allocations, overheads and buffers are bytes;
/// counts, ratios and percentages are not.
fn stat_value(metric: &str, raw: &str) -> SharedString {
    let is_bytes = metric.ends_with(".allocated")
        || metric.ends_with(".bytes")
        || metric.ends_with(".buffer")
        || metric.ends_with(".backlog")
        || metric.ends_with(".caches")
        || metric.contains(".overhead.")
        || metric.starts_with("overhead.")
        || metric.starts_with("clients.");
    match raw.parse::<u64>() {
        Ok(bytes) if is_bytes && bytes >= 1024 => format!("{raw} ({})", format_size(bytes, DECIMAL)).into(),
        _ => raw.to_string().into(),
    }
}

impl Render for ZedisServerReportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(content) = &self.content else {
            return v_flex()
                .w_full()
                .py_4()
                .child(Label::new(i18n_common(cx, "loading")).text_sm().text_color(muted))
                .into_any_element();
        };
        let nodes = match content {
            Ok(nodes) => nodes,
            Err(message) => {
                return v_flex()
                    .w_full()
                    .child(Label::new(message.clone()).text_sm().text_color(cx.theme().danger))
                    .into_any_element();
            }
        };
        let per_node = nodes.iter().any(|node| !node.node.is_empty());
        let has_stats = nodes.iter().any(|node| !node.stats.is_empty());
        // A cluster answers once per master, so the body can outgrow the
        // window: scroll it inside a viewport of definite height (`max_h`
        // would clip, not scroll). Sized to the content when that is
        // shorter, so a one-line answer does not sit in an empty box.
        let estimated = estimated_height(nodes, per_node, has_stats);
        let available = window.viewport_size().height.as_f32() - DIALOG_CHROME_HEIGHT;
        let body_height = estimated
            .min(available.clamp(MIN_BODY_HEIGHT, MAX_BODY_HEIGHT))
            .max(MIN_BODY_HEIGHT);
        let heading = match self.report {
            ServerReport::Memory => i18n_metrics(cx, "memory_doctor"),
            ServerReport::Latency => i18n_slowlog_editor(cx, "latency_doctor_command"),
        };
        // "Hi Sam" / "Dave" are the server's: MEMORY DOCTOR and LATENCY
        // DOCTOR answer in character, and the reply is shown as answered.
        let mut body = v_flex()
            .w_full()
            .gap_2()
            .child(
                Label::new(i18n_metrics(cx, "report_verbatim_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(Label::new(heading).text_xs().text_color(muted));
        for node in nodes {
            if per_node {
                body = body.child(
                    Label::new(node.node.clone())
                        .text_xs()
                        .font_family(get_mono_font_family()),
                );
            }
            body = body.child(
                div()
                    .w_full()
                    .p_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .child(Label::new(node.text.clone()).text_sm()),
            );
        }
        if has_stats {
            let table = self
                .table
                .get_or_insert_with(|| {
                    let mut columns = Vec::with_capacity(3);
                    if per_node {
                        columns
                            .push(TextColumn::new("node", i18n_metrics(cx, "report_col_node"), NODE_WIDTH).sortable());
                    }
                    columns.push(
                        TextColumn::new("metric", i18n_metrics(cx, "report_col_metric"), METRIC_WIDTH).sortable(),
                    );
                    columns.push(TextColumn::new(
                        "value",
                        i18n_metrics(cx, "report_col_value"),
                        VALUE_WIDTH,
                    ));
                    let rows = nodes
                        .iter()
                        .flat_map(|node| {
                            node.stats.iter().map(move |(metric, raw)| {
                                let mut row: Vec<SharedString> = Vec::with_capacity(3);
                                if per_node {
                                    row.push(node.node.clone().into());
                                }
                                row.push(metric.clone().into());
                                row.push(stat_value(metric, raw));
                                row
                            })
                        })
                        .collect();
                    let mut table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
                        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"));
                    table.set_rows(rows);
                    cx.new(|cx| TableState::new(table, window, cx))
                })
                .clone();
            body = body
                .child(
                    Label::new(i18n_metrics(cx, "memory_stats"))
                        .text_xs()
                        .text_color(muted)
                        .mt_2(),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(TABLE_HEIGHT))
                        .font_family(get_mono_font_family())
                        .child(DataTable::new(&table).bordered(false)),
                );
        }
        v_flex()
            .w_full()
            .h(px(body_height))
            .overflow_y_scrollbar()
            .child(body)
            .into_any_element()
    }
}

/// How tall the body would be unscrolled, from the text alone.
fn estimated_height(nodes: &[NodeReport], per_node: bool, has_stats: bool) -> f32 {
    let mut height = 2.0 * LINE_HEIGHT + 16.0;
    for node in nodes {
        if per_node {
            height += LINE_HEIGHT + 8.0;
        }
        let lines: usize = node
            .text
            .split('\n')
            .map(|line| line.chars().count().div_ceil(CHARS_PER_LINE).max(1))
            .sum();
        height += lines as f32 * LINE_HEIGHT + 24.0 + 8.0;
    }
    if has_stats {
        height += LINE_HEIGHT + 8.0 + TABLE_HEIGHT + 8.0;
    }
    height
}

/// Open `report` for the server behind `server_state`: the dialog shows at
/// once and fills in when the server answers. A command the server lacks
/// or forbids is reported in the dialog and noted for the feature matrix.
pub fn open_server_report_dialog(
    server_state: Entity<ZedisServerState>,
    report: ServerReport,
    window: &mut Window,
    cx: &mut App,
) {
    let (server_id, db) = {
        let state = server_state.read(cx);
        (state.server_id().to_string(), state.db())
    };
    let view = cx.new(|_| ZedisServerReportDialog {
        report,
        content: None,
        table: None,
    });
    let title = match report {
        ServerReport::Memory => i18n_metrics(cx, "memory_report"),
        ServerReport::Latency => i18n_slowlog_editor(cx, "latency_doctor"),
    };
    let weak = view.downgrade();
    cx.spawn(async move |cx| {
        let result: Result<ReportContent, Error> = cx
            .background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                Ok(match report {
                    ServerReport::Memory => {
                        let doctor = memory_doctor(&mut conn).await?;
                        let mut stats = memory_stats(&mut conn).await?;
                        doctor
                            .into_iter()
                            .map(|NodeReply { node, value: text }| {
                                // Both commands fan out to the same masters;
                                // pair each node's numbers with its prose.
                                let stats = stats
                                    .iter()
                                    .position(|reply| reply.node == node)
                                    .map(|at| stats.remove(at).value)
                                    .unwrap_or_default();
                                NodeReport { node, text, stats }
                            })
                            .collect()
                    }
                    ServerReport::Latency => latency_doctor(&mut conn)
                        .await?
                        .into_iter()
                        .map(|NodeReply { node, value: text }| NodeReport {
                            node,
                            text,
                            stats: Vec::new(),
                        })
                        .collect(),
                })
            })
            .await;
        let content = match result {
            Ok(content) => Ok(content),
            Err(error) => {
                server_state.update(cx, |state, cx| {
                    state.note_command_error(&error, cx);
                });
                Err(error.to_string())
            }
        };
        let _ = weak.update(cx, |this, cx| {
            this.content = Some(content);
            cx.notify();
        });
    })
    .detach();
    let body = view.clone();
    let close = i18n_common(cx, "close");
    ZedisDialog::new(title)
        .w(px(DIALOG_WIDTH))
        .ok_text(close.clone())
        .button_props(dialog_button_props(cx).ok_text(close))
        .child(move || body.clone())
        .on_ok(|_, _, _| true)
        .open(window, cx);
}

#[cfg(test)]
mod tests {
    use super::stat_value;

    #[test]
    fn byte_metrics_get_a_readable_size_and_the_rest_stay_raw() {
        assert_eq!(stat_value("peak.allocated", "1048576").as_ref(), "1048576 (1.05 MB)");
        assert_eq!(stat_value("db.0.overhead.hashtable.main", "72").as_ref(), "72");
        assert_eq!(stat_value("keys.count", "1048576").as_ref(), "1048576");
        assert_eq!(stat_value("fragmentation", "1.23").as_ref(), "1.23");
    }
}
