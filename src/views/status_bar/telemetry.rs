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

//! The bar's right side: the link's health and latency, and the metric chips
//! that open their panels.

use super::*;

/// Fixed mono width for the latency label (value left-aligned, padding
/// trailing): 5 chars covers "999ms" / "1.23s" / "--". The heartbeat
/// refreshes the value every 2s and the
/// telemetry cluster is right-anchored, so a "9ms" ⇄ "10ms" flip would resize
/// the (now native, center-justified) latency button and shove the Connected
/// chip beside it on each beat; padding the value to a fixed mono width keeps
/// the button width constant instead.
const LATENCY_LABEL_CHARS: usize = 5;

/// The green of a healthy link — the latency figure and the "Connected"
/// label. One value could not serve both themes: `#69b083` is 7.7:1 on the
/// dark bar and 2.6:1 on the light one, so light takes a darker green
/// (`#1a7f37`, 5.1:1 on white).
#[inline]
fn healthy_color(dark: bool) -> Hsla {
    rgb(if dark { 0x69b083 } else { 0x1a7f37 }).into()
}

/// Formats the latency string and determines the color based on the delay.
#[inline]
pub(super) fn format_latency(latency: Option<Duration>, cx: &Context<ZedisStatusBar>) -> (SharedString, Hsla) {
    let Some(latency) = latency else {
        return ("--".into(), cx.theme().primary);
    };
    let ms = latency.as_millis();
    let theme = cx.theme();
    let color = match ms {
        // Healthy latency uses the same green as the "Connected" label.
        0..50 => healthy_color(theme.is_dark()),
        50..500 => theme.yellow,
        _ => theme.red,
    };
    let label = if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", ms as f64 / 1000.0)
    };
    (label.into(), color)
}

/// Inputs for a clickable status-bar metric chip (icon + value → tool page).
struct MetricChip {
    id: &'static str,
    icon: CustomIconName,
    label: SharedString,
    label_color: Hsla,
    icon_color: Hsla,
    tooltip: SharedString,
    view: ServerView,
    /// Set when the destination panel can't work on this server: the chip is
    /// disabled and this replaces the tooltip.
    disabled_reason: Option<SharedString>,
    /// What the number counts — "clients", "slow" — and the colour to say
    /// it in, when the bar has room for it ([`ZedisStatusBar::shows_units`]).
    unit: Option<(SharedString, Hsla)>,
}

