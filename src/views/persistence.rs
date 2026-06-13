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

//! Persistence management panel.
//!
//! Surfaces the RDB/AOF state pulled from `INFO persistence` and lets
//! the user trigger `BGSAVE` / `BGREWRITEAOF`. Action buttons are gated
//! by three states:
//!   * read-only mode → disabled with tooltip;
//!   * Redis still loading from disk → disabled (and a banner explains);
//!   * a fork is already running → disabled, showing elapsed seconds.
//!
//! Both action paths go through `ZedisDialog::new_alert` so a stray
//! click never forks a 50 GB instance — the dialog spells out the
//! latency-spike risk, and the high-risk-tag treatment is layered in by
//! running the body through `escalate_dangerous_body` (PROD-tagged
//! servers get an extra warning appended).

use crate::connection::get_server;
use crate::helpers::{format_duration, unix_ts};
use crate::states::{
    RedisMetrics, Route, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, escalate_dangerous_body,
    i18n_common, i18n_persistence,
};
use gpui::{Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use std::time::Duration;
use zedis_ui::{ZedisDialog, ZedisSkeletonLoading};

pub struct ZedisPersistence {
    title: SharedString,
    server_state: Entity<ZedisServerState>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisPersistence {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id();
        let name = get_server(server_id)
            .map(|s| s.name)
            .unwrap_or_else(|_| "--".to_string());
        let nodes_description = state.nodes_description();
        let title = format!(
            "{name} - {}({})",
            nodes_description.server_type, nodes_description.master_nodes
        )
        .into();

        // Re-render whenever `refresh_redis_info` lands new metrics
        // (every 2s via heartbeat, plus the eager refresh kicked by
        // bgsave/bgrewriteaof on success).
        let subscriptions = vec![cx.subscribe(&server_state, |_this, _state, event, cx| {
            if matches!(
                event,
                ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
            ) {
                cx.notify();
            }
        })];

        Self {
            title,
            server_state,
            _subscriptions: subscriptions,
        }
    }

    fn metrics(&self, cx: &Context<Self>) -> Option<RedisMetrics> {
        self.server_state.read(cx).redis_info().map(|i| i.metrics)
    }

    fn readonly(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).readonly()
    }

    // ── Header ─────────────────────────────────────────────────────────
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .h(px(40.))
            .px_4()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("persistence-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
                                });
                            }),
                    )
                    .child(Label::new(i18n_persistence(cx, "title")).font_semibold())
                    .child(Label::new(self.title.clone()).text_color(theme.muted_foreground)),
            )
    }

    // ── Banners ────────────────────────────────────────────────────────
    fn render_loading_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .mx_4()
            .my_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.warning)
            .bg(theme.warning.opacity(0.1))
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(IconName::Info).text_color(theme.warning))
                    .child(Label::new(i18n_persistence(cx, "loading_state")).text_color(theme.warning)),
            )
    }

    fn render_failure_banner(&self, message: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .mx_4()
            .my_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger)
            .bg(theme.danger.opacity(0.1))
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(IconName::CircleX).text_color(theme.danger))
                    .child(Label::new(message).text_color(theme.danger)),
            )
    }

    // ── Status cards ───────────────────────────────────────────────────
    /// Render a single labelled status card. Mirrors the layout used by
    /// `ZedisMetrics::render_stat_card` so the panels look at-home
    /// alongside each other.
    fn render_stat_card(
        &self,
        cx: &mut Context<Self>,
        label: SharedString,
        value: SharedString,
        hint: Option<SharedString>,
        accent: Option<gpui::Hsla>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let value_color = accent.unwrap_or(theme.foreground);
        v_flex()
            .flex_1()
            .min_w(px(180.))
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .p_4()
            .gap_1()
            .child(Label::new(label).text_sm().text_color(theme.muted_foreground))
            .child(Label::new(value).font_semibold().text_color(value_color))
            .when_some(hint, |this, h| {
                this.child(Label::new(h).text_xs().text_color(theme.muted_foreground))
            })
    }

    /// Render "Last RDB save" — relative time ("2 min ago" / "Never"),
    /// coloured by the success flag.
    fn render_last_save_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (value, accent) = if m.rdb_last_save_time <= 0 {
            (i18n_persistence(cx, "state_never"), Some(theme.muted_foreground))
        } else {
            let elapsed = unix_ts().saturating_sub(m.rdb_last_save_time).max(0) as u64;
            let value: SharedString = if elapsed < 5 {
                i18n_persistence(cx, "state_now")
            } else {
                let dur = format_duration(Duration::from_secs(elapsed));
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                rust_i18n::t!("persistence.state_ago", ago = dur, locale = locale)
                    .to_string()
                    .into()
            };
            (value, None)
        };
        let hint = if m.rdb_last_bgsave_success {
            i18n_persistence(cx, "state_ok")
        } else {
            i18n_persistence(cx, "state_failed")
        };
        self.render_stat_card(cx, i18n_persistence(cx, "card_last_save"), value, Some(hint), accent)
    }

    fn render_changes_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // Highlight non-zero pending changes with the warning hue when
        // they grow past a token threshold — gives a quick "RDB is
        // getting stale" visual.
        let accent = if m.rdb_changes_since_last_save > 0 {
            Some(theme.warning)
        } else {
            None
        };
        self.render_stat_card(
            cx,
            i18n_persistence(cx, "card_changes"),
            m.rdb_changes_since_last_save.to_string().into(),
            None,
            accent,
        )
    }

    fn render_aof_status_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (value, accent) = if m.aof_enabled {
            (i18n_persistence(cx, "aof_enabled"), None)
        } else {
            (i18n_persistence(cx, "aof_disabled"), Some(theme.muted_foreground))
        };
        self.render_stat_card(cx, i18n_persistence(cx, "card_aof_status"), value, None, accent)
    }

    fn render_aof_size_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let current = humansize::format_size(
            m.aof_current_size,
            humansize::FormatSizeOptions::default().decimal_places(1),
        );
        let hint: Option<SharedString> = if m.aof_base_size == 0 {
            Some(i18n_persistence(cx, "aof_no_baseline"))
        } else {
            let ratio = m.aof_current_size as f64 / m.aof_base_size as f64;
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            Some(
                rust_i18n::t!(
                    "persistence.aof_growth",
                    ratio = format!("{:.2}", ratio),
                    locale = locale
                )
                .to_string()
                .into(),
            )
        };
        self.render_stat_card(cx, i18n_persistence(cx, "card_aof_size"), current.into(), hint, None)
    }

    fn render_stat_grid(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_4()
            .gap_2()
            .flex_wrap()
            // Stretch every card to the tallest in the row so a card with a
            // hint line (e.g. "Last RDB save") doesn't leave its hint-less
            // neighbours visibly shorter.
            .items_stretch()
            .child(self.render_last_save_card(m, cx))
            .child(self.render_changes_card(m, cx))
            .child(self.render_aof_status_card(m, cx))
            .when(m.aof_enabled, |this| this.child(self.render_aof_size_card(m, cx)))
    }

    // ── Action cards ───────────────────────────────────────────────────
    /// Render one action card (BGSAVE or BGREWRITEAOF). The button is
    /// disabled when any of `readonly`, `loading`, or `in_progress` is
    /// true; we pick a tooltip that explains *which* gate is blocking
    /// so the user can act on it rather than just seeing a dead button.
    #[allow(clippy::too_many_arguments)]
    fn render_action_card(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        title_key: &'static str,
        description_key: &'static str,
        button_label_key: &'static str,
        in_progress_label_key: &'static str,
        in_progress: bool,
        in_progress_elapsed_sec: i64,
        readonly: bool,
        loading: bool,
        on_click: impl Fn(&mut ZedisPersistence, &mut Window, &mut Context<ZedisPersistence>) + 'static,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let disabled = readonly || loading || in_progress;

        let tooltip: Option<SharedString> = if readonly {
            Some(i18n_persistence(cx, "readonly_tooltip"))
        } else if in_progress {
            Some(i18n_persistence(cx, "in_progress_tooltip"))
        } else {
            None
        };

        let (button_label, status_line) = if in_progress {
            let dur = format_duration(Duration::from_secs(in_progress_elapsed_sec.max(0) as u64));
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            let status = rust_i18n::t!("persistence.elapsed_running", elapsed = dur, locale = locale).to_string();
            (i18n_persistence(cx, in_progress_label_key), Some(status))
        } else {
            (i18n_persistence(cx, button_label_key), None)
        };

        v_flex()
            .flex_1()
            .min_w(px(280.))
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .p_4()
            .gap_2()
            .child(Label::new(i18n_persistence(cx, title_key)).font_semibold())
            .child(
                Label::new(i18n_persistence(cx, description_key))
                    .text_sm()
                    .text_color(theme.muted_foreground),
            )
            .child(
                h_flex()
                    .pt_2()
                    .gap_2()
                    .items_center()
                    .child({
                        let mut btn = Button::new(id).primary().small().label(button_label);
                        if disabled {
                            btn = btn.disabled(true);
                        } else {
                            btn = btn.on_click(cx.listener(move |this, _, window, cx| {
                                on_click(this, window, cx);
                            }));
                        }
                        if let Some(t) = tooltip {
                            btn = btn.tooltip(t);
                        }
                        btn
                    })
                    .when_some(status_line, |this, s| {
                        this.child(Label::new(s).text_sm().text_color(theme.muted_foreground))
                    }),
            )
    }

    fn render_bgsave_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let readonly = self.readonly(cx);
        let loading = m.loading;
        let in_progress = m.rdb_bgsave_in_progress;
        let elapsed = m.rdb_current_bgsave_time_sec;

        self.render_action_card(
            cx,
            "persistence-bgsave",
            "bgsave_title",
            "bgsave_description",
            "bgsave_button",
            "bgsave_in_progress",
            in_progress,
            elapsed,
            readonly,
            loading,
            Self::open_bgsave_dialog,
        )
    }

    fn render_bgrewriteaof_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let readonly = self.readonly(cx);
        let loading = m.loading;
        let in_progress = m.aof_rewrite_in_progress;
        let elapsed = m.aof_current_rewrite_time_sec;

        self.render_action_card(
            cx,
            "persistence-bgrewriteaof",
            "bgrewriteaof_title",
            "bgrewriteaof_description",
            "bgrewriteaof_button",
            "bgrewriteaof_in_progress",
            in_progress,
            elapsed,
            readonly,
            loading,
            Self::open_bgrewriteaof_dialog,
        )
    }

    // ── Confirm dialogs ────────────────────────────────────────────────
    /// Both confirmations run their body through `escalate_dangerous_body`
    /// so that PROD-tagged (high-risk) servers get the escalated warning,
    /// matching the convention used by destructive ops like `XGROUP DESTROY`.
    fn open_bgsave_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_persistence(cx, "bgsave_confirm_title");
        let body = i18n_persistence(cx, "bgsave_confirm_body");
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body.to_string()))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| state.bgsave(cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn open_bgrewriteaof_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_persistence(cx, "bgrewriteaof_confirm_title");
        let body = i18n_persistence(cx, "bgrewriteaof_confirm_body");
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body.to_string()))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| state.bgrewriteaof(cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}

