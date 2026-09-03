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

//! Raw `INFO` browser: every field the server reports, in one filterable
//! table. The structured panels (Metrics / Persistence / Topology / ...)
//! cover the common fields; this page exists for the long tail —
//! `errorstats`, `latencystats`, fork/COW costs, replication sync
//! counters, uptime — without dropping to the terminal.
//!
//! Fetches `INFO everything` (Redis 7+), degrading to `INFO all` then
//! plain `INFO` on older servers (unknown sections reply empty rather
//! than erroring). On clusters every master is queried and the section
//! column carries the node address, so filtering a field name compares
//! it across nodes.

use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::helpers::{KvDelta, build_csv, get_mono_font_family, kv_diff};
use crate::states::{
    InfoSnapshot, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip,
    content_area_width, i18n_common, i18n_server_info,
};
use crate::views::export_to_file;
use chrono::Local;
use gpui::{Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{DataTable, TableState},
    v_flex,
};
use redis::cmd;
use std::rc::Rc;
use tracing::{error, info};
use zedis_ui::{CellStyle, CellStyleProvider, TextColumn, ZedisDivider, ZedisTextTable, help_popover};

use crate::assets::CustomIconName;

const SECTION_COL_WIDTH: f32 = 220.;
const FIELD_COL_WIDTH: f32 = 300.;

/// One `field: value` line of an INFO reply, tagged with its section (and
/// node address on clusters).
#[derive(Clone, Debug)]
struct InfoRow {
    section: SharedString,
    field: SharedString,
    value: SharedString,
}

/// Split one node's INFO text into `(section, [(field, value)])` groups.
/// Lines outside any `# Section` header land in an unnamed section (never
/// produced by real servers, tolerated for robustness).
fn parse_info_sections(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: (String, Vec<(String, String)>) = (String::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('#') {
            if !current.1.is_empty() {
                sections.push(std::mem::take(&mut current));
            }
            current.0 = name.trim().to_string();
        } else if let Some((key, value)) = line.split_once(':') {
            current.1.push((key.to_string(), value.to_string()));
        }
    }
    if !current.1.is_empty() {
        sections.push(current);
    }
    sections
}

/// Flatten per-node INFO texts into display rows. Multi-node (cluster)
/// results prefix the section with the node address so identical fields
/// stay distinguishable — and comparable under one filter.
fn build_rows(replies: &[(String, String)]) -> Vec<InfoRow> {
    let multi_node = replies.len() > 1;
    let mut rows = Vec::new();
    for (node, text) in replies {
        for (section, fields) in parse_info_sections(text) {
            let section_label: SharedString = if multi_node {
                format!("{node} · {section}").into()
            } else {
                section.into()
            };
            for (field, value) in fields {
                rows.push(InfoRow {
                    section: section_label.clone(),
                    field: field.into(),
                    value: value.into(),
                });
            }
        }
    }
    rows
}

