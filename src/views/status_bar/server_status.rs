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

//! The bar's left side: the server's context — its tag, the node counts
//! and their tooltip, the database, read-only, the Tools menu and the key
//! count.

use super::*;

/// The server-status tooltip's first line: which product this is, in its own
/// version.
///
/// `version` is the one the version gates are asked. For Redis and Valkey
/// that is the product's own, and the line is `Redis 8.6.1` / `Valkey 9.0.3`.
/// A server that only speaks Redis has a second one — a Dragonfly 2.0.2
/// answers `redis_version:7.4.0` — and was labelled "Redis 7.4.0", the name
/// and number of another product. Its own comes first, and the Redis it
/// claims stays beside it because that is what decides which commands the
/// app expects of it: `Dragonfly 2.0.2 (Redis 7.4.0)`.
pub(crate) fn product_line(flavor: ServerFlavor, own_version: &str, version: &str) -> String {
    let label = flavor.label();
    if own_version.is_empty() || own_version == version {
        format!("{label} {version}")
    } else {
        format!("{label} {own_version} (Redis {version})")
    }
}

/// Formats the node count only (`masters / replicas`). Redis version lives
/// in the nodes tooltip so the bar stays short.
#[inline]
pub(super) fn format_nodes(nodes: (usize, usize)) -> SharedString {
    format!("{} / {}", nodes.0, nodes.1).into()
}

/// Build the multi-line tooltip body. `replicas` is the dynamic per-replica
/// state from the most recent `INFO replication` heartbeat — used to splice
/// `lag X / Ys` onto the matching topology line. Falls back gracefully when
/// the lag map has nothing for a given replica address (e.g. a fail node).
///
/// `version` is shown here (not in the bar label) so the compact
/// `masters / replicas` chip stays short.
#[inline]
pub(super) fn format_nodes_description(
    description: Arc<RedisClientDescription>,
    replicas: &[ReplicaInfo],
    version: &str,
    cx: &Context<ZedisStatusBar>,
) -> SharedString {
    let t = i18n_sidebar(cx, "server_type");
    let master_nodes = i18n_sidebar(cx, "master_nodes");
    let slave_nodes = i18n_sidebar(cx, "slave_nodes");
    let modules_label = i18n_sidebar(cx, "modules");
    let topology_label = i18n_sidebar(cx, "topology");
    let mut messages = Vec::with_capacity(6);

    // One product line: `Valkey 9.0.3` / `Redis 8.6.1`. The version was
    // read from `valkey_version` when the server is Valkey, so the old
    // "Valkey: Yes" + "Redis version: 9.0.3" pair labeled a Valkey version
    // as Redis; the product name needs no translation and says both facts
    // at once.
    if !version.is_empty() {
        messages.push(product_line(description.flavor, &description.flavor_version, version));
    } else if description.flavor != ServerFlavor::Redis {
        messages.push(description.flavor.label().to_string());
    }
    messages.push(format!("{t}: {}", description.server_type.as_str()));
    if description.topology.is_empty() {
        // Fallback for standalone or any client without grouped topology data.
        messages.push(format!("{master_nodes}: {}", description.master_nodes));
        if !replicas.is_empty() {
            // Plain (non-Sentinel) master-replica: node discovery only lists
            // the configured node, but the heartbeat's `INFO replication`
            // still reports the attached replicas — render them here with
            // the same lag/state detail the topology branch shows, instead
            // of silently dropping them.
            let mut lines: Vec<String> = vec![format!("{slave_nodes}:")];
            for lag in replicas {
                let mut line = format!(
                    "  ↳ {} replica  lag {} / {}s",
                    lag.addr,
                    format_lag_bytes(lag.lag_bytes),
                    lag.lag_seconds
                );
                // Surface non-steady states (wait_bgsave / send_bulk / etc.).
                if !lag.state.is_empty() && lag.state.as_ref() != "online" {
                    line.push_str("  ");
                    line.push_str(lag.state.as_ref());
                }
                lines.push(line);
            }
            messages.push(lines.join("\n"));
        } else if !description.slave_nodes.is_empty() {
            messages.push(format!("{slave_nodes}: {}", description.slave_nodes));
        }
    } else {
        let mut lines: Vec<String> = vec![format!("{topology_label}:")];
        for tm in description.topology.iter() {
            let mut header = format!("{} {} master", tm.master.role_marker, tm.master.addr);
            if !tm.master.annotation.is_empty() {
                header.push_str("  ");
                header.push_str(tm.master.annotation.as_ref());
            }
            lines.push(header);
            for r in &tm.replicas {
                let mut line = format!("  {} {} replica", r.role_marker, r.addr);
                if let Some(lag) = replicas.iter().find(|i| i.addr == r.addr) {
                    line.push_str(&format!(
                        "  lag {} / {}s",
                        format_lag_bytes(lag.lag_bytes),
                        lag.lag_seconds
                    ));
                    // Surface non-steady states (wait_bgsave / send_bulk / etc.).
                    // "online" is the silent default and would just be noise.
                    if !lag.state.is_empty() && lag.state.as_ref() != "online" {
                        line.push_str("  ");
                        line.push_str(lag.state.as_ref());
                    }
                }
                lines.push(line);
            }
        }
        messages.push(lines.join("\n"));
    }
    if !description.modules.is_empty() {
        messages.push(format!("{modules_label}: {}", description.modules));
    }
    messages.join("\n").into()
}