impl Render for ZedisPersistence {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(m) = self.metrics(cx) else {
            // Same skeleton placeholder Metrics uses while INFO is still
            // in flight — keeps the loading affordance consistent.
            return ZedisSkeletonLoading::new()
                .text(i18n_common(cx, "loading"))
                .into_any_element();
        };

        let mut body = v_flex().size_full().overflow_y_scrollbar();
        body = body.child(self.render_header(cx));
        if m.loading {
            body = body.child(self.render_loading_banner(cx));
        }
        if !m.rdb_last_bgsave_success {
            body = body.child(self.render_failure_banner(i18n_persistence(cx, "rdb_failure_banner"), cx));
        }
        if m.aof_enabled && !m.aof_last_bgrewrite_success {
            body = body.child(self.render_failure_banner(i18n_persistence(cx, "aof_failure_banner"), cx));
        }
        if m.aof_enabled && !m.aof_last_write_success {
            body = body.child(self.render_failure_banner(i18n_persistence(cx, "aof_write_failure_banner"), cx));
        }

        body = body.child(div().h(px(8.)));
        body = body.child(self.render_stat_grid(&m, cx));
        body = body.child(div().h(px(16.)));
        body = body.child(
            div()
                .px_4()
                .child(Label::new(i18n_persistence(cx, "actions_title")).font_semibold()),
        );
        body = body.child(div().h(px(8.)));
        body = body.child(
            h_flex()
                .w_full()
                .px_4()
                .gap_2()
                .flex_wrap()
                .child(self.render_bgsave_card(&m, cx))
                .when(m.aof_enabled, |this| this.child(self.render_bgrewriteaof_card(&m, cx))),
        );
        body = body.child(div().h(px(16.)));

        body.into_any_element()
    }
}
