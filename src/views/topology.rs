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

//! Cluster / Sentinel topology operations panel.
//!
//! Detects the active server's deployment mode (Cluster / Sentinel /
//! Standalone) from `nodes_description().server_type` and renders
//! per-mode content:
//!   * Cluster: four tabs — **Nodes** (FAILOVER/MEET/FORGET/REPLICATE),
//!     **Slots** (hash-slot map + in-flight migrations), **Load**
//!     (per-master memory/OPS heatmap), **Reshard** (plan + execute
//!     slot moves). Slot ownership comes from the heartbeat
//!     `ClusterSlotMap`; load is polled separately.
//!   * Sentinel: monitored-master list with per-master
//!     `Force Failover` / `Reset` / `Remove` buttons; replica rows
//!     are read-only because Sentinel ops target by master name.
//!   * Standalone / Unknown: localized placeholder text only.
//!
//! All destructive commands route through `ZedisDialog::new_alert`, with
//! the body run through `escalate_dangerous_body` so production-tagged
//! servers get the escalated warning.

use crate::connection::{CLUSTER_HASH_SLOTS, ClusterSlotMap};
use crate::helpers::get_mono_font_family;
use crate::states::{
    ClusterMasterRanges, ClusterNodeLoad, ServerEvent, ZedisServerState, dialog_button_props, escalate_dangerous_body,
    fetch_cluster_node_loads, i18n_topology, plan_cluster_reshard, source_owners_for_slots,
};
use gpui::{Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*, px, rgb};
use gpui_component::{
    ActiveTheme, Disableable, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use std::time::Duration;
use tracing::info;
use zedis_ui::ZedisDialog;

/// Fixed master palette for slot bar + load cards (cycled by color_index).
const MASTER_PALETTE: [u32; 8] = [
    0x4c_8b_f5, // blue
    0x69_b0_83, // green
    0xe5_a5_4b, // amber
    0xe0_6c_75, // red
    0xc6_78_dd, // purple
    0x56_b6_c2, // cyan
    0xd1_9a_66, // orange
    0xab_b2_bf, // grey
];

fn master_color(index: usize) -> Hsla {
    rgb(MASTER_PALETTE[index % MASTER_PALETTE.len()]).into()
}

/// Heat colour: low → green, mid → amber, high → red (ratio in 0..=1).
fn heat_color(ratio: f32) -> Hsla {
    let r = ratio.clamp(0.0, 1.0);
    if r < 0.5 {
        // green → amber
        let t = r * 2.0;
        let g = 0xb0u8;
        let red = (0x69u8 as f32 + t * (0xe5 - 0x69) as f32) as u8;
        let blue = (0x83u8 as f32 * (1.0 - t)) as u8;
        rgb(u32::from_be_bytes([0, red, g, blue])).into()
    } else {
        // amber → red
        let t = (r - 0.5) * 2.0;
        let red = 0xe5u8;
        let green = (0xa5u8 as f32 * (1.0 - t) + 0x6c as f32 * t) as u8;
        let blue = (0x4bu8 as f32 * (1.0 - t) + 0x75 as f32 * t) as u8;
        rgb(u32::from_be_bytes([0, red, green, blue])).into()
    }
}

/// Three deployment shapes Redis exposes — mutually exclusive per
/// connection. `Unknown` is the transient state before the first INFO.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum TopologyMode {
    #[default]
    Unknown,
    Standalone,
    Cluster,
    Sentinel,
}

/// Cluster sub-panel.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum ClusterTab {
    #[default]
    Nodes,
    Slots,
    Load,
    Reshard,
}

