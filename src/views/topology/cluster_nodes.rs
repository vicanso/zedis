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

//! Cluster mode, the Nodes tab: the node table and the per-node dialogs
//! (failover, forget, meet, replicate).
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    pub(super) fn fill_replicate_target(&mut self, addr: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.form_error = None;
        self.replicate_target_input.update(cx, |input, cx| {
            input.set_value(addr, window, cx);
        });
        cx.notify();
    }

    pub(super) fn fill_replicate_master(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.form_error = None;
        self.replicate_master_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    pub(super) fn render_nodes_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        // `fail?`: nodes this one has stopped hearing from, before the
        // cluster agrees they are gone — the same count on Redis and
        // Valkey, read from the flags rather than from Valkey 9's
        // `cluster_nodes_pfail`.
        let pfail_count: usize = desc
            .topology
            .iter()
            .map(|m| {
                usize::from(m.master.health == NodeHealth::PossiblyFailing)
                    + m.replicas
                        .iter()
                        .filter(|r| r.health == NodeHealth::PossiblyFailing)
                        .count()
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
        // Which zone each master answers from (Valkey 8.1+, when the
        // operator set `availability-zone`); nothing to show otherwise.
        let node_zones: Vec<(String, String)> = self
            .server_state
            .read(cx)
            .redis_info()
            .map(|info| info.node_zones.clone())
            .unwrap_or_default();
        let zone_tooltip = i18n_topology(cx, "availability_zone");
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
        let pfail_badge = (pfail_count > 0).then(|| {
            Label::new(SharedString::from(
                rust_i18n::t!("topology.nodes_pfail", count = pfail_count, locale = locale).to_string(),
            ))
            .text_xs()
            .text_color(warning)
        });

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
                .when_some(
                    node_zones
                        .iter()
                        .find(|(addr, _)| addr == &m_addr)
                        .map(|(_, zone)| zone.clone()),
                    |row, zone| {
                        let tooltip = zone_tooltip.clone();
                        row.child(
                            div()
                                .id(SharedString::from(format!("topo-zone-{m_addr}")))
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(muted)
                                .child(Label::new(SharedString::from(zone)).text_xs().text_color(muted))
                                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
                        )
                    },
                )
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
                    .when_some(fail_badge, |this, badge| this.child(badge))
                    .when_some(pfail_badge, |this, badge| this.child(badge)),
            )
            .child(Label::new(fill_hint).text_xs().text_color(muted))
            .child(meet_form)
            .child(replicate_form);
        if let Some(err) = form_error_el {
            col = col.child(err);
        }
        col.child(v_flex().gap_1().children(rows)).into_any_element()
    }

    pub(super) fn open_failover_dialog(
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

    pub(super) fn open_forget_dialog(
        &mut self,
        node_id: SharedString,
        addr_display: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        // Forgetting a master that still owns slots orphans them: the
        // cluster loses coverage and answers cluster_state:fail. Refuse
        // and say what to do instead, rather than confirming a break.
        let owned: u32 = self
            .server_state
            .read(cx)
            .nodes_description()
            .slot_map
            .owners
            .iter()
            .filter(|range| range.node_id.as_str() == node_id.as_ref())
            .map(|range| u32::from(range.end - range.start) + 1)
            .sum();
        if owned > 0 {
            let body = rust_i18n::t!(
                "topology.forget_owns_slots_body",
                addr = addr_display.as_ref(),
                count = owned,
                locale = &locale
            )
            .to_string();
            ZedisDialog::new_alert(i18n_topology(cx, "forget_owns_slots_title"), body)
                .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
                .open(window, cx);
            return;
        }

        let title = i18n_topology(cx, "forget_confirm_title");
        let body = rust_i18n::t!(
            "topology.forget_confirm_body",
            addr = addr_display.as_ref(),
            node_id = node_id.as_ref(),
            locale = &locale
        )
        .to_string();
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

    pub(super) fn open_meet_dialog(
        &mut self,
        host: SharedString,
        port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn open_replicate_dialog(
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
}