impl ZedisStatusBar {
    /// DB-dropdown menu width, hugging the current content: the default
    /// (`Length::Auto`) follows the narrow "DB N ▾" trigger and clips the
    /// key counts, while any fixed width leaves blank space when counts
    /// are small or absent. Sized from the widest count in the current
    /// snapshot instead.
    fn db_menu_width(&self) -> Pixels {
        // px_2 row padding (16) + label column + gap to the check icon (4)
        // + xsmall check icon (~14) + rounding slack (4).
        let mut width = 16. + DB_LABEL_COL_W + 4. + 14. + 4.;
        let max_keys = self.db_key_counts.iter().copied().max().unwrap_or(0);
        if max_keys > 0 {
            // gap_3 (12) + the widest grouped count at the mono advance.
            width += 12. + group_thousands(max_keys).len() as f32 * DB_MENU_CHAR_W;
        }
        px(width)
    }

    /// Render the server status.
    pub(super) fn render_server_status(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nodes_unit = self
            .shows_units(window, cx)
            .then(|| (i18n_status_bar(cx, "unit_nodes"), cx.theme().muted_foreground));
        let server_state = &self.state.server_state;
        let is_completed = server_state.scan_finished;
        // Read live, not from the snapshot: a scan round starts on this
        // button's own press, which is no server event.
        let scanning = self.server_state.read(cx).scanning();
        let nodes_description = server_state.nodes_description.clone();
        let terminal_tooltip = with_hot_key("terminal", &i18n_status_bar(cx, "toggle_terminal_tooltip"));
        let readonly_tooltip = i18n_status_bar(cx, "toggle_readonly_tooltip");
        let tag_text = server_state.tag.clone();
        let tag_chip = resolve_tag_chip(server_state.tag_color_key.as_deref(), cx.theme().is_dark());
        let is_replica = server_state.is_replica;
        let supports_search = server_state.supports_search;
        let supports_acl = server_state.supports_acl;
        let supports_functions = server_state.supports_functions;
        let supports_topology = server_state.supports_topology;
        let readonly = self.readonly;
        let features = server_state.features.clone();
        let gates = ToolsMenuGates {
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
            readonly,
            features,
        };
        let status_text = cx.theme().muted_foreground;
        ZedisDivider::new()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .when(!tag_text.is_empty() && tag_chip.is_some(), |this| {
                        let (bg, fg) = tag_chip.unwrap_or_else(|| (gpui::black(), gpui::white()));
                        this.child(
                            div()
                                .px_1p5()
                                .rounded_sm()
                                .bg(bg)
                                .child(Label::new(tag_text).text_xs().text_color(fg)),
                        )
                    })
                    // Quiet outline badge when the connection points at a
                    // replica (`role:slave`) — without it the only signal is a
                    // bare `-READONLY` error on the first write.
                    .when(is_replica, |this| {
                        let replica_tooltip = i18n_status_bar(cx, "replica_badge_tooltip");
                        this.child(
                            div()
                                .id("zedis-status-bar-replica-badge")
                                .px_1p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().warning)
                                .tooltip(move |window, cx| Tooltip::new(replica_tooltip.clone()).build(window, cx))
                                .child(
                                    Label::new(i18n_status_bar(cx, "replica_badge"))
                                        .text_xs()
                                        .text_color(cx.theme().warning),
                                ),
                        )
                    })
                    .child(
                        Button::new("zedis-status-bar-server-terminal")
                            .ghost()
                            .small()
                            .tooltip(terminal_tooltip)
                            .icon(Icon::new(IconName::SquareTerminal).text_color(status_text))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.toggle_terminal(cx);
                                });
                            })),
                    )
                    .when(self.databases > 1, |this| {
                        // `appearance(false)`: drop the bordered input chrome so it
                        // reads as plain "DB N ▾" text that inherits the muted
                        // status-bar color (matches the design), not a bright box.
                        // The tooltip names what the per-row numbers are — the
                        // dropdown shows bare counts with no unit, and on a
                        // multi-master setup they are sums across nodes.
                        let db_tooltip = i18n_status_bar(cx, "db_tooltip");
                        let menu_open = self.db_menu_open;
                        this.child(
                            div()
                                .id("zedis-status-bar-db-select")
                                .when(!menu_open, |this| {
                                    this.tooltip(move |window, cx| Tooltip::new(db_tooltip.clone()).build(window, cx))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _window, cx| {
                                        this.db_menu_open = true;
                                        cx.notify();
                                    }),
                                )
                                .child(
                                    Select::new(&self.db_state)
                                        .small()
                                        .appearance(false)
                                        .menu_width(self.db_menu_width()),
                                ),
                        )
                    })
                    // Clients paused — by this app (remembered), or by anyone
                    // where the server says so (Valkey 8.1+): the state in
                    // which every write hangs and nothing else on screen
                    // explains why. The click lifts it.
                    .when_some(self.server_state.read(cx).client_pause(), |this, pause| {
                        let seconds = pause.remaining.as_secs().to_string();
                        let label = i18n_status_bar(cx, "paused_chip")
                            .replace("%{actions}", &pause.actions)
                            .replace("%{seconds}", &seconds);
                        let tooltip: SharedString = if pause.from_server {
                            i18n_status_bar(cx, "paused_tooltip_server")
                                .replace("%{actions}", &pause.actions)
                                .replace("%{reason}", pause.reason.as_deref().unwrap_or("-"))
                                .replace("%{seconds}", &seconds)
                                .into()
                        } else {
                            i18n_status_bar(cx, "paused_tooltip_local")
                                .replace("%{actions}", &pause.actions)
                                .replace("%{seconds}", &seconds)
                                .into()
                        };
                        this.child(
                            Button::new("zedis-status-bar-client-pause")
                                .warning()
                                .small()
                                .compact()
                                .icon(Icon::new(CustomIconName::Clock3))
                                .label(label)
                                .tooltip(tooltip)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.server_state.update(cx, |state, cx| state.unpause_clients(cx));
                                })),
                        )
                    })
                    .child({
                        // A locked entry's lock is a window, not a switch (ADR
                        // 14): closed, the button asks the lock's question and
                        // opens it for a while; open, it shows what is left and
                        // closes it. Any other entry keeps the plain toggle.
                        let (windowed, remaining) = {
                            let state = self.server_state.read(cx);
                            // Not for an account or ACL user that may not
                            // write at all: there is no window to open.
                            (
                                state.write_locked() && !state.strict_readonly(),
                                state.unlock_remaining(),
                            )
                        };
                        let minutes_left = remaining.map(|left| left.as_secs().div_ceil(60));
                        let tooltip: SharedString = match (windowed, minutes_left) {
                            (true, Some(minutes)) => i18n_status_bar(cx, "unlocked_tooltip")
                                .replace("%{minutes}", &minutes.to_string())
                                .into(),
                            (true, None) => i18n_status_bar(cx, "write_locked_tooltip")
                                .replace("%{minutes}", &(WRITE_UNLOCK_SECS / 60).to_string())
                                .into(),
                            (false, _) => readonly_tooltip,
                        };
                        Button::new("zedis-status-bar-server-toggle-readonly")
                            .ghost()
                            .small()
                            .tooltip(tooltip)
                            .when(self.readonly, |this| {
                                this.icon(Icon::new(CustomIconName::Lock).text_color(status_text))
                            })
                            .when(!self.readonly, |this| {
                                this.icon(Icon::new(CustomIconName::LockOpen).text_color(status_text))
                            })
                            .when_some(minutes_left.filter(|_| windowed), |this, minutes| {
                                this.label(
                                    i18n_status_bar(cx, "unlocked_chip").replace("%{minutes}", &minutes.to_string()),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if !windowed {
                                    this.server_state.update(cx, |state, cx| state.toggle_readonly(cx));
                                    return;
                                }
                                if minutes_left.is_some() {
                                    this.server_state.update(cx, |state, cx| state.lock_writes(cx));
                                    return;
                                }
                                let Ok(server) = get_server(this.server_state.read(cx).server_id()) else {
                                    return;
                                };
                                let server_state = this.server_state.clone();
                                confirm_dangerous_command(
                                    &server,
                                    server_state.read(cx).db(),
                                    &DangerKind::WriteLocked,
                                    None,
                                    window,
                                    cx,
                                    move |_, cx| {
                                        server_state.update(cx, |state, cx| state.unlock_writes(cx));
                                    },
                                );
                            }))
                    })
                    .child(
                        Button::new("zedis-status-bar-tools")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Menu).text_color(status_text))
                            .tooltip(i18n_status_bar(cx, "tools_tooltip"))
                            // Status bar sits at the bottom, so open the menu
                            // upward (its bottom edge anchored to the button).
                            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |this, window, cx| {
                                Self::render_tools_menu(this, &gates, window, cx)
                            }),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("zedis-status-bar-scan-more")
                            .ghost()
                            .small()
                            .disabled(is_completed)
                            .loading(scanning && !is_completed)
                            .tooltip(if is_completed {
                                i18n_status_bar(cx, "scan_completed")
                            } else {
                                i18n_status_bar(cx, "scan_more_keys")
                            })
                            .mr_1()
                            .icon(Icon::new(CustomIconName::ChevronsDown).text_color(status_text))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.scan_next(cx);
                                });
                                cx.notify();
                            })),
                    )
                    .child({
                        // Two bare numbers: the tooltip says which is which.
                        let key_count_tooltip = i18n_status_bar(cx, "key_count_tooltip");
                        div()
                            .id("zedis-status-bar-key-count")
                            .mr_2()
                            .tooltip(move |window, cx| Tooltip::new(key_count_tooltip.clone()).build(window, cx))
                            .child(Label::new(server_state.size.clone()).text_color(status_text))
                    })
                    .child(
                        div()
                            .child(
                                h_flex()
                                    .items_center()
                                    .child(Icon::new(CustomIconName::Network).text_color(status_text).mr_1())
                                    .child(Label::new(server_state.nodes.clone()).text_color(status_text))
                                    .when_some(nodes_unit, |this, (unit, color)| {
                                        this.child(Label::new(unit).text_xs().text_color(color).ml_1())
                                    }),
                            )
                            .id("zedis-servers")
                            .tooltip(move |window, cx| Tooltip::new(nodes_description.clone()).build(window, cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A product is named in its own version; the Redis it claims to speak
    /// stays beside it only where that is a different number.
    #[test]
    fn the_status_tooltip_names_the_product_in_its_own_version() {
        assert_eq!(product_line(ServerFlavor::Redis, "", "8.6.1"), "Redis 8.6.1");
        assert_eq!(product_line(ServerFlavor::Valkey, "9.0.3", "9.0.3"), "Valkey 9.0.3");
        assert_eq!(
            product_line(ServerFlavor::Dragonfly, "2.0.2", "7.4.0"),
            "Dragonfly 2.0.2 (Redis 7.4.0)"
        );
        // A product that names no version of its own keeps the one it claims.
        assert_eq!(product_line(ServerFlavor::KeyDb, "", "6.3.4"), "KeyDB 6.3.4");
    }
}
