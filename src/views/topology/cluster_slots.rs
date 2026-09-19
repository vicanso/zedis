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

//! Cluster mode, the Slots tab: the slot bar and legend, per-slot statistics,
//! unassigned-slot repair, and live slot migrations.
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    /// One-shot `CLUSTER SLOT-STATS` fetch for the Slots tab — no polling:
    /// slot counters move slowly and the tab has an explicit Refresh. Skipped
    /// when the probe found the subcommand unusable (the section renders the
    /// reason chip instead) and when a fetch is already in flight or done.
    pub(super) fn ensure_slot_stats(&mut self, cx: &mut Context<Self>) {
        if self.cluster_tab != ClusterTab::Slots
            || self.mode != TopologyMode::Cluster
            || self.slot_stats.is_some()
            || self.slot_stats_task.is_some()
        {
            return;
        }
        let state = self.server_state.read(cx);
        if state
            .features()
            .first_unusable(&[ServerCommand::ClusterSlotStats])
            .is_some()
        {
            return;
        }
        let server_id = state.server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = state.db();
        let metric = self.slot_stats_metric;
        self.slot_stats_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_slot_stats(server_id, db, metric).await;
            let _ = this.update(cx, |this, cx| {
                this.slot_stats_task = None;
                match result {
                    Ok(rows) => {
                        this.slot_stats = Some(rows);
                        this.slot_stats_error = None;
                    }
                    Err(e) => this.slot_stats_error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    /// Re-sort server-side: `ORDERBY` runs on the node, so a metric switch
    /// is a re-fetch, not a client-side sort of the key-count top list.
    pub(super) fn set_slot_stats_metric(&mut self, metric: SlotStatMetric, cx: &mut Context<Self>) {
        if self.slot_stats_metric == metric {
            return;
        }
        self.slot_stats_metric = metric;
        self.slot_stats = None;
        self.slot_stats_task = None;
        self.slot_stats_error = None;
        self.ensure_slot_stats(cx);
        cx.notify();
    }

    /// Whether this server moves slots atomically (Valkey 9) instead of
    /// through the app's own `SETSLOT` + `MIGRATE` loop.
    pub(super) fn atomic_migration_supported(&self, cx: &Context<Self>) -> bool {
        self.mode == TopologyMode::Cluster && self.server_state.read(cx).supports(floors::ATOMIC_SLOT_MIGRATION)
    }

    /// Poll `CLUSTER GETSLOTMIGRATIONS` while the Reshard tab is open on a
    /// server that has it. Two seconds: the job list is what tells the user
    /// a migration they just started is alive, and the reply is tiny.
    pub(super) fn ensure_slot_migration_poll(&mut self, cx: &mut Context<Self>) {
        if self.cluster_tab != ClusterTab::Reshard
            || self.slot_migrations_task.is_some()
            || !self.atomic_migration_supported(cx)
        {
            return;
        }
        self.slot_migrations_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let target = this.update(cx, |this, cx| {
                    let state = this.server_state.read(cx);
                    let server_id = state.server_id().to_string();
                    let addrs: Vec<String> = state
                        .nodes_description()
                        .slot_map
                        .masters
                        .iter()
                        .map(|master| master.addr.to_string())
                        .collect();
                    (!server_id.is_empty() && !addrs.is_empty()).then_some((server_id, addrs))
                });
                match target {
                    Ok(Some((server_id, addrs))) => {
                        let migrations = fetch_slot_migrations(&server_id, &addrs).await;
                        if this
                            .update(cx, |this, cx| {
                                this.slot_migrations = migrations;
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        }));
    }

    /// The migrations still running, source side only — the same job shows
    /// up as EXPORT on its source and IMPORT on its target, and listing
    /// both would double every row.
    pub(super) fn active_migrations(&self) -> Vec<&(String, AtomicSlotMigration)> {
        self.slot_migrations
            .iter()
            .filter(|(_, migration)| migration.is_export() && migration.is_active())
            .collect()
    }

    pub(super) fn render_slots_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let slot_map = &desc.slot_map;

        if slot_map.masters.is_empty() && slot_map.owners.is_empty() {
            return Label::new(i18n_topology(cx, "cluster_placeholder"))
                .text_color(muted)
                .into_any_element();
        }

        let unassigned = CLUSTER_HASH_SLOTS.saturating_sub(slot_map.assigned_slots);
        let pct = if CLUSTER_HASH_SLOTS == 0 {
            0
        } else {
            (slot_map.assigned_slots as u64 * 100) / u64::from(CLUSTER_HASH_SLOTS)
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let summary: SharedString = rust_i18n::t!(
            "topology.slots_summary",
            assigned = slot_map.assigned_slots,
            total = CLUSTER_HASH_SLOTS,
            pct = pct,
            masters = slot_map.masters.len(),
            migrations = slot_map.migrations.len(),
            unassigned = unassigned,
            locale = locale
        )
        .to_string()
        .into();

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(Label::new(summary).text_xs().text_color(muted))
                    .child(
                        Button::new("slots-to-load")
                            .ghost()
                            .small()
                            .label(i18n_topology(cx, "goto_load"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.set_cluster_tab(ClusterTab::Load, cx);
                            })),
                    ),
            )
            .child(self.render_slot_bar(slot_map, cx))
            .child(self.render_slot_legend(slot_map, cx))
            .children(self.render_unassigned_repair(slot_map, cx))
            .child(self.render_migrations_list(slot_map, cx))
            .child(self.render_slot_stats_section(slot_map, cx))
            .into_any_element()
    }

    /// "Hot slots" — the top slots by the chosen `CLUSTER SLOT-STATS`
    /// metric, merged across masters. Extended metrics (memory / CPU /
    /// network) exist only when the cluster runs
    /// `cluster-slot-stats-enabled yes` (start-time config), so their sort
    /// buttons unlock from the reply and a hint names the config otherwise.
    pub(super) fn render_slot_stats_section(
        &self,
        slot_map: &ClusterSlotMap,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let title = Label::new(i18n_topology(cx, "slot_stats_title")).font_semibold();

        // Pre-8.2 (or denied): the section stays discoverable, with the
        // probe's verdict as the reason chip.
        if let Some((command, status)) = self
            .server_state
            .read(cx)
            .features()
            .first_unusable(&[ServerCommand::ClusterSlotStats])
        {
            return v_flex()
                .gap_1()
                .child(title)
                .child(unavailable_chip(cx, command, status))
                .into_any_element();
        }
        if let Some(err) = self.slot_stats_error.clone() {
            return v_flex()
                .gap_1()
                .child(title)
                .child(Label::new(err).text_xs().text_color(theme.danger))
                .into_any_element();
        }
        let Some(rows) = &self.slot_stats else {
            return v_flex()
                .gap_1()
                .child(title)
                .child(
                    Label::new(i18n_topology(cx, "slot_stats_loading"))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element();
        };
        let extended = rows.first().is_some_and(SlotStatRow::has_extended_metrics);

        // Metric picker — server-side ORDERBY, so switching re-fetches.
        let metrics: [(SlotStatMetric, &'static str, bool); 5] = [
            (SlotStatMetric::KeyCount, "slot_stats_col_keys", true),
            (SlotStatMetric::MemoryBytes, "slot_stats_col_memory", extended),
            (SlotStatMetric::CpuUsec, "slot_stats_col_cpu", extended),
            (SlotStatMetric::NetworkBytesIn, "slot_stats_col_net_in", extended),
            (SlotStatMetric::NetworkBytesOut, "slot_stats_col_net_out", extended),
        ];
        let mut picker = h_flex().gap_1().items_center().flex_wrap();
        for (metric, key, enabled) in metrics {
            let active = self.slot_stats_metric == metric;
            picker = picker.child(
                Button::new(SharedString::from(format!("slot-stats-m-{}", metric.word())))
                    .xsmall()
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.outline())
                    .label(i18n_topology(cx, key))
                    .disabled(!enabled)
                    .when(!enabled, |b| b.tooltip(i18n_topology(cx, "slot_stats_extended_hint")))
                    .on_click(cx.listener(move |this, _, _w, cx| this.set_slot_stats_metric(metric, cx))),
            );
        }

        let mut section = v_flex().gap_2().child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(title)
                .child(picker),
        );
        if !extended {
            section = section.child(
                Label::new(i18n_topology(cx, "slot_stats_extended_hint"))
                    .text_xs()
                    .text_color(muted),
            );
        }
        if rows.is_empty() {
            return section
                .child(
                    Label::new(i18n_topology(cx, "slot_stats_empty"))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element();
        }

        // Owner color dots come from the same palette as the slot bar, so a
        // hot slot is visually traceable to its segment.
        let color_of = |addr: &str| -> Option<Hsla> {
            slot_map
                .masters
                .iter()
                .find(|m| m.addr.as_str() == addr)
                .map(|m| master_color(m.color_index))
        };
        let num_col = px(96.);
        let header = h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().w(px(56.)).child(
                    Label::new(i18n_topology(cx, "slot_stats_col_slot"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(i18n_topology(cx, "slot_stats_col_node"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                h_flex().w(num_col).justify_end().child(
                    Label::new(i18n_topology(cx, "slot_stats_col_keys"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .when(extended, |this| {
                this.child(
                    h_flex().w(num_col).justify_end().child(
                        Label::new(i18n_topology(cx, "slot_stats_col_memory"))
                            .text_xs()
                            .text_color(muted),
                    ),
                )
                .child(
                    h_flex().w(num_col).justify_end().child(
                        Label::new(i18n_topology(cx, "slot_stats_col_cpu"))
                            .text_xs()
                            .text_color(muted),
                    ),
                )
                .child(
                    h_flex().w(num_col).justify_end().child(
                        Label::new(i18n_topology(cx, "slot_stats_col_net_in"))
                            .text_xs()
                            .text_color(muted),
                    ),
                )
                .child(
                    h_flex().w(num_col).justify_end().child(
                        Label::new(i18n_topology(cx, "slot_stats_col_net_out"))
                            .text_xs()
                            .text_color(muted),
                    ),
                )
            });

        let stripe_bg = theme.table_even;
        let mut list = v_flex()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .overflow_hidden()
            .child(header);
        let bytes = |v: Option<u64>| -> SharedString {
            v.map(|v| humansize::format_size(v, humansize::DECIMAL).into())
                .unwrap_or_else(|| "—".into())
        };
        for (ix, row) in rows.iter().enumerate() {
            let dot = color_of(&row.node);
            list = list.child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .when(ix % 2 != 0, |this| this.bg(stripe_bg))
                    .child(div().w(px(56.)).child(Label::new(row.slot.to_string()).text_xs()))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .when_some(dot, |this, color| {
                                this.child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
                            })
                            .child(Label::new(row.node.clone()).text_xs().text_color(muted).truncate()),
                    )
                    .child(
                        h_flex()
                            .w(num_col)
                            .justify_end()
                            .child(Label::new(row.key_count.to_string()).text_xs().font_semibold()),
                    )
                    .when(extended, |this| {
                        this.child(
                            h_flex()
                                .w(num_col)
                                .justify_end()
                                .child(Label::new(bytes(row.memory_bytes)).text_xs()),
                        )
                        .child(
                            h_flex().w(num_col).justify_end().child(
                                Label::new(match row.cpu_usec {
                                    Some(v) => SharedString::from(format!("{v} µs")),
                                    None => "—".into(),
                                })
                                .text_xs(),
                            ),
                        )
                        .child(
                            h_flex()
                                .w(num_col)
                                .justify_end()
                                .child(Label::new(bytes(row.network_bytes_in)).text_xs()),
                        )
                        .child(
                            h_flex()
                                .w(num_col)
                                .justify_end()
                                .child(Label::new(bytes(row.network_bytes_out)).text_xs()),
                        )
                    }),
            );
        }
        section.child(list).into_any_element()
    }

    pub(super) fn render_slot_bar(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        // Build proportional flex children. Unassigned gaps use a faint fill.
        let mut segments: Vec<(u32, Option<usize>, String)> = Vec::new(); // width, color_idx, label
        let mut cursor: u32 = 0;
        for owner in &slot_map.owners {
            let start = u32::from(owner.start);
            if start > cursor {
                segments.push((start - cursor, None, "unassigned".into()));
            }
            let width = u32::from(owner.end.saturating_sub(owner.start).saturating_add(1));
            let label = format!(
                "{}:{} ({}-{})",
                owner.addr,
                owner.node_id.chars().take(8).collect::<String>(),
                owner.start,
                owner.end
            );
            segments.push((width, Some(owner.color_index), label));
            cursor = u32::from(owner.end).saturating_add(1);
        }
        if cursor < CLUSTER_HASH_SLOTS {
            segments.push((CLUSTER_HASH_SLOTS - cursor, None, "unassigned".into()));
        }

        let mut bar = h_flex()
            .w_full()
            .h(px(28.))
            .rounded_md()
            .border_1()
            .border_color(border)
            .overflow_hidden();

        for (i, (width, color_idx, _label)) in segments.into_iter().enumerate() {
            if width == 0 {
                continue;
            }
            let bg = color_idx.map(master_color).unwrap_or_else(|| {
                let mut c = muted;
                c.a = 0.15;
                c
            });
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("slot-seg-{i}")))
                    .h_full()
                    .flex_grow(width as f32)
                    .bg(bg),
            );
        }

        bar.into_any_element()
    }

    pub(super) fn render_slot_legend(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().table_hover;
        let border = cx.theme().border;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut chips: Vec<gpui::AnyElement> = Vec::new();
        for m in &slot_map.masters {
            let color = master_color(m.color_index);
            let short_id = short_node_id(&m.node_id);
            let id_for_fill = m.node_id.clone();
            let slots_label =
                rust_i18n::t!("topology.slots_count_label", count = m.slot_count, locale = locale).to_string();
            chips.push(
                h_flex()
                    .id(SharedString::from(format!("legend-{}", m.node_id)))
                    .gap_1()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Click legend → fill reshard target and jump to Reshard.
                        this.fill_reshard_target(id_for_fill.clone().into(), window, cx);
                        this.set_cluster_tab(ClusterTab::Reshard, cx);
                    }))
                    .child(div().w(px(10.)).h(px(10.)).rounded_full().bg(color))
                    .child(Label::new(m.addr.clone()).text_xs())
                    .child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                    .child(Label::new(SharedString::from(slots_label)).text_xs().text_color(muted))
                    .into_any_element(),
            );
        }
        h_flex().gap_2().flex_wrap().children(chips).into_any_element()
    }

    /// The slots nobody owns, and the one command that fixes them. A
    /// cluster with a gap answers `cluster_state:fail` and refuses every
    /// key in it, so this is an error state, not a statistic — hence the
    /// warning frame and its place right under the slot bar.
    pub(super) fn render_unassigned_repair(
        &self,
        slot_map: &ClusterSlotMap,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let gaps = unassigned_slot_ranges(&slot_map.owners);
        if gaps.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (warning, danger, radius) = (theme.warning, theme.danger, theme.radius);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let count: u32 = gaps.iter().map(|(lo, hi)| u32::from(hi.saturating_sub(*lo)) + 1).sum();
        // Enough ranges to recognise the shape of the hole, not all of them.
        let shown: Vec<String> = gaps
            .iter()
            .take(6)
            .map(|(lo, hi)| if lo == hi { lo.to_string() } else { format!("{lo}-{hi}") })
            .collect();
        let ranges = if gaps.len() > shown.len() {
            format!("{}, …", shown.join(", "))
        } else {
            shown.join(", ")
        };
        let body: SharedString = rust_i18n::t!(
            "topology.slots_unassigned_body",
            count = count,
            ranges = ranges,
            locale = &locale
        )
        .to_string()
        .into();

        let mut block = v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(radius)
            .border_1()
            .border_color(warning)
            .bg(warning.opacity(0.1))
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(IconName::TriangleAlert).text_color(warning))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                Label::new(i18n_topology(cx, "slots_unassigned_title"))
                                    .text_sm()
                                    .text_color(warning),
                            )
                            .child(Label::new(body).text_xs().whitespace_normal()),
                    ),
            );
        if self.can_cluster_write(cx) {
            block = block.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Label::new(i18n_topology(cx, "slots_assign_label")).text_sm())
                    .child(Input::new(&self.addslots_target_input).w(px(260.)))
                    .child(
                        Button::new("topo-addslots")
                            .outline()
                            .small()
                            .label(i18n_topology(cx, "slots_assign_button"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let addr = this.addslots_target_input.read(cx).value().trim().to_string();
                                if parse_host_port(&addr).is_none() {
                                    this.form_error = Some(i18n_topology(cx, "slots_err_assign_target"));
                                    cx.notify();
                                    return;
                                }
                                this.form_error = None;
                                this.open_addslots_dialog(addr.into(), window, cx);
                            })),
                    ),
            );
            if let Some(error) = self.form_error.clone() {
                block = block.child(Label::new(error).text_xs().text_color(danger));
            }
        }
        Some(block.into_any_element())
    }

    /// `SETSLOT … STABLE` behind the alert dialog: what it settles, and
    /// what it cannot bring back.
    pub(super) fn open_stabilize_dialog(
        &mut self,
        slot: u16,
        ends: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "slots_stabilize_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!("topology.slots_stabilize_confirm_body", slot = slot, locale = &locale).to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let ends = ends.clone();
                server_state.update(cx, |state, cx| state.cluster_stabilize_slot(slot, ends, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// `CLUSTER ADDSLOTS` behind the alert dialog. The slot list is taken
    /// at confirm time, not at click time, so a heartbeat that repaired
    /// the coverage in between cannot make this claim slots that now have
    /// an owner.
    pub(super) fn open_addslots_dialog(
        &mut self,
        target_addr: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot_map = self.server_state.read(cx).nodes_description().slot_map.clone();
        let slots = slots_in_ranges(&unassigned_slot_ranges(&slot_map.owners));
        if slots.is_empty() {
            return;
        }
        let title = i18n_topology(cx, "slots_assign_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.slots_assign_confirm_body",
            count = slots.len(),
            addr = target_addr.as_ref(),
            locale = &locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let target = target_addr.clone();
                let slots = slots.clone();
                server_state.update(cx, |state, cx| state.cluster_add_slots(target, slots, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    pub(super) fn render_migrations_list(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let title = i18n_topology(cx, "slots_migrations");
        if slot_map.migrations.is_empty() {
            return v_flex()
                .gap_1()
                .child(Label::new(title).font_semibold())
                .child(
                    Label::new(i18n_topology(cx, "slots_no_migrations"))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element();
        }
        let can_write = self.can_cluster_write(cx);
        let stabilize_label = i18n_topology(cx, "slots_stabilize_button");
        let stabilize_tooltip = i18n_topology(cx, "slots_stabilize_tooltip");
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for m in &slot_map.migrations {
            let src = if m.source_addr.is_empty() {
                m.source_id.to_string()
            } else {
                m.source_addr.to_string()
            };
            let tgt = if m.target_addr.is_empty() {
                m.target_id.to_string()
            } else {
                m.target_addr.to_string()
            };
            // Both ends, so `SETSLOT … STABLE` reaches whichever of them
            // the map could pair.
            let ends: Vec<SharedString> = [m.source_addr.clone(), m.target_addr.clone()]
                .into_iter()
                .filter(|addr| !addr.is_empty())
                .map(SharedString::from)
                .collect();
            let slot = m.slot;
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("topo-migration-{slot}")))
                    .items_center()
                    .gap_2()
                    .child(Label::new(SharedString::from(format!("slot {slot} · {src} → {tgt}"))).text_xs())
                    .child(div().flex_1())
                    // A migration that never finished sits here forever:
                    // the cluster keeps serving the slot through ASK
                    // redirects but never settles it.
                    .when(can_write && !ends.is_empty(), |this| {
                        this.child(
                            Button::new(SharedString::from(format!("topo-stabilize-{slot}")))
                                .ghost()
                                .small()
                                .label(stabilize_label.clone())
                                .tooltip(stabilize_tooltip.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_stabilize_dialog(slot, ends.clone(), window, cx);
                                })),
                        )
                    })
                    .into_any_element(),
            );
        }
        v_flex()
            .gap_1()
            .child(Label::new(title).font_semibold())
            .children(rows)
            .into_any_element()
    }

    /// Atomic migrations the servers are running right now. Absent on a
    /// cluster without them, and while none are in flight.
    pub(super) fn render_slot_migrations(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let active = self.active_migrations();
        if active.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (muted, border, radius) = (theme.muted_foreground, theme.border, theme.radius);
        let can_write = self.can_cluster_write(cx);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        // `CANCELSLOTMIGRATIONS` aborts every job a node started, so the
        // control is per source node, not per row.
        let mut sources: Vec<SharedString> = active
            .iter()
            .map(|(addr, _)| SharedString::from(addr.clone()))
            .collect();
        sources.sort();
        sources.dedup();

        let rows: Vec<gpui::AnyElement> = active
            .iter()
            .map(|(addr, migration)| {
                let mut line = format!(
                    "{} · {} → {} · {}",
                    migration.slot_ranges, addr, migration.target_node, migration.state
                );
                if migration.remaining_repl_size > 0 {
                    line.push_str(&format!(
                        " · {}",
                        rust_i18n::t!(
                            "topology.migrations_remaining",
                            size = format_lag_bytes(migration.remaining_repl_size as i64),
                            locale = &locale
                        )
                    ));
                }
                Label::new(SharedString::from(line)).text_xs().into_any_element()
            })
            .collect();

        Some(
            v_flex()
                .w_full()
                .gap_1()
                .p_3()
                .rounded(radius)
                .border_1()
                .border_color(border)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(i18n_topology(cx, "migrations_title")).font_semibold())
                        .child(
                            Label::new(SharedString::from(active.len().to_string()))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(div().flex_1())
                        .when(can_write, |this| {
                            this.child(
                                Button::new("topo-cancel-migrations")
                                    .outline()
                                    .small()
                                    .label(i18n_topology(cx, "migrations_cancel"))
                                    .tooltip(i18n_topology(cx, "migrations_cancel_tooltip"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_cancel_migrations_dialog(sources.clone(), window, cx);
                                    })),
                            )
                        }),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// `CLUSTER CANCELSLOTMIGRATIONS` behind the alert dialog: it aborts
    /// every job the named nodes started, not one row.
    pub(super) fn open_cancel_migrations_dialog(
        &mut self,
        sources: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "migrations_cancel_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.migrations_cancel_confirm_body",
            nodes = sources.len(),
            locale = &locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let sources = sources.clone();
                server_state.update(cx, |state, cx| state.cluster_cancel_slot_migrations(sources, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}