pub struct ZedisTopology {
    server_state: Entity<ZedisServerState>,
    mode: TopologyMode,
    cluster_tab: ClusterTab,
    // Nodes tab forms.
    meet_input: Entity<InputState>,
    replicate_target_input: Entity<InputState>,
    replicate_master_input: Entity<InputState>,
    // Reshard wizard inputs.
    reshard_source_input: Entity<InputState>,
    reshard_target_input: Entity<InputState>,
    reshard_count_input: Entity<InputState>,
    planned_slots: Vec<u16>,
    plan_error: Option<SharedString>,
    // Load heatmap.
    node_loads: Vec<ClusterNodeLoad>,
    load_error: Option<SharedString>,
    load_metric: LoadMetric,
    load_poll_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum LoadMetric {
    #[default]
    Memory,
    Ops,
    Clients,
}

impl ZedisTopology {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let meet_input = cx.new(|cx| InputState::new(window, cx).placeholder("host:port"));
        let replicate_target_input = cx.new(|cx| InputState::new(window, cx).placeholder("target host:port"));
        let replicate_master_input = cx.new(|cx| InputState::new(window, cx).placeholder("master node_id"));
        let reshard_source_input = cx.new(|cx| InputState::new(window, cx).placeholder("source node_id (optional)"));
        let reshard_target_input = cx.new(|cx| InputState::new(window, cx).placeholder("target node_id"));
        let reshard_count_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 100"));

        let subscriptions = vec![cx.subscribe(&server_state, |this, _state, event, cx| {
            if matches!(
                event,
                ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
            ) {
                this.detect_mode(cx);
                if this.mode == TopologyMode::Cluster {
                    this.ensure_load_poll(cx);
                }
                cx.notify();
            }
        })];

        let mut this = Self {
            server_state,
            mode: TopologyMode::Unknown,
            cluster_tab: ClusterTab::Nodes,
            meet_input,
            replicate_target_input,
            replicate_master_input,
            reshard_source_input,
            reshard_target_input,
            reshard_count_input,
            planned_slots: Vec::new(),
            plan_error: None,
            node_loads: Vec::new(),
            load_error: None,
            load_metric: LoadMetric::Memory,
            load_poll_task: None,
            _subscriptions: subscriptions,
        };
        this.detect_mode(cx);
        if this.mode == TopologyMode::Cluster {
            this.ensure_load_poll(cx);
        }
        info!("Creating new topology view");
        this
    }

    fn detect_mode(&mut self, cx: &mut Context<Self>) {
        let desc = self.server_state.read(cx).nodes_description();
        self.mode = match desc.server_type.as_ref() {
            "Cluster" => TopologyMode::Cluster,
            "Sentinel" => TopologyMode::Sentinel,
            "Standalone" => TopologyMode::Standalone,
            _ => TopologyMode::Unknown,
        };
    }

    fn ensure_load_poll(&mut self, cx: &mut Context<Self>) {
        if self.load_poll_task.is_some() {
            return;
        }
        self.load_poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let masters = match this.update(cx, |this, cx| {
                    if this.mode != TopologyMode::Cluster {
                        return None;
                    }
                    let desc = this.server_state.read(cx).nodes_description();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    if server_id.is_empty() {
                        return None;
                    }
                    let masters: Vec<(String, String, u32, usize)> = desc
                        .slot_map
                        .masters
                        .iter()
                        .map(|m| (m.node_id.to_string(), m.addr.to_string(), m.slot_count, m.color_index))
                        .collect();
                    Some((server_id, masters))
                }) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        cx.background_executor().timer(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(_) => break,
                };

                let result = if masters.1.is_empty() {
                    Ok(Vec::new())
                } else {
                    fetch_cluster_node_loads(&masters.0, &masters.1).await
                };

                if this
                    .update(cx, |this, cx| {
                        match result {
                            Ok(loads) => {
                                this.node_loads = loads;
                                this.load_error = None;
                            }
                            Err(e) => {
                                this.load_error = Some(e.to_string().into());
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }

    fn set_cluster_tab(&mut self, tab: ClusterTab, cx: &mut Context<Self>) {
        self.cluster_tab = tab;
        if tab == ClusterTab::Load {
            self.ensure_load_poll(cx);
        }
        cx.notify();
    }

    fn render_cluster_tabs(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tabs = [
            (ClusterTab::Nodes, "tab_nodes"),
            (ClusterTab::Slots, "tab_slots"),
            (ClusterTab::Load, "tab_load"),
            (ClusterTab::Reshard, "tab_reshard"),
        ];
        let mut row = h_flex().gap_1().items_center();
        for (tab, key) in tabs {
            let active = self.cluster_tab == tab;
            let label = i18n_topology(cx, key);
            row = row.child(
                Button::new(SharedString::from(format!("topo-tab-{key}")))
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.ghost())
                    .small()
                    .label(label)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_cluster_tab(tab, cx);
                    })),
            );
        }
        row.into_any_element()
    }

