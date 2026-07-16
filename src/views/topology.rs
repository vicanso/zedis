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

use crate::assets::CustomIconName;
use crate::connection::{CLUSTER_HASH_SLOTS, Capability, ClusterSlotMap};
use crate::helpers::get_mono_font_family;
use crate::states::{
    ClusterMasterRanges, ClusterNodeLoad, ReplicaInfo, ServerEvent, ZedisGlobalStore, ZedisServerState,
    dialog_button_props, escalate_dangerous_body, fetch_cluster_node_loads, i18n_topology, plan_cluster_reshard,
    source_owners_for_slots,
};
use gpui::{Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*, px, rgb};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use std::time::Duration;
use tracing::info;
use zedis_ui::ZedisDialog;

/// Shorten a cluster node id for display (first 8 hex chars).
fn short_node_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Colour the role glyph: master green, fail red, replica muted.
fn role_marker_color(marker: &str, muted: Hsla, success: Hsla, danger: Hsla) -> Hsla {
    match marker {
        "●" => success,
        "✗" => danger,
        _ => muted,
    }
}

/// Compact lag-bytes label (same shape as the status-bar tooltip helper).
fn format_lag_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0".into();
    }
    humansize::format_size(bytes as u64, humansize::FormatSizeOptions::default().decimal_places(1))
}

/// Strip optional `@busport` so CLUSTER NODES addresses match INFO replication.
fn addr_key(addr: &str) -> &str {
    addr.split('@').next().unwrap_or(addr)
}