/// Case-insensitive substring filter over section, field, and value.
fn filter_rows(rows: &[InfoRow], filter: &str) -> Vec<InfoRow> {
    if filter.is_empty() {
        return rows.to_vec();
    }
    let needle = filter.to_lowercase();
    rows.iter()
        .filter(|r| {
            r.field.to_lowercase().contains(&needle)
                || r.section.to_lowercase().contains(&needle)
                || r.value.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

impl InfoRow {
    fn cells(&self) -> Vec<SharedString> {
        vec![self.section.clone(), self.field.clone(), self.value.clone()]
    }
}

/// Three text columns; the section column is muted, as it repeats down
/// every row of a section.
fn build_table(window: &mut Window, cx: &mut gpui::App) -> ZedisTextTable {
    let content_width = content_area_width(window, cx).as_f32();
    let value_w = content_width - SECTION_COL_WIDTH - FIELD_COL_WIDTH - 26.;
    let columns = [
        ("section", SECTION_COL_WIDTH),
        ("field", FIELD_COL_WIDTH),
        ("value", value_w),
    ]
    .into_iter()
    .map(|(key, w)| TextColumn::new(key, i18n_server_info(cx, key), w))
    .collect();
    let muted_section: CellStyleProvider = Rc::new(|col_ix, _cells, cx| CellStyle {
        color: (col_ix == 0).then(|| cx.theme().muted_foreground),
        ..Default::default()
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_style(muted_section)
}

/// Raw INFO browser view — see the module docs.
///
/// The comparison snapshot is NOT stored here: tool views are dropped on
/// route change, so it lives on [`ZedisServerState`] ([`InfoSnapshot`])
/// and survives navigating away and back within a connection session.
pub struct ZedisServerInfo {
    server_state: Entity<ZedisServerState>,
    filter_input_state: Entity<InputState>,
    table_state: Entity<TableState<ZedisTextTable>>,
    /// All parsed rows, unfiltered — the filter narrows a copy into the
    /// table delegate so clearing it is instant.
    rows: Vec<InfoRow>,
    /// When true the table shows the field-level diff between the
    /// session snapshot and the current rows instead of the raw field
    /// list. View-local: re-entering the panel lands on the raw list
    /// with the snapshot banner offering Compare again.
    diff_mode: bool,
    /// `(changed, added, removed)` counts of the last computed diff, for
    /// the snapshot banner.
    diff_counts: (usize, usize, usize),
    /// Rows currently shown (after filter), for the count chip.
    visible_count: usize,
    /// Unfiltered size of whatever the table is showing (raw rows or
    /// diff rows), the denominator of the count chip.
    display_total: usize,
    loading: bool,
    error: Option<SharedString>,
    refreshed_at: Option<SharedString>,
    refresh_task: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl ZedisServerInfo {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let filter_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_server_info(cx, "filter_placeholder"))
        });
        subscriptions.push(
            cx.subscribe_in(&filter_input_state, window, |view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    view.apply_filter(cx);
                }
            }),
        );
        // Startup restore builds this view *before* the `ServerSelected`
        // announce populates `server_state` (same trap as the memory
        // analyzer's dbsize) — the construction-time refresh sees an empty
        // server id and bails, so re-fetch when the selection lands. Also
        // covers switching servers while staying on this route.
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                // The snapshot itself dies with the session
                // (`ZedisServerState::reset` clears it on switch) — only
                // the view-local compare toggle needs resetting here.
                this.diff_mode = false;
                this.refresh(cx);
            }
        }));
        let table_state = cx.new(|cx| TableState::new(build_table(window, cx), window, cx));

        info!("Creating new server info view");
        let mut this = Self {
            server_state,
            filter_input_state,
            table_state,
            rows: Vec::new(),
            diff_mode: false,
            diff_counts: (0, 0, 0),
            visible_count: 0,
            display_total: 0,
            loading: false,
            error: None,
            refreshed_at: None,
            refresh_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    /// Whether the session holds a comparison snapshot (kept on the
    /// server state so it survives panel teardown).
    fn has_snapshot(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).info_snapshot().is_some()
    }

    /// Re-filter the current display source (raw rows, or the snapshot
    /// diff when compare mode is on) into the table delegate.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let source: Vec<InfoRow> = if self.diff_mode && self.has_snapshot(cx) {
            self.build_diff_rows(cx)
        } else {
            self.rows.clone()
        };
        self.display_total = source.len();
        let filter: SharedString = self.filter_input_state.read(cx).value();
        let visible = filter_rows(&source, filter.as_ref());
        self.visible_count = visible.len();
        self.table_state.update(cx, |s, _| {
            s.delegate_mut().set_rows(visible.iter().map(InfoRow::cells).collect())
        });
        cx.notify();
    }

    /// Field-level diff between the snapshot and the current rows,
    /// mapped onto the existing three-column table: section = change
    /// tag, field = "section · field", value = old → new. Reusing the
    /// table keeps the diff virtualized and filterable for free. Also
    /// refreshes `diff_counts` for the banner.
    fn build_diff_rows(&mut self, cx: &mut Context<Self>) -> Vec<InfoRow> {
        let Some(snapshot) = self.server_state.read(cx).info_snapshot().cloned() else {
            self.diff_counts = (0, 0, 0);
            return Vec::new();
        };
        let key_of = |section: &SharedString, field: &SharedString| {
            if section.is_empty() {
                field.to_string()
            } else {
                format!("{section} · {field}")
            }
        };
        let old: Vec<(String, String)> = snapshot
            .rows
            .iter()
            .map(|(section, field, value)| (key_of(section, field), value.to_string()))
            .collect();
        let new: Vec<(String, String)> = self
            .rows
            .iter()
            .map(|r| (key_of(&r.section, &r.field), r.value.to_string()))
            .collect();

        let added_tag = i18n_server_info(cx, "diff_added");
        let removed_tag = i18n_server_info(cx, "diff_removed");
        let changed_tag = i18n_server_info(cx, "diff_changed");

        let mut counts = (0_usize, 0_usize, 0_usize);
        let rows = kv_diff(&old, &new)
            .into_iter()
            .map(|entry| {
                let (tag, value) = match entry.delta {
                    KvDelta::Added => {
                        counts.1 += 1;
                        (added_tag.clone(), entry.new.unwrap_or_default())
                    }
                    KvDelta::Removed => {
                        counts.2 += 1;
                        (removed_tag.clone(), entry.old.unwrap_or_default())
                    }
                    KvDelta::Changed => {
                        counts.0 += 1;
                        (
                            changed_tag.clone(),
                            format!("{} → {}", entry.old.unwrap_or_default(), entry.new.unwrap_or_default()),
                        )
                    }
                };
                InfoRow {
                    section: tag,
                    field: entry.key.into(),
                    value: value.into(),
                }
            })
            .collect();
        self.diff_counts = counts;
        rows
    }

    /// Freeze the current rows as the "before" side of a comparison,
    /// stored on the server state so it outlives this view. Retaking
    /// replaces the previous snapshot.
    fn take_snapshot(&mut self, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let snapshot = InfoSnapshot {
            rows: self
                .rows
                .iter()
                .map(|r| (r.section.clone(), r.field.clone(), r.value.clone()))
                .collect(),
            taken_at: Local::now().format("%H:%M:%S").to_string().into(),
        };
        self.server_state
            .update(cx, |state, _| state.set_info_snapshot(Some(snapshot)));
        self.apply_filter(cx);
    }

    fn toggle_compare(&mut self, cx: &mut Context<Self>) {
        if !self.has_snapshot(cx) {
            return;
        }
        self.diff_mode = !self.diff_mode;
        self.apply_filter(cx);
    }

    fn clear_snapshot(&mut self, cx: &mut Context<Self>) {
        if !self.has_snapshot(cx) {
            return;
        }
        self.server_state.update(cx, |state, _| state.set_info_snapshot(None));
        self.diff_mode = false;
        self.apply_filter(cx);
    }

    /// Export whatever the table currently shows (filtered raw fields,
    /// or the diff rows in compare mode) as CSV.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let rows = self.table_state.read(cx).delegate().visible_rows();
        if rows.is_empty() {
            return;
        }
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|cells| cells.iter().map(|c| c.to_string()).collect())
            .collect();
        let csv = build_csv(&["section", "field", "value"], &data);
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            "server-info.csv",
            success,
            error,
        );
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        // No selection yet (startup restore) — stay quiet instead of
        // erroring on an empty id; the ServerSelected subscription will
        // call back in once the connection is announced.
        if server_id.is_empty() {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();

        self.refresh_task = Some(cx.spawn(async move |handle, cx| {
            let result: Result<Vec<(String, String)>, Error> = cx
                .background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_id, db).await?;
                    // `INFO everything` (7+) → `INFO all` → plain `INFO`.
                    // Unknown sections reply with an empty string instead of
                    // an error, so "empty" is the degrade signal.
                    for section in ["everything", "all", ""] {
                        let mut c = cmd("INFO");
                        if !section.is_empty() {
                            c.arg(section);
                        }
                        let (servers, list): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                        if list.iter().any(|text| !text.trim().is_empty()) {
                            return Ok(servers
                                .iter()
                                .zip(list)
                                .map(|(srv, text)| (format!("{}:{}", srv.host, srv.port), text))
                                .collect());
                        }
                    }
                    Ok(Vec::new())
                })
                .await;

            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(replies) => {
                        this.rows = build_rows(&replies);
                        this.refreshed_at = Some(Local::now().format("%H:%M:%S").to_string().into());
                    }
                    Err(e) => {
                        error!(error = %e, "INFO fetch failed");
                        this.error = Some(e.to_string().into());
                    }
                }
                this.loading = false;
                this.apply_filter(cx);
            });
        }));
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("server-info-back")
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
            .child(Icon::new(IconName::Info))
            .child(Label::new(i18n_server_info(cx, "title")).text_color(cx.theme().foreground))
            .child(help_popover("server-info-help", i18n_server_info(cx, "help")));

        let stat_item = |cx: &mut Context<Self>, key: &'static str, value: SharedString| {
            h_flex()
                .gap_1()
                .child(
                    Label::new(i18n_server_info(cx, key))
                        .text_color(cx.theme().muted_foreground)
                        .text_sm(),
                )
                .child(Label::new(value).text_sm().font_weight(gpui::FontWeight::MEDIUM))
        };
        let functions = ZedisDivider::new()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .when(self.display_total > 0, |this| {
                        this.child(stat_item(
                            cx,
                            "fields",
                            format!("{}/{}", self.visible_count, self.display_total).into(),
                        ))
                    })
                    .when_some(self.refreshed_at.clone(), |this, at| {
                        this.child(stat_item(cx, "refreshed_at", at))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Input::new(&self.filter_input_state).small().w(px(260.)))
                    .child(
                        Button::new("server-info-snapshot")
                            .outline()
                            .small()
                            .label(i18n_server_info(cx, "snapshot"))
                            .tooltip(i18n_server_info(cx, "snapshot_tooltip"))
                            .disabled(self.rows.is_empty())
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.take_snapshot(cx);
                            })),
                    )
                    .child(
                        Button::new("server-info-export")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::Download))
                            .label(i18n_common(cx, "export"))
                            .disabled(self.visible_count == 0)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.export_csv(cx);
                            })),
                    )
                    .child(
                        Button::new("server-info-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .loading(self.loading)
                            .disabled(self.loading)
                            .label(i18n_server_info(cx, "refresh"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.refresh(cx);
                            })),
                    ),
            );

        h_flex()
            .id("server-info-toolbar")
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
            .child(functions.flex_none())
    }

    /// Pill shown while a snapshot is held: when it was taken, the diff
    /// summary in compare mode, plus the Compare toggle and a discard
    /// button. Lives under the toolbar so the state survives filtering.
    fn render_snapshot_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (taken_at, field_count) = {
            let snapshot = self.server_state.read(cx).info_snapshot()?;
            (snapshot.taken_at.clone(), snapshot.rows.len())
        };
        let theme = cx.theme();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let label: SharedString = rust_i18n::t!(
            "server_info.snapshot_label",
            time = taken_at.as_ref(),
            count = field_count.to_string(),
            locale = locale
        )
        .to_string()
        .into();
        let summary: Option<SharedString> = if self.diff_mode {
            let (changed, added, removed) = self.diff_counts;
            Some(
                rust_i18n::t!(
                    "server_info.diff_summary",
                    changed = changed.to_string(),
                    added = added.to_string(),
                    removed = removed.to_string(),
                    locale = locale
                )
                .to_string()
                .into(),
            )
        } else {
            None
        };
        Some(
            h_flex()
                .mx_4()
                .my_1()
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(Label::new(label).text_xs().text_color(theme.muted_foreground))
                .when_some(summary, |this, s| {
                    this.child(Label::new(s).text_xs().text_color(theme.foreground))
                })
                .child(div().flex_1())
                .child({
                    let mut btn = Button::new("server-info-compare")
                        .xsmall()
                        .label(i18n_server_info(cx, "compare"))
                        .tooltip(i18n_server_info(cx, "compare_tooltip"));
                    btn = if self.diff_mode { btn.primary() } else { btn.outline() };
                    btn.on_click(cx.listener(|this, _, _w, cx| this.toggle_compare(cx)))
                })
                .child(
                    Button::new("server-info-clear-snapshot")
                        .ghost()
                        .xsmall()
                        .label(i18n_server_info(cx, "clear_snapshot"))
                        .on_click(cx.listener(|this, _, _w, cx| this.clear_snapshot(cx))),
                )
                .into_any_element(),
        )
    }
}