impl ZedisStatusBar {
    /// Render the right-hand telemetry cluster: a "Connected" health indicator
    /// (with one-click reconnect when the link is down) followed by the live
    /// metric chips — latency / memory / clients / slow log.
    pub(super) fn render_telemetry(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let shows_units = self.shows_units(window, cx);
        let unit_color = cx.theme().muted_foreground;
        let unit = |key: &str| shows_units.then(|| (i18n_status_bar(cx, key), unit_color));
        // Health icon + label. A chain link (connected) vs a broken one (link
        // down) carries the state in the *shape*, not just the color — a bare
        // colored dot was too easy to miss. Colors mirror the latency palette so
        // the cluster reads as one health unit.
        //
        // `Unknown` (still connecting) keeps the intact link at the muted color:
        // nothing is broken yet, so a red-flavoured "unlink" would over-alarm.
        let (health_color, health_icon, health_label) = match server_state.health {
            ConnectionHealth::Connected => (
                healthy_color(cx.theme().is_dark()),
                CustomIconName::Link,
                i18n_status_bar(cx, "conn_connected"),
            ),
            ConnectionHealth::Reconnecting => (
                cx.theme().yellow,
                CustomIconName::Unlink,
                i18n_status_bar(cx, "conn_reconnecting"),
            ),
            ConnectionHealth::Offline => (
                cx.theme().red,
                CustomIconName::Unlink,
                i18n_status_bar(cx, "conn_offline"),
            ),
            ConnectionHealth::Unknown => (
                cx.theme().muted_foreground,
                CustomIconName::Link,
                i18n_status_bar(cx, "conn_connecting"),
            ),
        };
        // When the link is down the icon doubles as a one-click reconnect
        // affordance. The heartbeat alone leaves `server_status` Idle, so a
        // plain re-select would no-op — `reconnect()` forces the reload.
        let is_link_down = matches!(
            server_state.health,
            ConnectionHealth::Offline | ConnectionHealth::Reconnecting
        );
        // Connected → the icon doubles as a one-click *disconnect*; when down it's
        // a *reconnect*. `Unknown` (still connecting) stays inert.
        let is_connected = server_state.health == ConnectionHealth::Connected;
        let icon_clickable = is_link_down || is_connected;
        let health_tooltip = if is_link_down {
            let hint = i18n_status_bar(cx, "conn_reconnect_hint");
            let reason = i18n_status_bar(cx, server_state.last_connection_error.reason_key());
            format!("{reason} · {hint}").into()
        } else if is_connected {
            format!("{} · {}", health_label, i18n_status_bar(cx, "conn_disconnect_hint")).into()
        } else {
            health_label.clone()
        };
        // When the link is down the cached latency is stale and misleading
        // (a green "5ms" beside a red dot), so blank it to a muted "--".
        // Chips whose panel can't work on this server are disabled with the
        // reason as tooltip (computed up front: the closure borrows `cx`).
        let features = server_state.features.clone();
        let block = |view: ServerView| -> Option<SharedString> {
            features
                .first_unusable(view.required_commands())
                .map(|(command, status)| command_status_label(cx, command, status))
        };
        // The memory and clients chips show numbers that are `INFO`'s. For
        // a user who may not run it they read "--", and the tooltip says why
        // where it would have named the number.
        let info_reason = features
            .first_unusable(&[ServerCommand::Info])
            .map(|(command, status)| command_status_label(cx, command, status));
        let stat_hint = |key: &str| info_reason.clone().unwrap_or_else(|| i18n_status_bar(cx, key));
        let chip_blocks = [
            (ServerView::Metrics, block(ServerView::Metrics)),
            (ServerView::MemoryAnalysis, block(ServerView::MemoryAnalysis)),
            (ServerView::Clients, block(ServerView::Clients)),
            (ServerView::Slowlog, block(ServerView::Slowlog)),
        ];
        let chip_block = |view: ServerView| -> Option<SharedString> {
            chip_blocks
                .iter()
                .find(|(v, _)| *v == view)
                .and_then(|(_, reason)| reason.clone())
        };
        let (latency_text, latency_color) = if server_state.health == ConnectionHealth::Offline {
            (SharedString::from("--"), cx.theme().muted_foreground)
        } else {
            server_state.latency.clone()
        };
        // Left-aligned, fixed mono width: the padding trails the value so the
        // button width stays constant (no resize jogging the Connected chip each
        // beat) while the number sits at the left edge of its slot.
        let latency_text: SharedString =
            format!("{:<width$}", latency_text.as_ref(), width = LATENCY_LABEL_CHARS).into();
        let status_text = status_text_color(cx.theme().is_dark());
        let link_label = self.bar_fit(window, cx) < BarFit::LinkIconOnly;
        ZedisDivider::new()
            .child(
                // One native ghost button covering icon + label, so the whole
                // "Connected" chip is the click target (disconnect when up /
                // reconnect when down) with the same hover background as the
                // metric chips and the left-side buttons — the label used to sit
                // outside the button and wasn't clickable. The icon keeps its own
                // health color (`.text_color` colors only the label); inert while
                // still connecting.
                Button::new("zedis-status-bar-conn-health")
                    .ghost()
                    .small()
                    // Dense status-bar row: small's default px_3 leaves too much
                    // air between adjacent chips; compact → px_1p5.
                    .compact()
                    .disabled(!icon_clickable)
                    // Where the bar is short of room the icon says it alone,
                    // and the tooltip still in words.
                    .icon(
                        Icon::new(health_icon)
                            .when(link_label, |icon| icon.mr_1())
                            .text_color(health_color),
                    )
                    .when(link_label, |button| button.label(health_label))
                    .text_color(status_text)
                    .tooltip(health_tooltip)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            if is_connected {
                                state.disconnect(cx);
                            } else {
                                state.reconnect(cx);
                            }
                        });
                    })),
            )
            // The link is down because a script has the server: reconnecting
            // cannot help, the kill can — offered right where the reason is
            // shown, on a connection a BUSY server still answers.
            .when(
                kills_runaway_scripts()
                    && is_link_down
                    && server_state.last_connection_error == ConnectionErrorKind::Busy,
                |this| {
                    this.child(
                        Button::new("zedis-status-bar-kill-script")
                            .danger()
                            .small()
                            .compact()
                            .label(i18n_common(cx, "kill_script_button"))
                            .tooltip(i18n_common(cx, "kill_script_tooltip"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state
                                    .update(cx, |state, cx| state.kill_running_script(KillTarget::Script, cx));
                            })),
                    )
                },
            )
            .child(
                h_flex()
                    .items_center()
                    // Chip internal padding already separates them; gap_1 is
                    // enough once buttons are compact (gap_2 looked sparse).
                    .gap_1()
                    // Whole chip is clickable (icon + value), with a tooltip that
                    // names both the metric and the destination page.
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Metrics),
                        id: "zedis-status-bar-server-metrics",
                        unit: None,
                        icon: CustomIconName::Activity,
                        label: latency_text,
                        label_color: latency_color,
                        icon_color: if server_state.active_alert_count > 0 {
                            cx.theme().yellow
                        } else {
                            status_text
                        },
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            i18n_status_bar(cx, "metric_latency_hint"),
                            i18n_status_bar(cx, "toggle_metrics_tooltip")
                        )),
                        view: ServerView::Metrics,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::MemoryAnalysis),
                        id: "zedis-status-bar-server-memory-analysis",
                        unit: None,
                        icon: CustomIconName::MemoryStick,
                        label: server_state.used_memory.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            stat_hint("metric_memory_hint"),
                            i18n_status_bar(cx, "toggle_memory_analysis_tooltip")
                        )),
                        view: ServerView::MemoryAnalysis,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Clients),
                        id: "zedis-status-bar-clients",
                        unit: unit("unit_clients"),
                        icon: CustomIconName::AudioWaveform,
                        label: server_state.clients.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            // The Metrics page's card says the same: one label.
                            info_reason
                                .clone()
                                .unwrap_or_else(|| i18n_common(cx, "clients_connected_blocked")),
                            i18n_status_bar(cx, "toggle_clients_tooltip")
                        )),
                        view: ServerView::Clients,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Slowlog),
                        id: "zedis-status-bar-server-slow-logs",
                        unit: unit("unit_slow"),
                        icon: CustomIconName::Snail,
                        label: server_state.slow_log_tips.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            i18n_status_bar(cx, "slowlog_stat_tooltip"),
                            i18n_status_bar(cx, "toggle_slowlog_tooltip")
                        )),
                        view: ServerView::Slowlog,
                    })),
            )
    }

    /// Icon + value chip that opens a tool page on click. A native ghost
    /// `Button` (not a hand-rolled div) so the whole chip — icon *and* value —
    /// is the hit target and gets the same hover-background treatment as the
    /// terminal / readonly / tools buttons on the left, rather than only
    /// flipping the cursor. The icon keeps its own color (muted, or the
    /// latency palette); `.text_color` colors just the value label.
    fn metric_chip(chip: MetricChip) -> impl IntoElement {
        let MetricChip {
            id,
            icon,
            label,
            label_color,
            icon_color,
            tooltip,
            view,
            disabled_reason,
            unit,
        } = chip;
        // A disabled `Button` still shows its tooltip, so the reason the
        // panel is unavailable replaces the destination hint.
        let disabled = disabled_reason.is_some();
        let tooltip = disabled_reason.unwrap_or(tooltip);
        Button::new(id)
            .ghost()
            .small()
            // Status-bar metric chips sit edge-to-edge; compact halves the
            // horizontal padding (px_3 → px_1p5) so the cluster reads dense.
            .compact()
            .icon(Icon::new(icon).mr_1().text_color(icon_color))
            .label(label)
            .text_color(label_color)
            .when_some(unit, |this, (unit, color)| {
                this.child(Label::new(unit).text_xs().text_color(color).ml_1())
            })
            .disabled(disabled)
            .tooltip(tooltip)
            .on_click(move |_, _window, cx| {
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.toggle_view(view, cx);
                });
            })
    }
}