    // ── Nodes tab (existing ops) ──────────────────────────────────────

    fn render_cluster_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let body = match self.cluster_tab {
            ClusterTab::Nodes => self.render_nodes_tab(window, cx),
            ClusterTab::Slots => self.render_slots_tab(cx),
            ClusterTab::Load => self.render_load_tab(cx),
            ClusterTab::Reshard => self.render_reshard_tab(window, cx),
        };
        v_flex()
            .gap_3()
            .child(self.render_cluster_tabs(cx))
            .child(body)
            .into_any_element()
    }

    fn render_nodes_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().table_hover;
        let failover_label = i18n_topology(cx, "failover_button");
        let force_failover_label = i18n_topology(cx, "force_failover_button");
        let forget_label = i18n_topology(cx, "forget_button");
        let meet_label = i18n_topology(cx, "meet_button");
        let replicate_label = i18n_topology(cx, "replicate_button");

        let meet_form = h_flex()
            .gap_2()
            .items_center()
            .child(Input::new(&self.meet_input).small().flex_1())
            .child(
                Button::new("topo-meet-btn")
                    .primary()
                    .small()
                    .label(meet_label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let raw = this.meet_input.read(cx).value().to_string();
                        let Some((host, port_str)) = raw.rsplit_once(':') else {
                            return;
                        };
                        let Ok(port) = port_str.parse::<u16>() else {
                            return;
                        };
                        this.open_meet_dialog(SharedString::from(host.to_string()), port, window, cx);
                    })),
            );

        let replicate_form = h_flex()
            .gap_2()
            .items_center()
            .child(Input::new(&self.replicate_target_input).small().flex_1())
            .child(Input::new(&self.replicate_master_input).small().flex_1())
            .child(
                Button::new("topo-replicate-btn")
                    .primary()
                    .small()
                    .label(replicate_label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let target = this.replicate_target_input.read(cx).value().to_string();
                        let master_id = this.replicate_master_input.read(cx).value().to_string();
                        if target.is_empty() || master_id.is_empty() {
                            return;
                        }
                        this.open_replicate_dialog(
                            SharedString::from(target),
                            SharedString::from(master_id),
                            window,
                            cx,
                        );
                    })),
            );

        if desc.topology.is_empty() {
            return v_flex()
                .gap_3()
                .child(meet_form)
                .child(replicate_form)
                .child(Label::new(i18n_topology(cx, "cluster_placeholder")).text_color(muted))
                .into_any_element();
        }

        let summary = SharedString::from(format!(
            "{} · masters {} · replicas {}",
            desc.server_type, desc.master_nodes, desc.slave_nodes
        ));

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for master in desc.topology.iter() {
            let m_addr = master.master.addr.clone();
            let m_node_id = master.master.node_id.clone();
            let m_role = master.master.role_marker.clone();
            let m_annot = master.master.annotation.clone();
            let mut master_row = h_flex()
                .id(SharedString::from(format!("topo-mrow-{m_addr}")))
                .items_center()
                .gap_2()
                .hover(move |s| s.bg(hover))
                .child(Label::new(m_addr.clone()).font_semibold())
                .child(Label::new(m_role).text_xs().text_color(muted))
                .child(Label::new(m_annot).text_xs().text_color(muted))
                .child(div().flex_1());
            if !m_node_id.is_empty() {
                let id_for_click = m_node_id.clone();
                let addr_for_click = m_addr.clone();
                master_row = master_row.child(
                    Button::new(SharedString::from(format!("topo-forget-{m_node_id}")))
                        .ghost()
                        .small()
                        .label(forget_label.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_forget_dialog(
                                id_for_click.clone().into(),
                                addr_for_click.clone().into(),
                                window,
                                cx,
                            );
                        })),
                );
            }
            rows.push(master_row.into_any_element());

            for replica in master.replicas.iter() {
                let r_addr = replica.addr.clone();
                let r_node_id = replica.node_id.clone();
                let r_role = replica.role_marker.clone();
                let r_annot = replica.annotation.clone();
                let failover_target = r_addr.clone();
                let force_failover_target = r_addr.clone();
                let mut replica_row = h_flex()
                    .id(SharedString::from(format!("topo-rrow-{r_addr}")))
                    .items_center()
                    .gap_2()
                    .pl_6()
                    .hover(move |s| s.bg(hover))
                    .child(Label::new(r_addr.clone()).text_color(muted))
                    .child(Label::new(r_role).text_xs().text_color(muted))
                    .child(Label::new(r_annot).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedString::from(format!("topo-failover-{r_addr}")))
                            .ghost()
                            .small()
                            .label(failover_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_failover_dialog(failover_target.clone().into(), false, window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-force-failover-{r_addr}")))
                            .ghost()
                            .small()
                            .label(force_failover_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_failover_dialog(force_failover_target.clone().into(), true, window, cx);
                            })),
                    );
                if !r_node_id.is_empty() {
                    let id_for_click = r_node_id.clone();
                    let addr_for_click = r_addr.clone();
                    replica_row = replica_row.child(
                        Button::new(SharedString::from(format!("topo-forget-{r_node_id}")))
                            .ghost()
                            .small()
                            .label(forget_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_forget_dialog(
                                    id_for_click.clone().into(),
                                    addr_for_click.clone().into(),
                                    window,
                                    cx,
                                );
                            })),
                    );
                }
                rows.push(replica_row.into_any_element());
            }
        }

        v_flex()
            .gap_3()
            .child(Label::new(summary).text_xs().text_color(muted))
            .child(meet_form)
            .child(replicate_form)
            .child(v_flex().gap_2().children(rows))
            .into_any_element()
    }

    // ── Slots tab ─────────────────────────────────────────────────────

    fn render_slots_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let slot_map = &desc.slot_map;

        if slot_map.masters.is_empty() && slot_map.owners.is_empty() {
            return Label::new(i18n_topology(cx, "cluster_placeholder"))
                .text_color(muted)
                .into_any_element();
        }

        let summary = SharedString::from(format!(
            "{}: {} / {CLUSTER_HASH_SLOTS} · {} masters · {} migrations",
            i18n_topology(cx, "slots_title"),
            slot_map.assigned_slots,
            slot_map.masters.len(),
            slot_map.migrations.len()
        ));

        v_flex()
            .gap_3()
            .child(Label::new(summary).text_xs().text_color(muted))
            .child(self.render_slot_bar(slot_map, cx))
            .child(self.render_slot_legend(slot_map, cx))
            .child(self.render_migrations_list(slot_map, cx))
            .into_any_element()
    }

    fn render_slot_bar(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
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

    fn render_slot_legend(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut chips: Vec<gpui::AnyElement> = Vec::new();
        for m in &slot_map.masters {
            let color = master_color(m.color_index);
            let short_id: String = m.node_id.chars().take(8).collect();
            chips.push(
                h_flex()
                    .id(SharedString::from(format!("legend-{}", m.node_id)))
                    .gap_1()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(div().w(px(10.)).h(px(10.)).rounded_full().bg(color))
                    .child(Label::new(m.addr.clone()).text_xs())
                    .child(Label::new(SharedString::from(short_id)).text_xs().text_color(muted))
                    .child(
                        Label::new(SharedString::from(format!("{} slots", m.slot_count)))
                            .text_xs()
                            .text_color(muted),
                    )
                    .into_any_element(),
            );
        }
        h_flex().gap_2().flex_wrap().children(chips).into_any_element()
    }

    fn render_migrations_list(&self, slot_map: &ClusterSlotMap, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            rows.push(
                Label::new(SharedString::from(format!("slot {} · {} → {}", m.slot, src, tgt)))
                    .text_xs()
                    .into_any_element(),
            );
        }
        v_flex()
            .gap_1()
            .child(Label::new(title).font_semibold())
            .children(rows)
            .into_any_element()
    }

    // ── Load tab ──────────────────────────────────────────────────────

    fn render_load_tab(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let metric_row = h_flex()
            .gap_1()
            .child(
                Button::new("load-mem")
                    .when(self.load_metric == LoadMetric::Memory, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Memory, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_mem"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Memory;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("load-ops")
                    .when(self.load_metric == LoadMetric::Ops, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Ops, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_ops"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Ops;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("load-clients")
                    .when(self.load_metric == LoadMetric::Clients, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Clients, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_clients"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Clients;
                        cx.notify();
                    })),
            );

        if let Some(err) = &self.load_error {
            return v_flex()
                .gap_2()
                .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
                .child(metric_row)
                .child(Label::new(err.clone()).text_color(cx.theme().danger))
                .into_any_element();
        }

        if self.node_loads.is_empty() {
            return v_flex()
                .gap_2()
                .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
                .child(metric_row)
                .child(Label::new(i18n_topology(cx, "load_refreshing")).text_color(muted))
                .into_any_element();
        }

        let max_val = self
            .node_loads
            .iter()
            .map(|n| match self.load_metric {
                LoadMetric::Memory => n.used_memory,
                LoadMetric::Ops => n.ops_per_sec,
                LoadMetric::Clients => n.connected_clients,
            })
            .max()
            .unwrap_or(1)
            .max(1);

        let mut cards: Vec<gpui::AnyElement> = Vec::new();
        for n in &self.node_loads {
            let value = match self.load_metric {
                LoadMetric::Memory => n.used_memory,
                LoadMetric::Ops => n.ops_per_sec,
                LoadMetric::Clients => n.connected_clients,
            };
            let ratio = value as f32 / max_val as f32;
            let heat = heat_color(ratio);
            let value_label = match self.load_metric {
                LoadMetric::Memory => humansize::format_size(n.used_memory, humansize::DECIMAL),
                LoadMetric::Ops => format!("{} ops/s", n.ops_per_sec),
                LoadMetric::Clients => format!("{} clients", n.connected_clients),
            };
            let short_id: String = n.node_id.chars().take(8).collect();
            let stripe = master_color(n.color_index);
            cards.push(
                v_flex()
                    .id(SharedString::from(format!("load-card-{}", n.node_id)))
                    .w(px(200.))
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg({
                        let mut c = heat;
                        c.a = 0.25;
                        c
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(stripe))
                            .child(Label::new(n.addr.clone()).font_semibold()),
                    )
                    .child(Label::new(SharedString::from(short_id)).text_xs().text_color(muted))
                    .child(Label::new(SharedString::from(value_label)).text_sm())
                    .child(
                        Label::new(SharedString::from(format!("{} slots", n.slot_count)))
                            .text_xs()
                            .text_color(muted),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
            .child(metric_row)
            .child(h_flex().gap_3().flex_wrap().children(cards))
            .into_any_element()
    }

    // ── Reshard tab ───────────────────────────────────────────────────

    fn render_reshard_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let desc = self.server_state.read(cx).nodes_description();

        // Quick-pick chips for target (and optional source).
        let mut master_chips: Vec<gpui::AnyElement> = Vec::new();
        for m in &desc.slot_map.masters {
            let id = m.node_id.clone();
            let id_as_source = id.clone();
            let id_as_target = id.clone();
            let color = master_color(m.color_index);
            master_chips.push(
                h_flex()
                    .id(SharedString::from(format!("reshard-chip-{}", m.node_id)))
                    .gap_1()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
                    .child(Label::new(m.addr.clone()).text_xs())
                    .child(
                        Button::new(SharedString::from(format!("reshard-src-{}", m.node_id)))
                            .ghost()
                            .small()
                            .label("src")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reshard_source_input.update(cx, |input, cx| {
                                    input.set_value(id_as_source.clone(), window, cx);
                                });
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("reshard-tgt-{}", m.node_id)))
                            .ghost()
                            .small()
                            .label("dst")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reshard_target_input.update(cx, |input, cx| {
                                    input.set_value(id_as_target.clone(), window, cx);
                                });
                            })),
                    )
                    .into_any_element(),
            );
        }

        let plan_btn = Button::new("reshard-plan")
            .primary()
            .small()
            .label(i18n_topology(cx, "reshard_plan"))
            .on_click(cx.listener(|this, _, _window, cx| {
                this.run_plan(cx);
            }));

        let execute_btn = Button::new("reshard-exec")
            .danger()
            .small()
            .label(i18n_topology(cx, "reshard_execute"))
            .disabled(self.planned_slots.is_empty())
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_reshard_dialog(window, cx);
            }));

        let preview: gpui::AnyElement = if let Some(err) = &self.plan_error {
            Label::new(err.clone()).text_color(cx.theme().danger).into_any_element()
        } else if self.planned_slots.is_empty() {
            Label::new(i18n_topology(cx, "reshard_pick_target"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            let preview_slots = if self.planned_slots.len() > 24 {
                let head: Vec<String> = self.planned_slots.iter().take(20).map(|s| s.to_string()).collect();
                format!("{} … (+{} more)", head.join(", "), self.planned_slots.len() - 20)
            } else {
                self.planned_slots
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            v_flex()
                .gap_1()
                .child(
                    Label::new(SharedString::from(format!(
                        "Will move {} slots",
                        self.planned_slots.len()
                    )))
                    .font_semibold(),
                )
                .child(
                    Label::new(SharedString::from(preview_slots))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element()
        };

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "reshard_title")).font_semibold())
            .child(
                Label::new(i18n_topology(cx, "reshard_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(h_flex().gap_2().flex_wrap().children(master_chips))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.reshard_source_input).small().flex_1())
                    .child(Input::new(&self.reshard_target_input).small().flex_1())
                    .child(Input::new(&self.reshard_count_input).small().w(px(100.))),
            )
            .child(h_flex().gap_2().child(plan_btn).child(execute_btn))
            .child(preview)
            .into_any_element()
    }

    fn run_plan(&mut self, cx: &mut Context<Self>) {
        let desc = self.server_state.read(cx).nodes_description();
        let source_raw = self.reshard_source_input.read(cx).value().to_string();
        let target_raw = self.reshard_target_input.read(cx).value().to_string();
        let count_raw = self.reshard_count_input.read(cx).value().to_string();

        let source_id = {
            let t = source_raw.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        let target_id = target_raw.trim().to_string();
        let count: u32 = match count_raw.trim().parse() {
            Ok(n) if n > 0 => n,
            _ => {
                self.plan_error = Some("Slot count must be a positive integer".into());
                self.planned_slots.clear();
                cx.notify();
                return;
            }
        };

        // Expand slot_map masters into (id, ranges) for the planner.
        // Ranges come from owners filtered by node_id.
        let mut by_id: std::collections::HashMap<String, Vec<(u16, u16)>> = std::collections::HashMap::new();
        for o in &desc.slot_map.owners {
            by_id.entry(o.node_id.to_string()).or_default().push((o.start, o.end));
        }
        let masters: Vec<(String, Vec<(u16, u16)>)> = by_id.into_iter().collect();

        match plan_cluster_reshard(&masters, source_id.as_deref(), &target_id, count) {
            Ok(slots) => {
                self.planned_slots = slots;
                self.plan_error = None;
            }
            Err(e) => {
                self.planned_slots.clear();
                self.plan_error = Some(e.into());
            }
        }
        cx.notify();
    }

    fn open_reshard_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.planned_slots.is_empty() {
            return;
        }
        let desc = self.server_state.read(cx).nodes_description();
        let target_id = self.reshard_target_input.read(cx).value().trim().to_string();
        let target_addr = desc
            .slot_map
            .masters
            .iter()
            .find(|m| m.node_id.as_str() == target_id)
            .map(|m| m.addr.to_string())
            .unwrap_or_default();
        if target_addr.is_empty() {
            self.plan_error = Some("Target master not found in current topology".into());
            cx.notify();
            return;
        }

        // Build owner list for source mapping.
        let mut masters_with_addr: Vec<ClusterMasterRanges> = Vec::new();
        for m in &desc.slot_map.masters {
            let ranges: Vec<(u16, u16)> = desc
                .slot_map
                .owners
                .iter()
                .filter(|o| o.node_id == m.node_id)
                .map(|o| (o.start, o.end))
                .collect();
            masters_with_addr.push(ClusterMasterRanges {
                node_id: m.node_id.to_string(),
                addr: m.addr.to_string(),
                ranges,
            });
        }
        let source_by_slot = match source_owners_for_slots(&masters_with_addr, &self.planned_slots) {
            Ok(v) => v,
            Err(e) => {
                self.plan_error = Some(e.into());
                cx.notify();
                return;
            }
        };

        let title = i18n_topology(cx, "reshard_confirm_title");
        let body = format!(
            "Move {} hash slots onto master {target_id} ({target_addr}). \
             Each slot is SETSLOT-migrated and keys are MIGRATEd. \
             This changes cluster slot ownership and may briefly block writes on those slots.",
            self.planned_slots.len()
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let slots = self.planned_slots.clone();
        let target_addr_s: SharedString = target_addr.into();
        let target_id_s: SharedString = target_id.into();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let slots = slots.clone();
                let source_by_slot = source_by_slot.clone();
                let t_addr = target_addr_s.clone();
                let t_id = target_id_s.clone();
                server_state.update(cx, |state, cx| {
                    state.cluster_reshard(t_addr, t_id, slots, source_by_slot, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    // ── Sentinel ──────────────────────────────────────────────────────

    fn render_sentinel_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().table_hover;

        if desc.topology.is_empty() {
            return Label::new(i18n_topology(cx, "sentinel_placeholder"))
                .text_color(muted)
                .into_any_element();
        }

        let summary = SharedString::from(format!(
            "{} · masters {} · replicas {}",
            desc.server_type, desc.master_nodes, desc.slave_nodes
        ));
        let failover_label = i18n_topology(cx, "sentinel_failover_button");
        let reset_label = i18n_topology(cx, "sentinel_reset_button");
        let remove_label = i18n_topology(cx, "sentinel_remove_button");

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for master in desc.topology.iter() {
            let m_addr = master.master.addr.clone();
            let m_name = master.master.master_name.clone();
            let m_role = master.master.role_marker.clone();
            let m_annot = master.master.annotation.clone();
            let mut master_row = h_flex()
                .id(SharedString::from(format!("topo-snt-mrow-{m_addr}")))
                .items_center()
                .gap_2()
                .hover(move |s| s.bg(hover))
                .child(Label::new(m_addr).font_semibold())
                .child(Label::new(m_role).text_xs().text_color(muted))
                .child(Label::new(m_annot).text_xs().text_color(muted))
                .child(div().flex_1());
            if !m_name.is_empty() {
                let name_for_failover = m_name.clone();
                let name_for_reset = m_name.clone();
                let name_for_remove = m_name.clone();
                master_row = master_row
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-failover-{m_name}")))
                            .ghost()
                            .small()
                            .label(failover_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_failover_dialog(name_for_failover.clone().into(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-reset-{m_name}")))
                            .ghost()
                            .small()
                            .label(reset_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_reset_dialog(name_for_reset.clone().into(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-remove-{m_name}")))
                            .ghost()
                            .small()
                            .label(remove_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_remove_dialog(name_for_remove.clone().into(), window, cx);
                            })),
                    );
            }
            rows.push(master_row.into_any_element());

            for replica in master.replicas.iter() {
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("topo-snt-rrow-{}", replica.addr)))
                        .items_center()
                        .gap_2()
                        .pl_6()
                        .hover(move |s| s.bg(hover))
                        .child(Label::new(replica.addr.clone()).text_color(muted))
                        .child(Label::new(replica.role_marker.clone()).text_xs().text_color(muted))
                        .child(Label::new(replica.annotation.clone()).text_xs().text_color(muted))
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .gap_3()
            .child(Label::new(summary).text_xs().text_color(muted))
            .child(v_flex().gap_2().children(rows))
            .into_any_element()
    }

    // ── Confirm dialogs (unchanged behaviour) ─────────────────────────

    fn open_failover_dialog(
        &mut self,
        target_addr: SharedString,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "failover_confirm_title");
        let body = if force {
            format!(
                "Force-promote replica {target_addr} to master, skipping the master-handshake \
                 step. Use only when the existing master is unreachable — risks split-brain if \
                 the old master returns before the cluster reconciles."
            )
        } else {
            format!(
                "Promote replica {target_addr} to master. The existing master will demote to \
                 a replica. This may briefly interrupt writes."
            )
        };
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let addr = target_addr.clone();
                server_state.update(cx, |state, cx| state.cluster_failover(addr, force, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_forget_dialog(
        &mut self,
        node_id: SharedString,
        addr_display: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "forget_confirm_title");
        let body = format!(
            "Drop node {addr_display} (id {node_id}) from the cluster view on all masters. \
             Gossip may re-add it within 60s if any master misses the forget."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let id = node_id.clone();
                server_state.update(cx, |state, cx| state.cluster_forget(id, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_meet_dialog(&mut self, host: SharedString, port: u16, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_topology(cx, "meet_confirm_title");
        let body = format!(
            "Introduce node {host}:{port} to the cluster. Sent to all masters; gossip \
             distributes the new membership within seconds. The new node appears in this \
             table after the next heartbeat refresh."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let h = host.clone();
                server_state.update(cx, |state, cx| state.cluster_meet(h, port, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_replicate_dialog(
        &mut self,
        target_addr: SharedString,
        master_node_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "replicate_confirm_title");
        let body = format!(
            "Make node {target_addr} a replica of master {master_node_id}. The target must be \
             empty and an already-known cluster member, otherwise Redis rejects the command."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let t = target_addr.clone();
                let m = master_node_id.clone();
                server_state.update(cx, |state, cx| state.cluster_replicate(t, m, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_sentinel_failover_dialog(
        &mut self,
        master_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "sentinel_failover_confirm_title");
        let body = format!(
            "Force a failover on master {master_name} across the sentinel quorum. The current \
             master will demote to replica and a healthy replica will be promoted. This skips \
             automatic detection — use when manual intervention is required."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let name = master_name.clone();
                server_state.update(cx, |state, cx| state.sentinel_failover(name, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_sentinel_reset_dialog(&mut self, master_name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_topology(cx, "sentinel_reset_confirm_title");
        let body = format!(
            "Reset sentinel state for master {master_name}. Forces re-discovery of replicas \
             and sentinel peers — useful after a network split healed. Does not affect the \
             data masters themselves."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let pattern = master_name.clone();
                server_state.update(cx, |state, cx| state.sentinel_reset(pattern, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_sentinel_remove_dialog(&mut self, master_name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_topology(cx, "sentinel_remove_confirm_title");
        let body = format!(
            "Stop monitoring master {master_name} across the sentinel quorum. The master and \
             its replicas continue to run, but sentinels will no longer track health or \
             failover for them. Re-adding requires SENTINEL MONITOR (config + quorum)."
        );
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let name = master_name.clone();
                server_state.update(cx, |state, cx| state.sentinel_remove(name, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}

impl Render for ZedisTopology {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = i18n_topology(cx, "title");
        let muted = cx.theme().muted_foreground;

        let body: gpui::AnyElement = match self.mode {
            TopologyMode::Cluster => self.render_cluster_body(window, cx),
            TopologyMode::Sentinel => self.render_sentinel_body(window, cx),
            TopologyMode::Standalone => Label::new(i18n_topology(cx, "standalone_placeholder"))
                .text_color(muted)
                .into_any_element(),
            TopologyMode::Unknown => Label::new(i18n_topology(cx, "unknown_placeholder"))
                .text_color(muted)
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .p_4()
            .gap_3()
            .child(Label::new(title).text_lg().font_bold())
            .child(body)
    }
}
