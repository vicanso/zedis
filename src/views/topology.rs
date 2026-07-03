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
//!   * Cluster: master+replica list pulled from
//!     `nodes_description().topology`. Replica rows carry per-row
//!     `Failover` (graceful) / `Force` (skip handshake) / `Forget`
//!     buttons; master rows carry `Forget` only. Above the table
//!     sit two forms — `MEET host:port` (introduce a new node)
//!     and `REPLICATE target master_id` (point a node at a master).
//!     Forms render even when the topology is empty so a fresh
//!     cluster can be bootstrapped through this panel.
//!   * Sentinel: monitored-master list with per-master
//!     `Force Failover` / `Reset` / `Remove` buttons; replica rows
//!     are read-only because Sentinel ops target by master name.
//!   * Standalone / Unknown: localized placeholder text only —
//!     topology operations do not apply.
//!
//! All destructive commands route through `ZedisDialog::new_alert`, with
//! the body run through `escalate_dangerous_body` so production-tagged
//! (high-risk) servers get the escalated warning. Cluster commands live in
//! `states/server/cluster.rs`; Sentinel commands in
//! `states/server/sentinel.rs`. Slot-range RESHARD is intentionally
//! out of scope — the visual slot editor is a substantial side
//! project the maintainer chose not to pursue.

use crate::helpers::get_mono_font_family;
use crate::states::{ServerEvent, ZedisServerState, dialog_button_props, escalate_dangerous_body, i18n_topology};
use gpui::{Entity, SharedString, Subscription, Window, div, prelude::*};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use tracing::info;
use zedis_ui::ZedisDialog;

/// Three deployment shapes Redis exposes — they are mutually
/// exclusive per connection, which is why a single panel can adapt
/// rather than two sidebar entries always showing one disabled.
/// `Unknown` is the transient state before the first `INFO` round
/// trip surfaces `nodes_description`.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum TopologyMode {
    #[default]
    Unknown,
    Standalone,
    Cluster,
    Sentinel,
}

pub struct ZedisTopology {
    server_state: Entity<ZedisServerState>,
    mode: TopologyMode,
    // P2.4 form inputs (rendered above the node table when in
    // Cluster mode). MEET takes `host:port`; REPLICATE takes both
    // the target node's `host:port` and the master's `node_id`,
    // because `CLUSTER REPLICATE` runs *on* the target but
    // identifies the future master by id.
    meet_input: Entity<InputState>,
    replicate_target_input: Entity<InputState>,
    replicate_master_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisTopology {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // P2.4 form inputs — placeholders are hardcoded English on
        // purpose: they're technical address/id formats that don't
        // translate cleanly and stay informative across locales.
        let meet_input = cx.new(|cx| InputState::new(window, cx).placeholder("host:port"));
        let replicate_target_input = cx.new(|cx| InputState::new(window, cx).placeholder("target host:port"));
        let replicate_master_input = cx.new(|cx| InputState::new(window, cx).placeholder("master node_id"));
        // Re-detect on the same events Persistence listens to:
        // `ServerRedisInfoUpdated` (2s heartbeat after connect) and
        // `ServerSelected` (user switched server in the sidebar).
        let subscriptions = vec![cx.subscribe(&server_state, |this, _state, event, cx| {
            if matches!(
                event,
                ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
            ) {
                this.detect_mode(cx);
                cx.notify();
            }
        })];

        let mut this = Self {
            server_state,
            mode: TopologyMode::Unknown,
            meet_input,
            replicate_target_input,
            replicate_master_input,
            _subscriptions: subscriptions,
        };
        this.detect_mode(cx);
        info!("Creating new topology view");
        this
    }

    fn detect_mode(&mut self, cx: &mut Context<Self>) {
        // `nodes_description().server_type` is the `Debug` repr of
        // `connection::manager::ServerType`, so the match arms below
        // are the exact strings — "Standalone" / "Cluster" /
        // "Sentinel" / "Unknown".
        let desc = self.server_state.read(cx).nodes_description();
        self.mode = match desc.server_type.as_ref() {
            "Cluster" => TopologyMode::Cluster,
            "Sentinel" => TopologyMode::Sentinel,
            "Standalone" => TopologyMode::Standalone,
            _ => TopologyMode::Unknown,
        };
    }

