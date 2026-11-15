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

//! Stream XINFO summary, consumer-group management cards, and interactive
//! XPENDING pending-entries table for [`ZedisStreamEditor`].

use super::ZedisStreamEditor;
use crate::{
    assets::CustomIconName,
    connection::Capability,
    helpers::{fast_contains_ignore_case, format_duration},
    states::{StreamInfoData, i18n_stream_editor},
};
use gpui::{App, Context, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    label::Label,
    scroll::ScrollableElement,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::sync::Arc;
use std::time::Duration;

/// A group's lag as the table shows it: "—" where the server gives none,
/// never a 0 that would read as "caught up".
fn lag_cell(lag: Option<i64>) -> SharedString {
    lag.map_or_else(|| "—".into(), |lag| lag.to_string().into())
}

/// Whether a pending entry matches the optional group filter and keyword search
/// (matched case-insensitively against entry ID, consumer name, or group name).
pub(super) fn matches_pending_filter(
    group: &str,
    entry_id: &str,
    consumer: &str,
    group_filter: Option<&str>,
    keyword: &str,
) -> bool {
    if let Some(target_group) = group_filter
        && group != target_group
    {
        return false;
    }
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return true;
    }
    let needle = trimmed.to_lowercase();
    fast_contains_ignore_case(entry_id, &needle)
        || fast_contains_ignore_case(consumer, &needle)
        || fast_contains_ignore_case(group, &needle)
}

/// A lightweight `TableDelegate` backed by static column names and row data.
/// Used to render Stream XINFO tables (groups, consumers, pending entries).
pub(super) struct SimpleTableDelegate {
    column_names: Vec<SharedString>,
    columns: Vec<Column>,
    rows: Vec<Vec<SharedString>>,
}

impl SimpleTableDelegate {
    /// `cols` is a list of `(name, width)` pairs; pass `None` for flexible width.
    fn new(cols: Vec<(SharedString, Option<f32>)>, rows: Vec<Vec<SharedString>>) -> Self {
        let column_names = cols.iter().map(|(n, _)| n.clone()).collect();
        let columns = cols
            .iter()
            .map(|(n, w)| {
                let col = Column::new(n.clone(), n.clone());
                if let Some(w) = w { col.width(*w) } else { col }
            })
            .collect();
        Self {
            column_names,
            columns,
            rows,
        }
    }
}

impl TableDelegate for SimpleTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl gpui::IntoElement {
        let name = self.column_names.get(col_ix).cloned().unwrap_or_default();
        h_flex()
            .size_full()
            .px_2()
            .child(Label::new(name).text_sm().text_color(cx.theme().muted_foreground))
    }
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) -> impl gpui::IntoElement {
        let value = self
            .rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .unwrap_or_default();
        h_flex()
            .size_full()
            .px_2()
            .child(Label::new(value).text_xs().text_ellipsis())
    }
    fn has_more(&self, _: &App) -> bool {
        false
    }
}

impl ZedisStreamEditor {
    /// Rebuilds the two info DataTables from freshly-fetched `StreamInfoData`.
    pub(super) fn rebuild_info_tables(
        &mut self,
        info: &Arc<StreamInfoData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Merged groups + consumers table.
        // One row per consumer; groups without consumers get a single placeholder row.
        // Columns: Group | Last Delivered ID | Lag | Consumer | Pending | Idle
        let gc_columns = vec![
            (i18n_stream_editor(cx, "col_group"), Some(140.)),
            (i18n_stream_editor(cx, "col_last_delivered_id"), Some(160.)),
            (i18n_stream_editor(cx, "col_lag"), Some(60.)),
            (i18n_stream_editor(cx, "col_consumer"), Some(140.)),
            (i18n_stream_editor(cx, "col_pending"), Some(80.)),
            (i18n_stream_editor(cx, "col_idle_ms"), None),
        ];
        let gc_rows: Vec<Vec<SharedString>> = info
            .groups
            .iter()
            .flat_map(|g| {
                if g.consumers.is_empty() {
                    vec![vec![
                        g.name.clone(),
                        g.last_delivered_id.clone(),
                        lag_cell(g.lag),
                        "—".into(),
                        "—".into(),
                        "—".into(),
                    ]]
                } else {
                    g.consumers
                        .iter()
                        .map(|c| {
                            vec![
                                g.name.clone(),
                                g.last_delivered_id.clone(),
                                lag_cell(g.lag),
                                c.name.clone(),
                                c.pending.to_string().into(),
                                format_duration(Duration::from_millis(c.idle_ms as u64)).into(),
                            ]
                        })
                        .collect()
                }
            })
            .collect();
        self.groups_consumers_table =
            Some(cx.new(|cx| TableState::new(SimpleTableDelegate::new(gc_columns, gc_rows), window, cx)));
    }

