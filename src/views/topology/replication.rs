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

//! Standalone mode — the replication view (ADR 6): the primary / replica pair
//! from `INFO replication`, and `REPLICAOF` / promote / `FAILOVER`.
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    /// The replication link of a standalone server, from the heartbeat's
    /// `INFO replication`: a primary with its replicas (each with a
    /// Failover button, Redis 6.2+), or a replica under the primary it
    /// follows; then the REPLICAOF forms. Sentinel and Cluster entries never
    /// come here — their failover lives in their own bodies, and a hand-made
    /// REPLICAOF would be undone by the sentinel or refused by the cluster.
    pub(super) fn render_standalone_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.server_state.read(cx);
        let Some(info) = state.redis_info() else {
            let muted = cx.theme().muted_foreground;
            return Label::new(i18n_topology(cx, "unknown_placeholder"))
                .text_color(muted)
                .into_any_element();
        };
        let repl = info.replication.clone();
        let server_id = state.server_id().to_string();
        let can_write = self.can_replication_write(cx);
        let replicaof_block = state.blocked_by(Capability::ReplicationWrite);
        let supports_failover = state.supports(floors::FAILOVER);
        let failover_block = state.command_block(ServerCommand::Failover);
        let self_addr: SharedString = get_server(&server_id)
            .map(|s| format!("{}:{}", s.host, s.port))
            .unwrap_or_default()
            .into();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let success = theme.success;
        let danger = theme.danger;
        let warning = theme.warning;
        let hover = theme.table_hover;
        let chip_bg = theme.secondary;
        let chip_fg = theme.secondary_foreground;
        let radius = theme.radius;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let primary_label = i18n_topology(cx, "repl_role_primary");
        let failover_label = i18n_topology(cx, "failover_button");
        let force_failover_label = i18n_topology(cx, "force_failover_button");
        let force_failover_tooltip = i18n_topology(cx, "repl_force_failover_tooltip");

        let chip = |text: SharedString, fg: Hsla| {
            div()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(chip_bg)
                .child(Label::new(text).text_xs().text_color(fg))
        };
        // A node line: role glyph, address, role tag.
        let node_row = |id: &str, marker: &str, marker_color: Hsla, addr: SharedString, role: SharedString| {
            h_flex()
                .id(SharedString::from(id.to_string()))
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .hover(move |s| s.bg(hover))
                .child(
                    Label::new(SharedString::from(marker.to_string()))
                        .text_xs()
                        .text_color(marker_color),
                )
                .child(Label::new(addr).font_semibold())
                .child(chip(role, chip_fg))
        };

        let summary: SharedString = match repl.role {
            ReplicationRole::Primary => rust_i18n::t!(
                "topology.repl_summary_primary",
                replicas = repl.connected_replicas,
                locale = &locale
            )
            .to_string()
            .into(),
            ReplicationRole::Replica => {
                let link_key = if repl.link_up() {
                    "repl_link_up"
                } else {
                    "repl_link_down"
                };
                rust_i18n::t!(
                    "topology.repl_summary_replica",
                    addr = repl.master_addr(),
                    link = i18n_topology(cx, link_key),
                    locale = &locale
                )
                .to_string()
                .into()
            }
            ReplicationRole::Unknown => i18n_topology(cx, "unknown_placeholder"),
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        match repl.role {
            ReplicationRole::Primary => {
                rows.push(
                    node_row("topo-repl-self", "●", success, self_addr.clone(), primary_label.clone())
                        .into_any_element(),
                );
                // The primary's own numbers: offset, replication id, backlog,
                // and a running failover in the warning tone.
                let mut chips = h_flex().pl_6().pr_2().pb_1().gap_1().flex_wrap();
                chips = chips.child(chip(
                    rust_i18n::t!(
                        "topology.repl_chip_offset",
                        offset = repl.master_repl_offset,
                        locale = &locale
                    )
                    .to_string()
                    .into(),
                    chip_fg,
                ));
                if !repl.master_replid.is_empty() {
                    let short: String = repl.master_replid.chars().take(8).collect();
                    chips = chips.child(chip(
                        rust_i18n::t!("topology.repl_chip_replid", id = short, locale = &locale)
                            .to_string()
                            .into(),
                        chip_fg,
                    ));
                }
                let backlog = if repl.repl_backlog_active {
                    rust_i18n::t!(
                        "topology.repl_chip_backlog",
                        size = format_lag_bytes(repl.repl_backlog_size as i64),
                        locale = &locale
                    )
                } else {
                    rust_i18n::t!("topology.repl_chip_backlog_off", locale = &locale)
                };
                chips = chips.child(chip(backlog.to_string().into(), chip_fg));
                if repl.failover_in_progress() {
                    chips = chips.child(chip(
                        rust_i18n::t!(
                            "topology.repl_chip_failover_state",
                            state = repl.master_failover_state.clone(),
                            locale = &locale
                        )
                        .to_string()
                        .into(),
                        warning,
                    ));
                }
                rows.push(chips.into_any_element());

                if repl.replicas.is_empty() {
                    rows.push(
                        Label::new(i18n_topology(cx, "repl_no_replicas"))
                            .text_xs()
                            .text_color(muted)
                            .pl_6()
                            .into_any_element(),
                    );
                }
                let can_failover = can_write && supports_failover && failover_block.is_none();
                for replica in &repl.replicas {
                    let addr: SharedString = replica.addr.clone().into();
                    let state_color = if replica.state == "online" { success } else { warning };
                    let lag: SharedString = rust_i18n::t!(
                        "topology.repl_chip_lag",
                        secs = replica.lag_seconds,
                        bytes = format_lag_bytes(replica.lag_bytes),
                        locale = &locale
                    )
                    .to_string()
                    .into();
                    let mut row = h_flex()
                        .id(SharedString::from(format!("topo-repl-row-{}", replica.addr)))
                        .items_center()
                        .gap_2()
                        .pl_6()
                        .pr_2()
                        .py_1()
                        .rounded_md()
                        .hover(move |s| s.bg(hover))
                        .child(Label::new("↳").text_xs().text_color(muted))
                        .child(Label::new(addr.clone()).font_semibold())
                        .child(chip(replica.state.clone().into(), state_color))
                        .child(chip(lag, chip_fg))
                        .child(div().flex_1());
                    if can_failover {
                        let target = addr.clone();
                        let force_target = addr.clone();
                        row = row
                            .child(
                                Button::new(SharedString::from(format!("topo-repl-failover-{}", replica.addr)))
                                    .danger()
                                    .small()
                                    .label(failover_label.clone())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_replication_failover_dialog(target.clone(), false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("topo-repl-force-{}", replica.addr)))
                                    .ghost()
                                    .small()
                                    .label(force_failover_label.clone())
                                    .tooltip(force_failover_tooltip.clone())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_replication_failover_dialog(force_target.clone(), true, window, cx);
                                    })),
                            );
                    }
                    rows.push(row.into_any_element());
                }
            }
            ReplicationRole::Replica => {
                // The primary this server follows, then this server under it
                // with the state of the link.
                let link_up = repl.link_up();
                let link_key = if link_up { "repl_link_up" } else { "repl_link_down" };
                let link_color = if link_up { success } else { danger };
                rows.push(
                    node_row(
                        "topo-repl-primary",
                        "●",
                        muted,
                        repl.master_addr().into(),
                        primary_label.clone(),
                    )
                    .child(chip(i18n_topology(cx, link_key), link_color))
                    .into_any_element(),
                );
                let mut chips = h_flex().pl_6().pr_2().gap_1().flex_wrap();
                chips = chips.child(chip(
                    rust_i18n::t!(
                        "topology.repl_chip_offset",
                        offset = repl.replica_repl_offset,
                        locale = &locale
                    )
                    .to_string()
                    .into(),
                    chip_fg,
                ));
                if link_up {
                    chips = chips.child(chip(
                        rust_i18n::t!(
                            "topology.repl_chip_last_io",
                            secs = repl.master_last_io_seconds_ago,
                            locale = &locale
                        )
                        .to_string()
                        .into(),
                        chip_fg,
                    ));
                } else if repl.master_link_down_since_seconds > 0 {
                    chips = chips.child(chip(
                        rust_i18n::t!(
                            "topology.repl_chip_link_down_since",
                            secs = repl.master_link_down_since_seconds,
                            locale = &locale
                        )
                        .to_string()
                        .into(),
                        danger,
                    ));
                }
                if repl.master_sync_in_progress {
                    chips = chips.child(chip(i18n_topology(cx, "repl_chip_sync"), warning));
                }
                if repl.replica_read_only {
                    chips = chips.child(chip(i18n_topology(cx, "repl_chip_read_only"), chip_fg));
                }
                rows.push(
                    h_flex()
                        .id("topo-repl-self")
                        .items_center()
                        .gap_2()
                        .pl_6()
                        .pr_2()
                        .py_1()
                        .rounded_md()
                        .hover(move |s| s.bg(hover))
                        .child(Label::new("↳").text_xs().text_color(muted))
                        .child(Label::new(self_addr.clone()).font_semibold())
                        .child(chip(i18n_topology(cx, "repl_this_server"), chip_fg))
                        .into_any_element(),
                );
                rows.push(chips.pl_12().into_any_element());
            }
            ReplicationRole::Unknown => {}
        }

        // The changes: Replicate from (REPLICAOF host port), Promote
        // (REPLICAOF NO ONE, replica only) and Abort failover while one runs.
        let mut actions = v_flex().gap_2().pt_2();
        if !can_write {
            actions = actions.child(
                div()
                    .p_2()
                    .rounded(radius)
                    .border_1()
                    .border_color(warning)
                    .bg(warning.opacity(0.1))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Info).text_color(warning))
                            .child(
                                Label::new(i18n_topology(cx, "repl_readonly_banner"))
                                    .text_xs()
                                    .text_color(warning),
                            ),
                    ),
            );
        } else if let Some((command, status)) = replicaof_block {
            actions = actions.child(unavailable_chip(cx, command, status));
        } else {
            let mut form = h_flex()
                .gap_2()
                .items_center()
                .child(Label::new(i18n_topology(cx, "repl_make_replica_label")).text_sm())
                .child(Input::new(&self.replicaof_input).w(px(260.)))
                .child(
                    Button::new("topo-repl-make-replica")
                        .outline()
                        .small()
                        .label(i18n_topology(cx, "repl_make_replica_button"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            let value = this.replicaof_input.read(cx).value().to_string();
                            match parse_host_port(&value) {
                                Some((host, port)) => {
                                    this.form_error = None;
                                    this.open_replicaof_dialog(host.into(), port, window, cx);
                                }
                                None => {
                                    this.form_error = Some(i18n_topology(cx, "repl_err_addr"));
                                    cx.notify();
                                }
                            }
                        })),
                );
            if repl.role == ReplicationRole::Replica {
                let master_addr: SharedString = repl.master_addr().into();
                form = form.child(
                    Button::new("topo-repl-promote")
                        .danger()
                        .small()
                        .label(i18n_topology(cx, "repl_promote_button"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_promote_dialog(master_addr.clone(), window, cx);
                        })),
                );
            }
            if repl.failover_in_progress() {
                form = form.child(
                    Button::new("topo-repl-abort-failover")
                        .outline()
                        .small()
                        .label(i18n_topology(cx, "repl_abort_failover_button"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.server_state.update(cx, |state, cx| state.failover_abort(cx));
                        })),
                );
            }
            actions = actions.child(form);
            if repl.role == ReplicationRole::Primary {
                if !supports_failover {
                    actions = actions.child(
                        Label::new(i18n_topology(cx, "repl_failover_needs_version"))
                            .text_xs()
                            .text_color(muted)
                            .whitespace_normal(),
                    );
                } else if let Some(status) = failover_block {
                    actions = actions.child(unavailable_chip(cx, ServerCommand::Failover, status));
                }
            }
            if let Some(error) = self.form_error.clone() {
                actions = actions.child(Label::new(error).text_xs().text_color(danger));
            }
        }

        v_flex()
            .gap_2()
            .child(Label::new(summary).text_sm().text_color(muted))
            .children(rows)
            .child(actions)
            .into_any_element()
    }

    /// `REPLICAOF host port` behind the alert dialog: the body says the
    /// dataset goes, production tags escalate it.
    pub(super) fn open_replicaof_dialog(
        &mut self,
        host: SharedString,
        port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "repl_make_replica_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.repl_make_replica_confirm_body",
            addr = format!("{host}:{port}"),
            locale = &locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let host = host.clone();
                server_state.update(cx, |state, cx| state.replicaof(host, port, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// `REPLICAOF NO ONE` behind the alert dialog.
    pub(super) fn open_promote_dialog(
        &mut self,
        master_addr: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n_topology(cx, "repl_promote_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.repl_promote_confirm_body",
            addr = master_addr.as_ref(),
            locale = &locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| state.replicaof_no_one(cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// `FAILOVER TO host port [FORCE] TIMEOUT` behind the alert dialog; the
    /// body names the timeout, and what FORCE gives up.
    pub(super) fn open_replication_failover_dialog(
        &mut self,
        target: SharedString,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, port)) = parse_host_port(&target) else {
            return;
        };
        let (title_key, body_key) = if force {
            (
                "repl_force_failover_confirm_title",
                "topology.repl_force_failover_confirm_body",
            )
        } else {
            ("repl_failover_confirm_title", "topology.repl_failover_confirm_body")
        };
        let title = i18n_topology(cx, title_key);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            body_key,
            addr = target.as_ref(),
            timeout = FAILOVER_TIMEOUT_MS / 1000,
            locale = &locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let host: SharedString = host.into();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let host = host.clone();
                server_state.update(cx, |state, cx| state.failover(host, port, force, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}
