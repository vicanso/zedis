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

//! Sentinel mode: the monitored masters and their sentinels, and the
//! `SENTINEL …` administration dialogs (failover, reset, remove, monitor, set,
//! flushconfig).
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    /// Ask the sentinels for their view of the masters once the panel knows
    /// it is on a Sentinel entry — once per visit, not per heartbeat tick.
    pub(super) fn ensure_sentinel_info(&mut self, cx: &mut Context<Self>) {
        if self.mode != TopologyMode::Sentinel || self.sentinel_info_requested {
            return;
        }
        self.sentinel_info_requested = true;
        self.server_state
            .update(cx, |state, cx| state.load_sentinel_masters(cx));
    }

    pub(super) fn render_sentinel_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.server_state.read(cx);
        let desc = state.nodes_description();
        let sentinel_masters: Vec<SentinelMaster> = state.sentinel_masters().to_vec();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.table_hover;
        let success = theme.success;
        let danger = theme.danger;
        let chip_bg = theme.secondary;
        let chip_fg = theme.secondary_foreground;
        let can_write = self.can_sentinel_write(cx);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        if desc.topology.is_empty() {
            return Label::new(i18n_topology(cx, "sentinel_placeholder"))
                .text_color(muted)
                .into_any_element();
        }

        let master_count = desc.topology.len();
        let replica_count: usize = desc.topology.iter().map(|m| m.replicas.len()).sum();
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
        let ckquorum_label = i18n_topology(cx, "sentinel_ckquorum_button");
        let settings_label = i18n_topology(cx, "sentinel_settings_button");
        let current_master = desc
            .topology
            .first()
            .map(|m| m.master.master_name.clone())
            .unwrap_or_default();

        // A small rounded tag in the secondary tone; the down flags in the
        // danger tone so a failing master reads at a glance.
        let chip = |text: SharedString, fg: Hsla| {
            div()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(chip_bg)
                .child(Label::new(text).text_xs().text_color(fg))
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for master in desc.topology.iter() {
            let m_addr = master.master.addr.clone();
            let m_name = master.master.master_name.clone();
            let m_role = master.master.role_marker.clone();
            let m_annot = master.master.annotation.clone();
            let role_color = role_marker_color(&m_role, muted, success, danger);
            let info = sentinel_masters.iter().find(|m| m.name == m_name).cloned();
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
            if !m_name.is_empty() {
                // Read-only too: CKQUORUM changes nothing.
                let name_for_ckquorum = m_name.clone();
                master_row = master_row.child(
                    Button::new(SharedString::from(format!("topo-snt-ckquorum-{m_name}")))
                        .ghost()
                        .small()
                        .label(ckquorum_label.clone())
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            let name: SharedString = name_for_ckquorum.clone().into();
                            this.server_state
                                .update(cx, |state, cx| state.sentinel_ckquorum(name, cx));
                        })),
                );
            }
            if can_write && !m_name.is_empty() {
                let name_for_failover = m_name.clone();
                let name_for_reset = m_name.clone();
                let name_for_remove = m_name.clone();
                let settings_master = info.clone().unwrap_or_else(|| SentinelMaster {
                    name: m_name.clone(),
                    ..Default::default()
                });
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
                        Button::new(SharedString::from(format!("topo-snt-settings-{m_name}")))
                            .ghost()
                            .small()
                            .label(settings_label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sentinel_set_dialog(settings_master.clone(), window, cx);
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

            // What the sentinels say about this master: its flags when not a
            // plain `master`, then quorum, sentinel count and the timings.
            if let Some(info) = info {
                let mut chips = h_flex().pl_6().pr_2().pb_1().gap_1().flex_wrap();
                if info.flags != "master" {
                    let fg = if info.is_down() { danger } else { chip_fg };
                    chips = chips.child(chip(info.flags.clone().into(), fg));
                }
                let texts = [
                    rust_i18n::t!("topology.sentinel_chip_quorum", n = info.quorum, locale = &locale),
                    rust_i18n::t!(
                        "topology.sentinel_chip_sentinels",
                        n = info.num_other_sentinels + 1,
                        locale = &locale
                    ),
                    rust_i18n::t!(
                        "topology.sentinel_chip_down_after",
                        ms = info.down_after_ms,
                        locale = &locale
                    ),
                    rust_i18n::t!(
                        "topology.sentinel_chip_failover_timeout",
                        ms = info.failover_timeout_ms,
                        locale = &locale
                    ),
                    rust_i18n::t!(
                        "topology.sentinel_chip_parallel_syncs",
                        n = info.parallel_syncs,
                        locale = &locale
                    ),
                ];
                for text in texts {
                    chips = chips.child(chip(text.to_string().into(), chip_fg));
                }
                rows.push(chips.into_any_element());
            }

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

        // The entry named no master and the sentinel has several: offer the
        // others. Switching rewrites the saved entry and reconnects.
        let master_names = desc.sentinel_master_names.clone();
        let switcher = (master_names.len() > 1).then(|| {
            let mut row = h_flex().items_center().gap_1().child(
                Label::new(i18n_topology(cx, "sentinel_switch_master"))
                    .text_xs()
                    .text_color(muted),
            );
            for name in master_names {
                let is_current = name == current_master;
                let name_for_click = name.clone();
                let button = Button::new(SharedString::from(format!("topo-snt-switch-{name}")))
                    .small()
                    .label(name.clone());
                let button = if is_current { button.primary() } else { button.outline() };
                row = row.child(button.on_click(cx.listener(move |this, _, _window, cx| {
                    let name: SharedString = name_for_click.clone().into();
                    this.server_state
                        .update(cx, |state, cx| state.switch_sentinel_master(name, cx));
                })));
            }
            row
        });

        let mut actions = h_flex().items_center().gap_1();
        if can_write {
            actions = actions
                .child(
                    Button::new("topo-snt-add-master")
                        .outline()
                        .small()
                        .label(i18n_topology(cx, "sentinel_add_master_button"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_sentinel_monitor_dialog(window, cx))),
                )
                .child(
                    Button::new("topo-snt-flushconfig")
                        .ghost()
                        .small()
                        .label(i18n_topology(cx, "sentinel_flushconfig_button"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_sentinel_flushconfig_dialog(window, cx))),
                );
        }
        actions = actions.child(
            Button::new("topo-snt-refresh")
                .outline()
                .small()
                .icon(Icon::new(CustomIconName::RotateCw))
                .tooltip(i18n_topology(cx, "refresh_tooltip"))
                .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
        );

        let mut col = v_flex().gap_3().child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    h_flex()
                        .items_center()
                        .gap_3()
                        .child(Label::new(summary).text_xs().text_color(muted))
                        .children(switcher),
                )
                .child(actions),
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

    pub(super) fn open_sentinel_failover_dialog(
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

    pub(super) fn open_sentinel_reset_dialog(
        &mut self,
        master_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn open_sentinel_remove_dialog(
        &mut self,
        master_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn open_sentinel_monitor_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| ZedisSentinelMonitorDialog::new(window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let server_state = self.server_state.clone();
        ZedisDialog::new(i18n_topology(cx, "sentinel_monitor_dialog_title"))
            .w(px(460.))
            .ok_text(i18n_topology(cx, "sentinel_monitor_ok"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_topology(cx, "sentinel_monitor_ok"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, window, cx| {
                // An invalid form shows its error and stays open.
                let Some(input) = view_ok.update(cx, |view, cx| view.validate(cx)) else {
                    return false;
                };
                server_state.update(cx, |state, cx| {
                    state.sentinel_monitor(input.name, input.host, input.port, input.quorum, cx)
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    pub(super) fn open_sentinel_set_dialog(
        &mut self,
        master: SentinelMaster,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name: SharedString = master.name.clone().into();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let title = rust_i18n::t!(
            "topology.sentinel_set_dialog_title",
            name = master.name,
            locale = &locale
        )
        .to_string();
        let view = cx.new(|cx| ZedisSentinelSetDialog::new(master, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let server_state = self.server_state.clone();
        ZedisDialog::new(title)
            .w(px(520.))
            .ok_text(i18n_topology(cx, "sentinel_set_ok"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_topology(cx, "sentinel_set_ok"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, window, cx| {
                let Some(options) = view_ok.update(cx, |view, cx| view.changed_options(cx)) else {
                    return false;
                };
                if !options.is_empty() {
                    let name = name.clone();
                    server_state.update(cx, |state, cx| state.sentinel_set(name, options, cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    pub(super) fn open_sentinel_flushconfig_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_topology(cx, "sentinel_flushconfig_confirm_title");
        let body = i18n_topology(cx, "sentinel_flushconfig_body");
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| state.sentinel_flushconfig(cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}