    /// Render the Cluster mode body: a flat master-then-replicas list
    /// pulled from `nodes_description().topology`, with per-row action
    /// buttons on the right edge. Master rows get FORGET (no FAILOVER
    /// — Redis rejects FAILOVER on a master). Replica rows get
    /// FAILOVER + FORGET. Buttons whose `node_id` is empty (sentinel
    /// or standalone entries that somehow reach this branch) drop
    /// their FORGET — `CLUSTER FORGET` targets by id, not address.
    fn render_cluster_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let desc = self.server_state.read(cx).nodes_description();
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().table_hover;
        let failover_label = i18n_topology(cx, "failover_button");
        let force_failover_label = i18n_topology(cx, "force_failover_button");
        let forget_label = i18n_topology(cx, "forget_button");
        let meet_label = i18n_topology(cx, "meet_button");
        let replicate_label = i18n_topology(cx, "replicate_button");

        // Build the two write-op forms once so they show in both
        // the empty (first-bootstrap) and non-empty (mature cluster)
        // paths — MEET is most useful precisely when there's
        // nothing else to interact with yet.
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
                        // Silently drop malformed input — the form
                        // stays open so the user can correct. A real
                        // inline error message lands in a polish pass.
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
            // Master row — only FORGET applies (FAILOVER targets
            // replicas). Suppress FORGET when node_id is empty.
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
                            this.open_forget_dialog(id_for_click.clone(), addr_for_click.clone(), window, cx);
                        })),
                );
            }
            rows.push(master_row.into_any_element());

            // Replica rows — FAILOVER + FORGET. Replicas in FAIL
            // state still get both: operators may want to evict a
            // dead replica or take over via FAILOVER FORCE (the
            // FORCE option lands with P2.4's checkbox; P2.3 uses
            // graceful failover only).
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
                                this.open_failover_dialog(failover_target.clone(), false, window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-force-failover-{r_addr}")))
                            .ghost()
                            .small()
                            .label(force_failover_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_failover_dialog(force_failover_target.clone(), true, window, cx);
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
                                this.open_forget_dialog(id_for_click.clone(), addr_for_click.clone(), window, cx);
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

    /// Render the Sentinel mode body: a list of monitored masters
    /// (with their replicas indented below) and per-master action
    /// buttons. Each master row gets FAILOVER (force a swap),
    /// RESET (rediscover topology), and REMOVE (stop monitoring).
    /// Replica rows render read-only — Sentinel ops target by
    /// master name, not by replica address. When `master_name` is
    /// missing (older Redis or parser edge case), buttons drop.
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
                                this.open_sentinel_failover_dialog(name_for_failover.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-reset-{m_name}")))
                            .ghost()
                            .small()
                            .label(reset_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_reset_dialog(name_for_reset.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topo-snt-remove-{m_name}")))
                            .ghost()
                            .small()
                            .label(remove_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_remove_dialog(name_for_remove.clone(), window, cx);
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

    /// `CLUSTER FAILOVER [FORCE]` confirm dialog. The `force` flag
    /// branches the body text so users see the right risk callout
    /// before confirming. Both paths share the same i18n title —
    /// the body is English (admin op, low-frequency).
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

    /// `CLUSTER FORGET` confirm dialog. Body stresses the 60s gossip
    /// re-add window since that's the common foot-gun — fan-out by
    /// `cluster_forget` covers every master we currently know about,
    /// but a master that's briefly unreachable will re-announce the
    /// dropped node on the next gossip tick.
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

    /// `CLUSTER MEET` confirm dialog. Body stays English (admin op,
    /// title is the localized part). The op fans out to all masters
    /// via the pooled client, so gossip distributes membership within
    /// seconds — the table doesn't reflect the new node immediately,
    /// only after the next `INFO`/`CLUSTER NODES` round trip.
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

    /// `CLUSTER REPLICATE` confirm dialog. The body spells out the
    /// "must be empty / known member" precondition because the most
    /// common failure mode is REPLICATE on a node that still has
    /// data — Redis refuses with `ERR To set a master the node must
    /// be empty and without assigned hash slots`.
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

    /// `SENTINEL FAILOVER` confirm. Body stresses that this is a
    /// manual override of the sentinel quorum — operators should
    /// already know they want to skip automatic detection.
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

    /// `SENTINEL RESET` confirm. Pattern is pre-filled with the
    /// master name; for "reset everything" the operator can use
    /// the master_name "*" (sentinel glob) but this dialog runs
    /// per-master to keep the surface obvious.
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

    /// `SENTINEL REMOVE` confirm. The biggest foot-gun in this
    /// panel — once removed, the sentinel quorum stops watching
    /// the master entirely. Re-adding requires a full
    /// `SENTINEL MONITOR` invocation with config + quorum, not
    /// just clicking a button in this UI.
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