    fn render_metric(&self, label: SharedString, value: SharedString, muted: gpui::Hsla) -> impl gpui::IntoElement {
        v_flex()
            .gap_0p5()
            .child(Label::new(label).text_xs().text_color(muted))
            .child(Label::new(value).text_sm())
    }

    pub(super) fn render_info_view(&self, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        let stream_value = self
            .server_state
            .read(cx)
            .value()
            .and_then(|v| v.stream_value())
            .cloned();

        let stream_info = stream_value.as_ref().and_then(|sv| sv.info.clone());

        let base = v_flex().size_full().overflow_y_scrollbar().p_4().gap_4();

        let Some(info) = stream_info else {
            return base
                .child(
                    Label::new(i18n_stream_editor(cx, "loading"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        };

        // ── Stream summary card ───────────────────────────────────────────────
        let length = stream_value.as_ref().map_or(0, |sv| sv.size);
        let summary = info.summary.clone();
        // What XSETID actually changes. Falls back to the last entry's id
        // on a server whose XINFO does not report it.
        let last_generated_id: SharedString = summary
            .as_ref()
            .map(|s| {
                if s.last_generated_id.is_empty() {
                    s.last_entry_id.clone()
                } else {
                    s.last_generated_id.clone()
                }
            })
            .unwrap_or_default();

        let summary_card = v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .child(
                Label::new(i18n_stream_editor(cx, "stream_summary_title"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                h_flex()
                    .gap_6()
                    .flex_wrap()
                    .child(self.render_metric(i18n_stream_editor(cx, "col_length"), length.to_string().into(), muted))
                    .when_some(summary.as_ref(), |this, s| {
                        this.child(self.render_metric(
                            i18n_stream_editor(cx, "col_groups_count"),
                            s.groups_count.to_string().into(),
                            muted,
                        ))
                    })
                    .when_some(summary.as_ref(), |this, s| {
                        this.child(self.render_metric(
                            i18n_stream_editor(cx, "col_first_entry_id"),
                            s.first_entry_id.clone(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_last_entry_id"),
                            s.last_entry_id.clone(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_radix_tree_nodes"),
                            s.radix_tree_nodes.to_string().into(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_radix_tree_keys"),
                            s.radix_tree_keys.to_string().into(),
                            muted,
                        ))
                    }),
            );

        let mut result = base.child(summary_card);

        // ── Idempotent producer (IDMP, Redis 8.6+) ───────────────────────────
        // Presence-gated on the XINFO reply — shown only when the server
        // reports the counters, so no version check is needed.
        if let Some(idmp) = summary.as_ref().and_then(|s| s.idmp.clone()) {
            result = result.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_stream_editor(cx, "idmp_title"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_6()
                            .flex_wrap()
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_pids"),
                                idmp.pids_tracked.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_iids"),
                                idmp.iids_tracked.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_added"),
                                idmp.iids_added.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_duplicates"),
                                idmp.iids_duplicates.to_string().into(),
                                muted,
                            )),
                    ),
            );
        }

        // ── Manage groups: header + create, then per-group actions ───────────
        let can_mutate = self.server_state.read(cx).can(Capability::MutateContainer);
        if can_mutate || !info.groups.is_empty() {
            result = result.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        Label::new(i18n_stream_editor(cx, "manage_groups_title"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .when(can_mutate, |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("stream-setid-stream")
                                        .small()
                                        .icon(IconName::Asterisk)
                                        .label(i18n_stream_editor(cx, "setid_stream"))
                                        .tooltip(i18n_stream_editor(cx, "setid_stream_tooltip"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.set_stream_id_dialog(last_generated_id.clone(), window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("stream-trim")
                                        .small()
                                        .icon(Icon::new(CustomIconName::ListX))
                                        .label(i18n_stream_editor(cx, "trim"))
                                        .tooltip(i18n_stream_editor(cx, "trim_tooltip"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.trim_stream_dialog(window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("stream-create-group")
                                        .small()
                                        .icon(IconName::Plus)
                                        .label(i18n_stream_editor(cx, "create_group"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.create_group_dialog(window, cx);
                                        })),
                                ),
                        )
                    }),
            );
        }

        let can_create_consumer = can_mutate && self.server_state.read(cx).supports_stream_create_consumer();
        for (idx, g) in info.groups.iter().enumerate() {
            let setid_group = g.name.clone();
            let autoclaim_group = g.name.clone();
            let destroy_group = g.name.clone();
            let create_consumer_group = g.name.clone();
            let delete_consumer_group = g.name.clone();
            let group_consumers: Vec<(SharedString, u64)> =
                g.consumers.iter().map(|c| (c.name.clone(), c.pending as u64)).collect();
            let consumer_names: Vec<SharedString> = g.consumers.iter().map(|c| c.name.clone()).collect();
            let group_pending_ids: Vec<SharedString> = g.pending_entries.iter().map(|p| p.id.clone()).collect();
            let ack_group = g.name.clone();
            result = result.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .border_1()
                    .border_color(border)
                    .rounded(cx.theme().radius)
                    .child(Label::new(g.name.clone()).text_sm())
                    .child(Label::new(g.last_delivered_id.clone()).text_xs().text_color(muted))
                    .child(div().flex_1())
                    // Batch XACK for all loaded pending entries of this group.
                    .when(can_mutate && !group_pending_ids.is_empty(), |this| {
                        let ack_group = ack_group.clone();
                        let ids = group_pending_ids.clone();
                        this.child(
                            Button::new(("stream-ack-group", idx))
                                .small()
                                .ghost()
                                .icon(IconName::Check)
                                .tooltip(i18n_stream_editor(cx, "ack_group_tooltip"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.server_state.update(cx, |state, cx| {
                                        state.ack_stream_entries(ack_group.clone(), ids.clone(), cx);
                                    });
                                })),
                        )
                    })
                    // An empty consumer, visible in XINFO before it reads.
                    .when(can_create_consumer, |this| {
                        let create_consumer_group = create_consumer_group.clone();
                        this.child(
                            Button::new(("stream-create-consumer", idx))
                                .small()
                                .ghost()
                                .icon(Icon::new(CustomIconName::UserRoundPlus))
                                .tooltip(i18n_stream_editor(cx, "create_consumer_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.create_consumer_dialog(create_consumer_group.clone(), window, cx);
                                })),
                        )
                    })
                    // Only where there is a consumer to remove; an empty
                    // group has nothing for the picker to offer.
                    .when(can_mutate && !g.consumers.is_empty(), |this| {
                        let group = delete_consumer_group.clone();
                        let consumers = group_consumers.clone();
                        this.child(
                            Button::new(("stream-delete-consumer", idx))
                                .small()
                                .ghost()
                                .icon(IconName::Delete)
                                .tooltip(i18n_stream_editor(cx, "delete_consumer_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete_consumer_dialog(group.clone(), consumers.clone(), window, cx);
                                })),
                        )
                    })
                    .when(can_mutate, |this| {
                        let consumers = consumer_names.clone();
                        this.child(
                            Button::new(("stream-setid", idx))
                                .small()
                                .ghost()
                                .icon(IconName::Asterisk)
                                .tooltip(i18n_stream_editor(cx, "setid_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.setid_group_dialog(setid_group.clone(), window, cx);
                                })),
                        )
                        .child(
                            Button::new(("stream-autoclaim", idx))
                                .small()
                                .ghost()
                                .icon(Icon::new(CustomIconName::ListCheck))
                                .tooltip(i18n_stream_editor(cx, "autoclaim_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.autoclaim_dialog(autoclaim_group.clone(), consumers.clone(), window, cx);
                                })),
                        )
                        .child(
                            Button::new(("stream-destroy", idx))
                                .small()
                                .danger()
                                .icon(IconName::Close)
                                .tooltip(i18n_stream_editor(cx, "destroy_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_destroy_group(destroy_group.clone(), window, cx);
                                })),
                        )
                    }),
            );
        }

        if info.groups.is_empty() {
            return result
                .child(
                    Label::new(i18n_stream_editor(cx, "no_consumer_groups"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        }

        if let Some(ref t) = self.groups_consumers_table {
            result = result
                .child(
                    Label::new(i18n_stream_editor(cx, "consumer_groups_title"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(160.))
                        .border_1()
                        .border_color(border)
                        .rounded_lg()
                        .overflow_hidden()
                        .child(DataTable::new(t).stripe(true).bordered(false)),
                );
        }

        result = result
            .child(self.render_pending_toolbar(&info, muted, cx))
            .child(self.render_pending_entries(&info, muted, border, cx));

        result.into_any_element()
    }

    /// Toolbar above the Pending Entries list: section title, optional group
    /// filter pills, search input, and a batch "Ack shown" action.
    fn render_pending_toolbar(
        &self,
        info: &Arc<StreamInfoData>,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement + use<> {
        let keyword = self.pending_filter_input.read(cx).value().to_string();
        let active_group = self.pending_group_filter.as_deref();
        let shown_batches: Vec<(SharedString, Vec<SharedString>)> = info
            .groups
            .iter()
            .filter_map(|g| {
                let ids: Vec<SharedString> = g
                    .pending_entries
                    .iter()
                    .filter(|p| {
                        matches_pending_filter(
                            g.name.as_ref(),
                            p.id.as_ref(),
                            p.consumer.as_ref(),
                            active_group,
                            &keyword,
                        )
                    })
                    .map(|p| p.id.clone())
                    .collect();
                (!ids.is_empty()).then(|| (g.name.clone(), ids))
            })
            .collect();
        let has_shown = !shown_batches.is_empty();

        let mut left = h_flex().gap_1p5().items_center().flex_wrap().child(
            Label::new(i18n_stream_editor(cx, "pending_entries_title"))
                .text_xs()
                .text_color(muted),
        );

        if info.groups.len() > 1 {
            let all_active = self.pending_group_filter.is_none();
            left = left.child(
                Button::new("stream-pending-group-all")
                    .xsmall()
                    .when(all_active, |b| b.primary())
                    .when(!all_active, |b| b.outline())
                    .label(i18n_stream_editor(cx, "pending_all_groups"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.pending_group_filter = None;
                        cx.notify();
                    })),
            );
            for (idx, g) in info.groups.iter().enumerate() {
                let is_active = self.pending_group_filter.as_ref() == Some(&g.name);
                let group_name = g.name.clone();
                left = left.child(
                    Button::new(("stream-pending-group", idx))
                        .xsmall()
                        .when(is_active, |b| b.primary())
                        .when(!is_active, |b| b.outline())
                        .label(format!("{} ({})", g.name, g.pending_count))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.pending_group_filter = Some(group_name.clone());
                            cx.notify();
                        })),
                );
            }
        }

        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .child(left)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(220.))
                            .child(Input::new(&self.pending_filter_input).small().cleanable(true)),
                    )
                    .when(
                        has_shown && self.server_state.read(cx).can(Capability::MutateContainer),
                        |this| {
                            let batches = shown_batches.clone();
                            this.child(
                                Button::new("stream-ack-shown")
                                    .small()
                                    .outline()
                                    .icon(IconName::Check)
                                    .label(i18n_stream_editor(cx, "ack_shown"))
                                    .tooltip(i18n_stream_editor(cx, "ack_shown_tooltip"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.server_state.update(cx, |state, cx| {
                                            for (group, ids) in batches.iter() {
                                                state.ack_stream_entries(group.clone(), ids.clone(), cx);
                                            }
                                        });
                                    })),
                            )
                        },
                    ),
            )
    }

    /// Pending entries across every group, as interactive rows: per-row
    /// Inspect / XACK / XCLAIM, plus a per-group "load more" while XPENDING pages
    /// remain. Hand-rolled instead of a `DataTable` because the table
    /// component has no per-row action slot (same reason as the
    /// workspace tab strip).
    fn render_pending_entries(
        &self,
        info: &Arc<StreamInfoData>,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement + use<> {
        let keyword = self.pending_filter_input.read(cx).value().to_string();
        let active_group = self.pending_group_filter.as_deref();

        // Header row mirroring the old table columns; the trailing gap
        // roughly reserves the per-row action-button width.
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .child(
                Label::new(i18n_stream_editor(cx, "col_group"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(120.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_entry_id"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(150.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_consumer"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(120.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_idle_ms"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(80.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_deliveries"))
                    .text_xs()
                    .text_color(muted),
            );

        let mut list = v_flex().w_full();
        let can_mutate = self.server_state.read(cx).can(Capability::MutateContainer);
        let can_ackdel = can_mutate && self.server_state.read(cx).supports_stream_ref_policies();
        let can_nack = can_mutate && self.server_state.read(cx).supports_stream_nack();
        let mut row_ix = 0usize;
        let mut matched_rows = 0usize;
        for g in info.groups.iter() {
            if let Some(target_group) = active_group
                && g.name.as_ref() != target_group
            {
                continue;
            }
            let group_consumers: Vec<SharedString> = g.consumers.iter().map(|c| c.name.clone()).collect();
            for p in g.pending_entries.iter() {
                if !matches_pending_filter(
                    g.name.as_ref(),
                    p.id.as_ref(),
                    p.consumer.as_ref(),
                    active_group,
                    &keyword,
                ) {
                    continue;
                }
                matched_rows += 1;
                let inspect_id = p.id.clone();
                let ack_group = g.name.clone();
                let ack_id = p.id.clone();
                let ackdel_group = g.name.clone();
                let ackdel_id = p.id.clone();
                let nack_group = g.name.clone();
                let nack_id = p.id.clone();
                let claim_group = g.name.clone();
                let claim_id = p.id.clone();
                let claim_consumers = group_consumers.clone();
                list = list.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Label::new(g.name.clone())
                                .text_xs()
                                .text_color(muted)
                                .w(px(120.))
                                .text_ellipsis(),
                        )
                        .child(Label::new(p.id.clone()).text_xs().w(px(150.)).text_ellipsis())
                        // A released (XNACKed) entry has no consumer and
                        // reports idle -1 until someone claims it.
                        .child(
                            Label::new(if p.consumer.is_empty() {
                                i18n_stream_editor(cx, "pending_unowned")
                            } else {
                                p.consumer.clone()
                            })
                            .text_xs()
                            .w(px(120.))
                            .text_ellipsis(),
                        )
                        .child(
                            Label::new(match u64::try_from(p.idle_ms) {
                                Ok(ms) => SharedString::from(format_duration(Duration::from_millis(ms))),
                                Err(_) => SharedString::from("-"),
                            })
                            .text_xs()
                            .text_color(muted)
                            .w(px(80.)),
                        )
                        .child(Label::new(p.delivery_count.to_string()).text_xs().text_color(muted))
                        .child(div().flex_1())
                        .child(
                            Button::new(("stream-inspect", row_ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Search)
                                .tooltip(i18n_stream_editor(cx, "inspect_entry_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.jump_to_entry(inspect_id.clone(), window, cx);
                                })),
                        )
                        .when(can_mutate, |this| {
                            let ack_group = ack_group.clone();
                            let ack_id = ack_id.clone();
                            this.child(
                                Button::new(("stream-ack", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Check)
                                    .tooltip(i18n_stream_editor(cx, "ack_tooltip"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.server_state.update(cx, |state, cx| {
                                            state.ack_stream_entry(ack_group.clone(), ack_id.clone(), cx);
                                        });
                                    })),
                            )
                        })
                        // Ack + delete in one atomic XACKDEL (Redis 8.2+ —
                        // hidden elsewhere, incl. Valkey).
                        .when(can_ackdel, |this| {
                            let ackdel_group = ackdel_group.clone();
                            let ackdel_id = ackdel_id.clone();
                            this.child(
                                Button::new(("stream-ackdel", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(CustomIconName::ListX))
                                    .tooltip(i18n_stream_editor(cx, "ackdel_tooltip"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.server_state.update(cx, |state, cx| {
                                            state.ackdel_stream_entry(ackdel_group.clone(), ackdel_id.clone(), cx);
                                        });
                                    })),
                            )
                        })
                        // Release back to the group without acking (XNACK,
                        // Redis 8.8+): the consumer is cleared and the entry
                        // becomes claimable at once.
                        .when(can_nack, |this| {
                            let nack_group = nack_group.clone();
                            let nack_id = nack_id.clone();
                            this.child(
                                Button::new(("stream-nack", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(CustomIconName::Undo2))
                                    .tooltip(i18n_stream_editor(cx, "nack_tooltip"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.server_state.update(cx, |state, cx| {
                                            state.nack_stream_entry(nack_group.clone(), nack_id.clone(), cx);
                                        });
                                    })),
                            )
                        })
                        .when(can_mutate, |this| {
                            let claim_group = claim_group.clone();
                            let claim_id = claim_id.clone();
                            let claim_consumers = claim_consumers.clone();
                            this.child(
                                Button::new(("stream-claim", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::CircleUser)
                                    .tooltip(i18n_stream_editor(cx, "claim_tooltip"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.claim_entry_dialog(
                                            claim_group.clone(),
                                            claim_id.clone(),
                                            claim_consumers.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                        }),
                );
                row_ix += 1;
            }
            if !g.pending_done {
                let more_group = g.name.clone();
                let label: SharedString =
                    format!("{} · {}", i18n_stream_editor(cx, "pending_load_more"), g.name).into();
                list = list.child(
                    div().p_1().child(
                        Button::new(("stream-pending-more", row_ix))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(CustomIconName::ChevronsDown))
                            .label(label)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.load_more_stream_pending(more_group.clone(), cx);
                                });
                            })),
                    ),
                );
                row_ix += 1;
            }
        }

        if matched_rows == 0 {
            list = list.child(
                div().p_2().child(
                    Label::new(i18n_stream_editor(cx, "no_pending_entries"))
                        .text_sm()
                        .text_color(muted),
                ),
            );
        }

        // Fixed height on purpose: `max_h` + a scroll wrapper silently
        // clips instead of scrolling (see the CLAUDE.md gotcha).
        v_flex()
            .w_full()
            .h(px(220.))
            .border_1()
            .border_color(border)
            .rounded_lg()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(list))
    }
}