/// Find live lag for a topology replica address.
fn lag_for_addr<'a>(replicas: &'a [ReplicaInfo], addr: &str) -> Option<&'a ReplicaInfo> {
    let key = addr_key(addr);
    replicas.iter().find(|r| addr_key(r.addr.as_ref()) == key)
}

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
    /// Inline validation for Meet / Replicate forms.
    form_error: Option<SharedString>,
    // Reshard wizard inputs.
    reshard_source_input: Entity<InputState>,
    reshard_target_input: Entity<InputState>,
    reshard_count_input: Entity<InputState>,
    planned_slots: Vec<u16>,
    plan_error: Option<SharedString>,
    /// True while a reshard batch is in flight (Execute confirmed → done/fail).
    reshard_running: bool,
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
                // Reshard completion refreshes INFO — drop the in-flight flag so
                // Execute becomes available again after success or failure toast.
                if this.reshard_running {
                    this.reshard_running = false;
                }
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
            form_error: None,
            reshard_source_input,
            reshard_target_input,
            reshard_count_input,
            planned_slots: Vec::new(),
            plan_error: None,
            reshard_running: false,
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

    fn can_cluster_write(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).can(Capability::ClusterWrite)
    }

    fn can_sentinel_write(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).can(Capability::SentinelWrite)
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.server_state.update(cx, |state, cx| {
            state.refresh_redis_info(cx);
        });
        // Force load re-sample on next poll cycle by clearing cache.
        if self.mode == TopologyMode::Cluster {
            self.node_loads.clear();
            self.load_poll_task = None;
            self.ensure_load_poll(cx);
        }
        cx.notify();
    }

    fn fill_replicate_target(&mut self, addr: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.form_error = None;
        self.replicate_target_input.update(cx, |input, cx| {
            input.set_value(addr, window, cx);
        });
        cx.notify();
    }

    fn fill_replicate_master(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.form_error = None;
        self.replicate_master_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    fn fill_reshard_source(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_error = None;
        self.reshard_source_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    fn fill_reshard_target(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_error = None;
        self.reshard_target_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    fn clear_reshard_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reshard_source_input.update(cx, |input, cx| {
            input.set_value(SharedString::default(), window, cx);
        });
        cx.notify();
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
        let mut row = h_flex().gap_1().items_center().flex_1();
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
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .child(row)
            .child(
                Button::new("topo-refresh")
                    .outline()
                    .small()
                    .icon(Icon::new(CustomIconName::RotateCw))
                    .tooltip(i18n_topology(cx, "refresh_tooltip"))
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
            .into_any_element()
    }

    fn render_readonly_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.can_cluster_write(cx) {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .p_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.warning)
                .bg(theme.warning.opacity(0.1))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Info).text_color(theme.warning))
                        .child(
                            Label::new(i18n_topology(cx, "readonly_banner"))
                                .text_xs()
                                .text_color(theme.warning),
                        ),
                )
                .into_any_element(),
        )
    }

    // ── Nodes tab (existing ops) ──────────────────────────────────────

    fn render_cluster_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let body = match self.cluster_tab {
            ClusterTab::Nodes => self.render_nodes_tab(window, cx),
            ClusterTab::Slots => self.render_slots_tab(cx),
            ClusterTab::Load => self.render_load_tab(cx),
            ClusterTab::Reshard => self.render_reshard_tab(window, cx),
        };
        let mut col = v_flex().gap_3().child(self.render_cluster_tabs(cx));
        if let Some(banner) = self.render_readonly_banner(cx) {
            col = col.child(banner);
        }
        col.child(body).into_any_element()
    }

    fn render_nodes_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let hover = cx.theme().table_hover;
        let can_write = self.can_cluster_write(cx);
        let failover_label = i18n_topology(cx, "failover_button");
        let force_failover_label = i18n_topology(cx, "force_failover_button");
        let forget_label = i18n_topology(cx, "forget_button");
        let meet_label = i18n_topology(cx, "meet_button");
        let replicate_label = i18n_topology(cx, "replicate_button");
        let fill_hint = i18n_topology(cx, "click_fill_hint");

        let form_error_el = self
            .form_error
            .as_ref()
            .map(|err| Label::new(err.clone()).text_xs().text_color(danger).into_any_element());

        let meet_form = v_flex()
            .gap_1()
            .child(Label::new(i18n_topology(cx, "meet_label")).text_xs().text_color(muted))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    // Default (medium) input size — `.small()` was too tight for host:port.
                    .child(Input::new(&self.meet_input).flex_1())
                    .child(
                        Button::new("topo-meet-btn")
                            .primary()
                            .small()
                            .label(meet_label)
                            .disabled(!can_write)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.can_cluster_write(cx) {
                                    return;
                                }
                                let raw = this.meet_input.read(cx).value().to_string();
                                let raw = raw.trim();
                                let Some((host, port_str)) = raw.rsplit_once(':') else {
                                    this.form_error = Some(i18n_topology(cx, "err_meet_host_port"));
                                    cx.notify();
                                    return;
                                };
                                let host = host.trim();
                                if host.is_empty() {
                                    this.form_error = Some(i18n_topology(cx, "err_meet_host_port"));
                                    cx.notify();
                                    return;
                                }
                                let Ok(port) = port_str.trim().parse::<u16>() else {
                                    this.form_error = Some(i18n_topology(cx, "err_meet_host_port"));
                                    cx.notify();
                                    return;
                                };
                                this.form_error = None;
                                this.open_meet_dialog(SharedString::from(host.to_string()), port, window, cx);
                            })),
                    ),
            );

        let replicate_form = v_flex()
            .gap_1()
            .child(
                Label::new(i18n_topology(cx, "replicate_label"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.replicate_target_input).flex_1())
                    .child(Input::new(&self.replicate_master_input).flex_1())
                    .child(
                        Button::new("topo-replicate-btn")
                            .primary()
                            .small()
                            .label(replicate_label)
                            .disabled(!can_write)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.can_cluster_write(cx) {
                                    return;
                                }
                                let target = this.replicate_target_input.read(cx).value().trim().to_string();
                                let master_id = this.replicate_master_input.read(cx).value().trim().to_string();
                                if target.is_empty() {
                                    this.form_error = Some(i18n_topology(cx, "err_replicate_target"));
                                    cx.notify();
                                    return;
                                }
                                if master_id.is_empty() {
                                    this.form_error = Some(i18n_topology(cx, "err_replicate_master"));
                                    cx.notify();
                                    return;
                                }
                                this.form_error = None;
                                this.open_replicate_dialog(
                                    SharedString::from(target),
                                    SharedString::from(master_id),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            );

        if desc.topology.is_empty() {
            let mut empty = v_flex().gap_3().child(meet_form).child(replicate_form);
            if let Some(err) = form_error_el {
                empty = empty.child(err);
            }
            return empty
                .child(Label::new(i18n_topology(cx, "cluster_placeholder")).text_color(muted))
                .into_any_element();
        }

        let master_count = desc.topology.len();
        let replica_count: usize = desc.topology.iter().map(|m| m.replicas.len()).sum();
        let fail_count: usize = desc
            .topology
            .iter()
            .map(|m| {
                let mut n = if m.master.role_marker == "✗" { 1 } else { 0 };
                n += m.replicas.iter().filter(|r| r.role_marker == "✗").count();
                n
            })
            .sum();
        let assigned = desc.slot_map.assigned_slots;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        // Live lag from INFO replication (heartbeat) — matched onto topology
        // replica rows by host:port. Empty when connected to a replica or
        // when the master has not reported any slaves yet.
        let replica_lags: Vec<ReplicaInfo> = self
            .server_state
            .read(cx)
            .redis_info()
            .map(|info| info.replicas.clone())
            .unwrap_or_default();
        let warning = cx.theme().warning;
        let summary: SharedString = rust_i18n::t!(
            "topology.nodes_summary",
            masters = master_count,
            replicas = replica_count,
            slots = assigned,
            locale = locale
        )
        .to_string()
        .into();
        let fail_badge = if fail_count > 0 {
            Some(
                Label::new(SharedString::from(
                    rust_i18n::t!("topology.nodes_failed", count = fail_count, locale = locale).to_string(),
                ))
                .text_xs()
                .text_color(danger),
            )
        } else {
            None
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for master in desc.topology.iter() {
            let m_addr = master.master.addr.clone();
            let m_node_id = master.master.node_id.clone();
            let m_role = master.master.role_marker.clone();
            let m_annot = master.master.annotation.clone();
            let role_color = role_marker_color(&m_role, muted, success, danger);
            let short_id = short_node_id(&m_node_id);
            let slot_count = desc
                .slot_map
                .masters
                .iter()
                .find(|m| m.node_id == m_node_id)
                .map(|m| m.slot_count)
                .unwrap_or(0);
            let color_idx = desc
                .slot_map
                .masters
                .iter()
                .find(|m| m.node_id == m_node_id)
                .map(|m| m.color_index)
                .unwrap_or(0);
            let stripe = master_color(color_idx);

            let id_for_fill = m_node_id.clone();
            let mut master_row = h_flex()
                .id(SharedString::from(format!("topo-mrow-{m_addr}")))
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .hover(move |s| s.bg(hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !id_for_fill.is_empty() {
                        this.fill_replicate_master(id_for_fill.clone().into(), window, cx);
                    }
                }))
                .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(stripe))
                .child(Label::new(m_role).text_xs().text_color(role_color))
                .child(Label::new(m_addr.clone()).font_semibold())
                .when(!short_id.is_empty(), |row| {
                    row.child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                })
                .when(!m_annot.is_empty(), |row| {
                    row.child(Label::new(m_annot).text_xs().text_color(muted))
                })
                .when(slot_count > 0, |row| {
                    row.child(
                        Label::new(SharedString::from(
                            rust_i18n::t!("topology.slots_count_label", count = slot_count, locale = locale)
                                .to_string(),
                        ))
                        .text_xs()
                        .text_color(muted),
                    )
                })
                .child(div().flex_1());
            if can_write && !m_node_id.is_empty() {
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
                let role_color = role_marker_color(&r_role, muted, success, danger);
                let short_id = short_node_id(&r_node_id);
                let failover_target = r_addr.clone();
                let force_failover_target = r_addr.clone();
                let addr_for_fill = r_addr.clone();
                // Lag chip: always shown on replica rows. Unknown → muted "—";
                // elevated lag_seconds / lag_bytes use warning colour.
                let lag_info = lag_for_addr(&replica_lags, &r_addr);
                let (lag_label, lag_color): (SharedString, Hsla) = if let Some(lag) = lag_info {
                    let text: SharedString = rust_i18n::t!(
                        "topology.replica_lag",
                        bytes = format_lag_bytes(lag.lag_bytes),
                        secs = lag.lag_seconds,
                        locale = locale
                    )
                    .to_string()
                    .into();
                    // Soft warn from 3s / 256KB; strong warn from 10s / 4MB.
                    // Sub-second lag is not reported (Redis lag is integer
                    // seconds), and 1–2s is common under light load.
                    let color = if lag.lag_seconds >= 10 || lag.lag_bytes >= 4 * 1024 * 1024 {
                        danger
                    } else if lag.lag_seconds >= 3 || lag.lag_bytes >= 256 * 1024 {
                        warning
                    } else {
                        muted
                    };
                    (text, color)
                } else {
                    (i18n_topology(cx, "replica_lag_unknown"), muted)
                };
                let lag_state = lag_info.map(|l| l.state.clone()).unwrap_or_default();
                let mut replica_row = h_flex()
                    .id(SharedString::from(format!("topo-rrow-{r_addr}")))
                    .items_center()
                    .gap_2()
                    .pl_6()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .hover(move |s| s.bg(hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.fill_replicate_target(addr_for_fill.clone().into(), window, cx);
                    }))
                    .child(Label::new(r_role).text_xs().text_color(role_color))
                    .child(Label::new(r_addr.clone()).text_color(muted))
                    .when(!short_id.is_empty(), |row| {
                        row.child(
                            Label::new(SharedString::from(short_id))
                                .text_xs()
                                .text_color(muted)
                                .font_family(get_mono_font_family()),
                        )
                    })
                    .when(!r_annot.is_empty(), |row| {
                        row.child(Label::new(r_annot).text_xs().text_color(muted))
                    })
                    .child(
                        Label::new(lag_label)
                            .text_xs()
                            .text_color(lag_color)
                            .font_family(get_mono_font_family()),
                    )
                    .when(!lag_state.is_empty() && lag_state.as_ref() != "online", |row| {
                        row.child(Label::new(lag_state).text_xs().text_color(warning))
                    })
                    .child(div().flex_1());
                if can_write {
                    replica_row = replica_row
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
                                .danger()
                                .small()
                                .label(force_failover_label.clone())
                                .tooltip(i18n_topology(cx, "force_failover_tooltip"))
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
                }
                rows.push(replica_row.into_any_element());
            }
        }

        let mut col = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Label::new(summary).text_xs().text_color(muted))
                    .when_some(fail_badge, |this, badge| this.child(badge)),
            )
            .child(Label::new(fill_hint).text_xs().text_color(muted))
            .child(meet_form)
            .child(replicate_form);
        if let Some(err) = form_error_el {
            col = col.child(err);
        }
        col.child(v_flex().gap_1().children(rows)).into_any_element()
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
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
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
            )
            .child(div().flex_1())
            .child(
                Button::new("load-to-reshard")
                    .ghost()
                    .small()
                    .label(i18n_topology(cx, "goto_reshard"))
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.set_cluster_tab(ClusterTab::Reshard, cx);
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
            let short_id = short_node_id(&n.node_id);
            let stripe = master_color(n.color_index);
            let slots_label =
                rust_i18n::t!("topology.slots_count_label", count = n.slot_count, locale = locale).to_string();
            let id_for_fill = n.node_id.clone();
            cards.push(
                v_flex()
                    .id(SharedString::from(format!("load-card-{}", n.node_id)))
                    .w(px(200.))
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .cursor_pointer()
                    .bg({
                        let mut c = heat;
                        c.a = 0.25;
                        c
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Click load card → target for reshard (heavier masters
                        // are the usual source; fill as source when above avg).
                        this.fill_reshard_source(id_for_fill.clone(), window, cx);
                        this.set_cluster_tab(ClusterTab::Reshard, cx);
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(stripe))
                            .child(Label::new(n.addr.clone()).font_semibold()),
                    )
                    .child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                    .child(Label::new(SharedString::from(value_label)).text_sm())
                    .child(Label::new(SharedString::from(slots_label)).text_xs().text_color(muted))
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
            .child(
                Label::new(i18n_topology(cx, "load_click_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(metric_row)
            .child(h_flex().gap_3().flex_wrap().children(cards))
            .into_any_element()
    }

    // ── Reshard tab ───────────────────────────────────────────────────

    fn render_reshard_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let can_write = self.can_cluster_write(cx);
        let desc = self.server_state.read(cx).nodes_description();
        let selected_source = self.reshard_source_input.read(cx).value().to_string();
        let selected_target = self.reshard_target_input.read(cx).value().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        // Master pickers: each row is a master with Source / Target buttons.
        // Selected side is highlighted so the free-text fields stay as power-user
        // fallback while the common path is one click.
        let mut master_chips: Vec<gpui::AnyElement> = Vec::new();
        for m in &desc.slot_map.masters {
            let id = m.node_id.clone();
            let id_as_source = id.clone();
            let id_as_target = id.clone();
            let color = master_color(m.color_index);
            let is_src = !selected_source.is_empty() && selected_source == id;
            let is_tgt = !selected_target.is_empty() && selected_target == id;
            let short_id = short_node_id(&m.node_id);
            let slots_label =
                rust_i18n::t!("topology.slots_count_label", count = m.slot_count, locale = locale).to_string();
            master_chips.push(
                h_flex()
                    .id(SharedString::from(format!("reshard-chip-{}", m.node_id)))
                    .gap_1()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if is_src || is_tgt { primary } else { border })
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
                    .child(Label::new(m.addr.clone()).text_xs())
                    .child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                    .child(Label::new(SharedString::from(slots_label)).text_xs().text_color(muted))
                    .child(
                        Button::new(SharedString::from(format!("reshard-src-{}", m.node_id)))
                            .when(is_src, |b| b.primary())
                            .when(!is_src, |b| b.ghost())
                            .small()
                            .label(i18n_topology(cx, "reshard_as_source"))
                            .disabled(!can_write || self.reshard_running)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fill_reshard_source(id_as_source.clone().into(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("reshard-tgt-{}", m.node_id)))
                            .when(is_tgt, |b| b.primary())
                            .when(!is_tgt, |b| b.ghost())
                            .small()
                            .label(i18n_topology(cx, "reshard_as_target"))
                            .disabled(!can_write || self.reshard_running)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fill_reshard_target(id_as_target.clone().into(), window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }

        let plan_btn = Button::new("reshard-plan")
            .primary()
            .small()
            .label(i18n_topology(cx, "reshard_plan"))
            .disabled(!can_write || self.reshard_running)
            .on_click(cx.listener(|this, _, _window, cx| {
                this.run_plan(cx);
            }));

        let execute_btn = Button::new("reshard-exec")
            .danger()
            .small()
            .label(if self.reshard_running {
                i18n_topology(cx, "reshard_running")
            } else {
                i18n_topology(cx, "reshard_execute")
            })
            .disabled(!can_write || self.planned_slots.is_empty() || self.reshard_running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_reshard_dialog(window, cx);
            }));

        let clear_src_btn = Button::new("reshard-clear-src")
            .ghost()
            .small()
            .label(i18n_topology(cx, "reshard_clear_source"))
            .disabled(selected_source.trim().is_empty() || self.reshard_running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_reshard_source(window, cx);
            }));

        let preview: gpui::AnyElement = if self.reshard_running {
            Label::new(i18n_topology(cx, "reshard_progress"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else if let Some(err) = &self.plan_error {
            Label::new(err.clone()).text_color(cx.theme().danger).into_any_element()
        } else if self.planned_slots.is_empty() {
            Label::new(i18n_topology(cx, "reshard_pick_target"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            let n = self.planned_slots.len();
            let preview_slots = if n > 24 {
                let head: Vec<String> = self.planned_slots.iter().take(20).map(|s| s.to_string()).collect();
                format!("{} … (+{} more)", head.join(", "), n - 20)
            } else {
                self.planned_slots
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let title: SharedString = rust_i18n::t!("topology.reshard_will_move", count = n, locale = locale)
                .to_string()
                .into();
            v_flex()
                .gap_1()
                .child(Label::new(title).font_semibold())
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
            .child(
                Label::new(i18n_topology(cx, "reshard_pick_masters"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(v_flex().gap_1().children(master_chips))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                Label::new(i18n_topology(cx, "reshard_source_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_source_input)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                Label::new(i18n_topology(cx, "reshard_target_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_target_input)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(120.))
                            .child(
                                Label::new(i18n_topology(cx, "reshard_count_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_count_input)),
                    ),
            )
            .child(h_flex().gap_2().child(plan_btn).child(execute_btn).child(clear_src_btn))
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
        if target_id.is_empty() {
            self.plan_error = Some(i18n_topology(cx, "err_reshard_target"));
            self.planned_slots.clear();
            cx.notify();
            return;
        }
        let count: u32 = match count_raw.trim().parse() {
            Ok(n) if n > 0 => n,
            _ => {
                self.plan_error = Some(i18n_topology(cx, "err_reshard_count"));
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
                // Map known English planner errors to i18n keys.
                let key = match e.as_str() {
                    "no source slots available" => "err_reshard_no_source",
                    other if other.contains("target") => "err_reshard_target",
                    _ => "err_reshard_plan",
                };
                let mapped = if key == "err_reshard_plan" {
                    SharedString::from(e)
                } else {
                    i18n_topology(cx, key)
                };
                self.plan_error = Some(mapped);
            }
        }
        cx.notify();
    }

    fn open_reshard_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.planned_slots.is_empty() || self.reshard_running {
            return;
        }
        if !self.can_cluster_write(cx) {
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
            self.plan_error = Some(i18n_topology(cx, "err_reshard_target_missing"));
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
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.reshard_confirm_body",
            count = self.planned_slots.len(),
            target_id = target_id.as_str(),
            target_addr = target_addr.as_str(),
            locale = locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let slots = self.planned_slots.clone();
        let target_addr_s: SharedString = target_addr.into();
        let target_id_s: SharedString = target_id.into();
        let entity = cx.entity().downgrade();
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
                if let Some(this) = entity.upgrade() {
                    this.update(cx, |this, cx| {
                        this.reshard_running = true;
                        this.planned_slots.clear();
                        cx.notify();
                    });
                }
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
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let can_write = self.can_sentinel_write(cx);

        if desc.topology.is_empty() {
            return Label::new(i18n_topology(cx, "sentinel_placeholder"))
                .text_color(muted)
                .into_any_element();
        }

        let master_count = desc.topology.len();
        let replica_count: usize = desc.topology.iter().map(|m| m.replicas.len()).sum();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let summary: SharedString = rust_i18n::t!(
            "topology.sentinel_summary",
            masters = master_count,
            replicas = replica_count,
            locale = locale
        )
        .to_string()
        .into();
        let failover_label = i18n_topology(cx, "sentinel_failover_button");
        let reset_label = i18n_topology(cx, "sentinel_reset_button");
        let remove_label = i18n_topology(cx, "sentinel_remove_button");

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for master in desc.topology.iter() {
            let m_addr = master.master.addr.clone();
            let m_name = master.master.master_name.clone();
            let m_role = master.master.role_marker.clone();
            let m_annot = master.master.annotation.clone();
            let role_color = role_marker_color(&m_role, muted, success, danger);
            let mut master_row = h_flex()
                .id(SharedString::from(format!("topo-snt-mrow-{m_addr}")))
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .hover(move |s| s.bg(hover))
                .child(Label::new(m_role).text_xs().text_color(role_color))
                .child(Label::new(m_addr).font_semibold())
                .child(Label::new(m_annot).text_xs().text_color(muted))
                .child(div().flex_1());
            if can_write && !m_name.is_empty() {
                let name_for_failover = m_name.clone();
                let name_for_reset = m_name.clone();
                let name_for_remove = m_name.clone();
                master_row = master_row
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-failover-{m_name}")))
                            .danger()
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
                let r_role = replica.role_marker.clone();
                let role_color = role_marker_color(&r_role, muted, success, danger);
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("topo-snt-rrow-{}", replica.addr)))
                        .items_center()
                        .gap_2()
                        .pl_6()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .hover(move |s| s.bg(hover))
                        .child(Label::new(r_role).text_xs().text_color(role_color))
                        .child(Label::new(replica.addr.clone()).text_color(muted))
                        .child(Label::new(replica.annotation.clone()).text_xs().text_color(muted))
                        .into_any_element(),
                );
            }
        }

        let mut col = v_flex().gap_3().child(
            h_flex()
                .items_center()
                .justify_between()
                .child(Label::new(summary).text_xs().text_color(muted))
                .child(
                    Button::new("topo-snt-refresh")
                        .outline()
                        .small()
                        .icon(Icon::new(CustomIconName::RotateCw))
                        .tooltip(i18n_topology(cx, "refresh_tooltip"))
                        .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
                ),
        );
        if !can_write {
            let theme = cx.theme();
            col = col.child(
                div()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.warning)
                    .bg(theme.warning.opacity(0.1))
                    .child(
                        Label::new(i18n_topology(cx, "readonly_banner"))
                            .text_xs()
                            .text_color(theme.warning),
                    ),
            );
        }
        col.child(v_flex().gap_1().children(rows)).into_any_element()
    }

    // ── Confirm dialogs (unchanged behaviour) ─────────────────────────

    fn open_failover_dialog(
        &mut self,
        target_addr: SharedString,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if force {
            i18n_topology(cx, "force_failover_confirm_title")
        } else {
            i18n_topology(cx, "failover_confirm_title")
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = if force {
            rust_i18n::t!(
                "topology.force_failover_confirm_body",
                addr = target_addr.as_ref(),
                locale = locale
            )
            .to_string()
        } else {
            rust_i18n::t!(
                "topology.failover_confirm_body",
                addr = target_addr.as_ref(),
                locale = locale
            )
            .to_string()
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

        // Cluster/Sentinel headers already include refresh; stand-alone still
        // gets a refresh so the mode can flip once CLUSTER INFO arrives.
        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(Label::new(title).text_lg().font_bold())
            .when(
                matches!(self.mode, TopologyMode::Standalone | TopologyMode::Unknown),
                |this| {
                    this.child(
                        Button::new("topo-header-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_topology(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
                    )
                },
            );

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .p_4()
            .gap_3()
            .child(header)
            .child(body)
    }
}