impl Render for ZedisServerInfo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.visible_count == 0;
        let snapshot_banner = self.render_snapshot_banner(cx);

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .child(self.render_toolbar(cx))
            .when_some(snapshot_banner, |this, banner| this.child(banner))
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(IconName::CircleX).text_color(cx.theme().danger))
                        .child(Label::new(message).text_sm().text_color(cx.theme().danger)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(is_empty, |this| {
                        // In compare mode an empty table means "nothing
                        // changed", not "no data" — say so.
                        let message = if self.diff_mode && self.has_snapshot(cx) && self.display_total == 0 {
                            i18n_server_info(cx, "diff_no_changes")
                        } else {
                            i18n_server_info(cx, "no_data")
                        };
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Label::new(message).text_color(cx.theme().muted_foreground)),
                        )
                    })
                    .when(!is_empty, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_fields() {
        let text =
            "# Server\r\nredis_version:7.2.5\r\nuptime_in_seconds:12345\r\n\r\n# Clients\r\nconnected_clients:3\r\n";
        let sections = parse_info_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "Server");
        assert_eq!(
            sections[0].1,
            vec![
                ("redis_version".to_string(), "7.2.5".to_string()),
                ("uptime_in_seconds".to_string(), "12345".to_string()),
            ]
        );
        assert_eq!(sections[1].0, "Clients");
        assert_eq!(sections[1].1, vec![("connected_clients".to_string(), "3".to_string())]);
    }

    #[test]
    fn multi_node_rows_carry_node_prefix_and_filter_matches() {
        let replies = vec![
            ("10.0.0.1:6379".to_string(), "# Stats\nsync_full:2\n".to_string()),
            ("10.0.0.2:6379".to_string(), "# Stats\nsync_full:0\n".to_string()),
        ];
        let rows = build_rows(&replies);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].section.as_ref(), "10.0.0.1:6379 · Stats");
        assert_eq!(rows[1].section.as_ref(), "10.0.0.2:6379 · Stats");

        // One filter compares the field across nodes; value matching works too.
        assert_eq!(filter_rows(&rows, "SYNC_FULL").len(), 2);
        assert_eq!(filter_rows(&rows, "10.0.0.2").len(), 1);
        assert_eq!(filter_rows(&rows, "nosuch").len(), 0);
        assert_eq!(filter_rows(&rows, "").len(), 2);
    }

    #[test]
    fn single_node_keeps_plain_section_names() {
        let replies = vec![("127.0.0.1:6379".to_string(), "# Memory\nused_memory:100\n".to_string())];
        let rows = build_rows(&replies);
        assert_eq!(rows[0].section.as_ref(), "Memory");
        assert_eq!(rows[0].field.as_ref(), "used_memory");
        assert_eq!(rows[0].value.as_ref(), "100");
    }
}
