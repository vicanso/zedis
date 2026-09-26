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
//!     **Slots** (hash-slot map, in-flight migrations with a
//!     `SETSLOT … STABLE` repair for one that never finished, and
//!     `ADDSLOTS` for slots no master owns), **Load**
//!     (per-master memory/OPS heatmap), **Reshard** (plan + execute
//!     slot moves, and a whole-cluster rebalance). Reshard takes the
//!     Valkey 9 route when the server has it — `CLUSTER MIGRATESLOTS`
//!     hands whole slots to the servers, which then own the job — and
//!     the app's own `SETSLOT` + `MIGRATE` loop otherwise. Slot ownership comes from the heartbeat
//!     `ClusterSlotMap`; load is polled separately.
//!   * Sentinel: monitored-master list with per-master
//!     `Force Failover` / `Reset` / `Remove` buttons; replica rows
//!     are read-only because Sentinel ops target by master name.
//!   * Standalone: the primary / replica link from the heartbeat's
//!     `INFO replication` — this server with its replicas (each with a
//!     `FAILOVER` button, Redis 6.2+) or under the primary it follows —
//!     plus `REPLICAOF host port` / `REPLICAOF NO ONE`. Those three live
//!     here and nowhere else: a Sentinel undoes a hand-made REPLICAOF and
//!     a cluster refuses it (ADR 6).
//!   * Unknown: localized placeholder text only.
//!
//! All destructive commands route through `ZedisDialog::new_alert`, with
//! the body run through `escalate_dangerous_body` so production-tagged
//! servers get the escalated warning.

use crate::assets::CustomIconName;
use crate::connection::{
    AtomicSlotMigration, CLUSTER_HASH_SLOTS, Capability, ClusterSlotMap, FAILOVER_TIMEOUT_MS, NodeHealth,
    RebalanceMove, ReplicationRole, SentinelMaster, ServerCommand, ServerDb, SlotStatMetric, SlotStatRow,
    cluster_slot_stats, floors, get_server, group_slot_ranges, slots_in_ranges, unassigned_slot_ranges,
};
use crate::error::Error;
use crate::helpers::{format_lag_bytes, get_mono_font_family};
use crate::states::{
    ClusterMasterRanges, ClusterNodeLoad, HINT_TOPOLOGY, RebalanceLeg, ReplicaInfo, ServerEvent, ZedisGlobalStore,
    ZedisServerState, dialog_button_props, escalate_dangerous_body, fetch_cluster_node_loads, fetch_slot_migrations,
    i18n_common, i18n_hints, i18n_topology, plan_cluster_rebalance_moves, plan_cluster_reshard,
    source_owners_for_slots, update_app_state_and_save_quiet,
};
use crate::views::{ZedisSentinelMonitorDialog, ZedisSentinelSetDialog, unavailable_chip};
use gpui::{Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*, px, rgb};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    progress::Progress,
    v_flex,
};
use std::time::Duration;
use tracing::info;
use zedis_ui::{ZedisDialog, hint_banner};

// One module per mode, and per tab of the cluster mode — the three modes
// never render together (ADR 6), so they never needed to share a file.
mod cluster_load;
mod cluster_nodes;
mod cluster_reshard;
mod cluster_slots;
mod replication;
mod sentinel;

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
    /// `SENTINEL MASTERS` asked for once per visit / server; cleared on a
    /// server switch so the next Sentinel entry loads its own.
    sentinel_info_requested: bool,
    cluster_tab: ClusterTab,
    // Nodes tab forms.
    meet_input: Entity<InputState>,
    replicate_target_input: Entity<InputState>,
    replicate_master_input: Entity<InputState>,
    /// Standalone: the `REPLICAOF host port` target.
    replicaof_input: Entity<InputState>,
    /// Cluster: which master the unowned slots are handed to.
    addslots_target_input: Entity<InputState>,
    /// Inline validation for the Meet / Replicate / Replicate-from forms.
    form_error: Option<SharedString>,
    // Reshard wizard inputs.
    reshard_source_input: Entity<InputState>,
    reshard_target_input: Entity<InputState>,
    reshard_count_input: Entity<InputState>,
    planned_slots: Vec<u16>,
    plan_error: Option<SharedString>,
    /// True while a reshard batch is in flight (Execute confirmed → done/fail).
    reshard_running: bool,
    /// Valkey 9 atomic migrations reported by every master, tagged with
    /// the node they were read from. Polled only while the Reshard tab is
    /// open on a server that has them.
    slot_migrations: Vec<(String, AtomicSlotMigration)>,
    slot_migrations_task: Option<Task<()>>,
    /// The even-out plan, and whether it has been computed at all — an
    /// empty plan after planning means "already balanced", which is a
    /// different thing from "not planned yet".
    rebalance_plan: Vec<RebalanceMove>,
    rebalance_planned: bool,
    // Load heatmap.
    node_loads: Vec<ClusterNodeLoad>,
    load_error: Option<SharedString>,
    load_metric: LoadMetric,
    load_poll_task: Option<Task<()>>,
    // Per-slot usage (CLUSTER SLOT-STATS, Redis 8.2) on the Slots tab.
    /// `None` until the first fetch answers (or after a refresh cleared it).
    slot_stats: Option<Vec<SlotStatRow>>,
    slot_stats_error: Option<SharedString>,
    slot_stats_metric: SlotStatMetric,
    slot_stats_task: Option<Task<()>>,
    /// First visit ever (HINT_TOPOLOGY not yet dismissed) — show the one-time
    /// intro banner. Local so closing it repaints without waiting for the
    /// async state save.
    show_first_visit_hint: bool,
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
        let replicaof_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_topology(cx, "repl_make_replica_placeholder")));
        let addslots_target_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_topology(cx, "slots_assign_placeholder")));
        let reshard_source_input = cx.new(|cx| InputState::new(window, cx).placeholder("source node_id (optional)"));
        let reshard_target_input = cx.new(|cx| InputState::new(window, cx).placeholder("target node_id"));
        let reshard_count_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 100"));

        let subscriptions = vec![cx.subscribe(&server_state, |this, _state, event, cx| {
            // Live reshard ticks: mirror the server state's in-flight flag
            // (Some → running, None → finished) and repaint the bar.
            if matches!(event, ServerEvent::ClusterReshardProgress) {
                this.reshard_running = this.server_state.read(cx).reshard_progress().is_some();
                cx.notify();
            }
            if matches!(event, ServerEvent::SentinelInfoUpdated) {
                cx.notify();
            }
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.sentinel_info_requested = false;
            }
            if matches!(
                event,
                ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
            ) {
                this.detect_mode(cx);
                this.ensure_sentinel_info(cx);
                // Drop the in-flight flag once the reshard has actually
                // reported completion (progress cleared) — not on every
                // heartbeat INFO tick, which used to reset it mid-run.
                if this.reshard_running && this.server_state.read(cx).reshard_progress().is_none() {
                    this.reshard_running = false;
                }
                if this.mode == TopologyMode::Cluster {
                    this.ensure_load_poll(cx);
                    // The version only lands with the first INFO, so a
                    // Reshard tab opened before that starts polling here.
                    this.ensure_slot_migration_poll(cx);
                }
                cx.notify();
            }
        })];

        let mut this = Self {
            server_state,
            mode: TopologyMode::Unknown,
            sentinel_info_requested: false,
            cluster_tab: ClusterTab::Nodes,
            meet_input,
            replicate_target_input,
            replicate_master_input,
            replicaof_input,
            addslots_target_input,
            form_error: None,
            reshard_source_input,
            reshard_target_input,
            reshard_count_input,
            planned_slots: Vec::new(),
            plan_error: None,
            reshard_running: false,
            slot_migrations: Vec::new(),
            slot_migrations_task: None,
            rebalance_plan: Vec::new(),
            rebalance_planned: false,
            node_loads: Vec::new(),
            load_error: None,
            load_metric: LoadMetric::Memory,
            load_poll_task: None,
            slot_stats: None,
            slot_stats_error: None,
            slot_stats_metric: SlotStatMetric::default(),
            slot_stats_task: None,
            show_first_visit_hint: !cx.global::<ZedisGlobalStore>().read(cx).hint_dismissed(HINT_TOPOLOGY),
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

    fn can_replication_write(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).can(Capability::ReplicationWrite)
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.server_state.update(cx, |state, cx| {
            state.refresh_redis_info(cx);
            if self.mode == TopologyMode::Sentinel {
                state.load_sentinel_masters(cx);
            }
        });
        // Force load re-sample on next poll cycle by clearing cache.
        if self.mode == TopologyMode::Cluster {
            self.node_loads.clear();
            self.load_poll_task = None;
            self.ensure_load_poll(cx);
            self.slot_stats = None;
            self.slot_stats_task = None;
            self.ensure_slot_stats(cx);
        }
        cx.notify();
    }

    fn set_cluster_tab(&mut self, tab: ClusterTab, cx: &mut Context<Self>) {
        self.cluster_tab = tab;
        if tab == ClusterTab::Load {
            self.ensure_load_poll(cx);
        } else {
            // Dropping the task cancels the poll loop (and its in-flight
            // sample) the moment the heatmap is no longer visible.
            self.load_poll_task = None;
        }
        if tab == ClusterTab::Slots {
            self.ensure_slot_stats(cx);
        }
        if tab == ClusterTab::Reshard {
            self.ensure_slot_migration_poll(cx);
        } else {
            self.slot_migrations_task = None;
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
}

/// `host:port` for the REPLICAOF / FAILOVER forms — an IPv6 host may be
/// bracketed (`[::1]:6379`).
fn parse_host_port(value: &str) -> Option<(String, u16)> {
    let (host, port) = value.trim().rsplit_once(':')?;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    let port: u16 = port.trim().parse().ok()?;
    (port > 0).then(|| (host.to_string(), port))
}

impl Render for ZedisTopology {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = i18n_topology(cx, "title");
        let muted = cx.theme().muted_foreground;

        let body: gpui::AnyElement = match self.mode {
            TopologyMode::Cluster => self.render_cluster_body(window, cx),
            TopologyMode::Sentinel => self.render_sentinel_body(window, cx),
            TopologyMode::Standalone => self.render_standalone_body(window, cx),
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

        let first_visit_hint = self.show_first_visit_hint.then(|| {
            hint_banner("topology-first-visit", i18n_hints(cx, "topology_banner")).on_close(cx.listener(
                |this, _, _window, cx| {
                    this.show_first_visit_hint = false;
                    update_app_state_and_save_quiet(cx, "dismiss_hint_topology", |state, _| {
                        state.dismiss_hint(HINT_TOPOLOGY)
                    });
                    cx.notify();
                },
            ))
        });

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .p_4()
            .gap_3()
            .child(header)
            .children(first_visit_hint)
            .child(body)
    }
}

/// Top slots by `metric` across all masters — the client merges, re-sorts
/// and truncates (see `RedisClient::cluster_slot_stats`).
async fn fetch_slot_stats(server_id: String, db: usize, metric: SlotStatMetric) -> Result<Vec<SlotStatRow>, Error> {
    const SLOT_STATS_LIMIT: u64 = 20;
    Ok(cluster_slot_stats(&ServerDb::new(server_id, db), metric, SLOT_STATS_LIMIT).await?)
}

#[cfg(test)]
mod tests {
    use super::parse_host_port;

    #[test]
    fn host_port_forms_accepted_by_the_replication_forms() {
        assert_eq!(parse_host_port(" 10.0.0.4:6379 "), Some(("10.0.0.4".to_string(), 6379)));
        assert_eq!(parse_host_port("[::1]:6380"), Some(("::1".to_string(), 6380)));
        assert_eq!(
            parse_host_port("redis.internal:16379"),
            Some(("redis.internal".to_string(), 16379))
        );
        for bad in ["", "6379", ":6379", "host:", "host:0", "host:99999", "host:port"] {
            assert_eq!(parse_host_port(bad), None, "{bad}");
        }
    }
}
