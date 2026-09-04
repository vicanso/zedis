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
//! Surfaces the RDB/AOF state pulled from `INFO persistence`, a read-only
//! summary of the relevant `CONFIG` knobs, and (on cluster) a per-master
//! table. Actions (`BGSAVE` / `BGREWRITEAOF`) are gated by:
//!   * `Capability::PersistenceWrite` (safe / strict RO);
//!   * Redis still loading from disk;
//!   * a fork already running (shows elapsed seconds).
//!
//! Confirm dialogs go through `escalate_dangerous_body` so PROD-tagged
//! servers get the escalated warning.

use crate::assets::CustomIconName;
use crate::connection::{Capability, get_connection_manager, get_server};
use crate::helpers::{format_duration, format_unix_secs, get_mono_font_family, unix_ts};
use crate::states::{
    PersistenceNodeSnapshot, RedisMetrics, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState,
    back_to_editor_tooltip, dialog_button_props, escalate_dangerous_body, i18n_common, i18n_persistence,
};
use crate::views::unavailable_chip;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use redis::cmd;
use std::time::Duration;
use zedis_ui::{ZedisDialog, ZedisSkeletonLoading};

/// Soft staleness threshold for the "snapshot is old" info banner.
const STALE_SAVE_SECS: i64 = 6 * 3600;

/// Labels + state for a BGSAVE / BGREWRITEAOF action card.
struct PersistenceActionCard {
    id: &'static str,
    title_key: &'static str,
    description_key: &'static str,
    button_label_key: &'static str,
    in_progress_label_key: &'static str,
    in_progress: bool,
    in_progress_elapsed_sec: i64,
    can_write: bool,
    loading: bool,
}

/// Read-only CONFIG subset shown under the status cards.
#[derive(Debug, Clone, Default)]
struct PersistenceConfig {
    save: String,
    appendonly: String,
    appendfsync: String,
    auto_aof_rewrite_percentage: String,
    auto_aof_rewrite_min_size: String,
    dir: String,
    dbfilename: String,
    appendfilename: String,
    /// True when CONFIG GET failed (e.g. managed cloud NOPERM).
    unavailable: bool,
}

pub struct ZedisPersistence {
    title: SharedString,
    server_state: Entity<ZedisServerState>,
    config: Option<PersistenceConfig>,
    /// Previous in-progress flags — used to fire completion toasts when
    /// a fork transitions 1 → 0 between INFO polls.
    prev_rdb_bgsave: bool,
    prev_aof_rewrite: bool,
    pending_notification: Option<Notification>,
    _config_task: Option<Task<()>>,
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

        let metrics = state.redis_info().map(|i| i.metrics);
        let prev_rdb = metrics.is_some_and(|m| m.rdb_bgsave_in_progress);
        let prev_aof = metrics.is_some_and(|m| m.aof_rewrite_in_progress);

        let mut this = Self {
            title,
            server_state: server_state.clone(),
            config: None,
            prev_rdb_bgsave: prev_rdb,
            prev_aof_rewrite: prev_aof,
            pending_notification: None,
            _config_task: None,
            _subscriptions: Vec::new(),
        };

        this._subscriptions
            .push(cx.subscribe(&server_state, |this, state, event, cx| match event {
                ServerEvent::ServerRedisInfoUpdated => {
                    this.detect_completion(&state, cx);
                    cx.notify();
                }
                ServerEvent::ServerSelected(_) => {
                    this.title = {
                        let st = state.read(cx);
                        let name = get_server(st.server_id())
                            .map(|s| s.name)
                            .unwrap_or_else(|_| "--".to_string());
                        let nodes = st.nodes_description();
                        format!("{name} - {}({})", nodes.server_type, nodes.master_nodes).into()
                    };
                    this.config = None;
                    this.prev_rdb_bgsave = false;
                    this.prev_aof_rewrite = false;
                    this.fetch_config(cx);
                    cx.notify();
                }
                _ => {}
            }));

        this.fetch_config(cx);
        this
    }

    fn metrics(&self, cx: &Context<Self>) -> Option<RedisMetrics> {
        self.server_state.read(cx).redis_info().map(|i| i.metrics)
    }

    fn persistence_nodes(&self, cx: &Context<Self>) -> Vec<PersistenceNodeSnapshot> {
        self.server_state
            .read(cx)
            .redis_info()
            .map(|i| i.persistence_nodes.clone())
            .unwrap_or_default()
    }

    fn can_write(&self, cx: &Context<Self>) -> bool {
        self.server_state.read(cx).can(Capability::PersistenceWrite)
    }

    fn detect_completion(&mut self, state: &Entity<ZedisServerState>, cx: &mut Context<Self>) {
        let Some(m) = state.read(cx).redis_info().map(|i| i.metrics) else {
            return;
        };
        if self.prev_rdb_bgsave && !m.rdb_bgsave_in_progress {
            let msg = if m.rdb_last_bgsave_success {
                i18n_persistence(cx, "bgsave_finished_ok")
            } else {
                i18n_persistence(cx, "bgsave_finished_fail")
            };
            self.pending_notification = Some(if m.rdb_last_bgsave_success {
                Notification::success(msg)
            } else {
                Notification::error(msg)
            });
        }
        if self.prev_aof_rewrite && !m.aof_rewrite_in_progress {
            let msg = if m.aof_last_bgrewrite_success {
                i18n_persistence(cx, "bgrewriteaof_finished_ok")
            } else {
                i18n_persistence(cx, "bgrewriteaof_finished_fail")
            };
            self.pending_notification = Some(if m.aof_last_bgrewrite_success {
                Notification::success(msg)
            } else {
                Notification::error(msg)
            });
        }
        self.prev_rdb_bgsave = m.rdb_bgsave_in_progress;
        self.prev_aof_rewrite = m.aof_rewrite_in_progress;
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.server_state.update(cx, |state, cx| {
            state.refresh_redis_info(cx);
        });
        self.fetch_config(cx);
    }

    fn fetch_config(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._config_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let keys = [
                    "save",
                    "appendonly",
                    "appendfsync",
                    "auto-aof-rewrite-percentage",
                    "auto-aof-rewrite-min-size",
                    "dir",
                    "dbfilename",
                    "appendfilename",
                ];
                let mut cfg = PersistenceConfig::default();
                let mut any_ok = false;
                for key in keys {
                    let res: redis::RedisResult<Vec<String>> =
                        cmd("CONFIG").arg("GET").arg(key).query_async(&mut conn).await;
                    match res {
                        Ok(pairs) => {
                            any_ok = true;
                            // CONFIG GET returns [key, value] pairs.
                            if pairs.len() >= 2 {
                                let val = pairs[1].clone();
                                match key {
                                    "save" => cfg.save = val,
                                    "appendonly" => cfg.appendonly = val,
                                    "appendfsync" => cfg.appendfsync = val,
                                    "auto-aof-rewrite-percentage" => cfg.auto_aof_rewrite_percentage = val,
                                    "auto-aof-rewrite-min-size" => cfg.auto_aof_rewrite_min_size = val,
                                    "dir" => cfg.dir = val,
                                    "dbfilename" => cfg.dbfilename = val,
                                    "appendfilename" => cfg.appendfilename = val,
                                    _ => {}
                                }
                            }
                        }
                        Err(_) => {
                            // NOPERM / blocked — mark unavailable if nothing succeeded.
                        }
                    }
                }
                if !any_ok {
                    cfg.unavailable = true;
                }
                Ok::<_, crate::error::Error>(cfg)
            });
            if let Ok(cfg) = task.await {
                let _ = handle.update(cx, |this, cx| {
                    this.config = Some(cfg);
                    cx.notify();
                });
            }
        }));
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
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Label::new(i18n_persistence(cx, "title")).font_semibold())
                    .child(Label::new(self.title.clone()).text_color(theme.muted_foreground)),
            )
            .child(
                Button::new("persistence-refresh")
                    .outline()
                    .small()
                    .icon(Icon::new(CustomIconName::RotateCw))
                    .tooltip(i18n_persistence(cx, "refresh_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
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

    fn render_stale_banner(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if m.rdb_last_save_time <= 0 {
            return None;
        }
        let age = unix_ts().saturating_sub(m.rdb_last_save_time);
        if age < STALE_SAVE_SECS {
            return None;
        }
        let theme = cx.theme();
        let dur = format_duration(Duration::from_secs(age.max(0) as u64));
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let msg: SharedString = rust_i18n::t!("persistence.stale_banner", age = dur, locale = locale)
            .to_string()
            .into();
        Some(
            div()
                .mx_4()
                .my_2()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.warning)
                .bg(theme.warning.opacity(0.08))
                .child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(IconName::Info).text_color(theme.warning))
                        .child(Label::new(msg).text_color(theme.warning).text_sm()),
                ),
        )
    }

    // ── Status cards ───────────────────────────────────────────────────
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

        // Hint: absolute time · last duration · ok/failed
        let mut parts: Vec<String> = Vec::new();
        if m.rdb_last_save_time > 0 {
            parts.push(format_unix_local(m.rdb_last_save_time));
        }
        if m.rdb_last_bgsave_time_sec >= 0 {
            let dur = format_duration(Duration::from_secs(m.rdb_last_bgsave_time_sec as u64));
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            parts.push(rust_i18n::t!("persistence.last_duration", duration = dur, locale = locale).to_string());
        }
        parts.push(
            if m.rdb_last_bgsave_success {
                i18n_persistence(cx, "state_ok")
            } else {
                i18n_persistence(cx, "state_failed")
            }
            .to_string(),
        );
        let hint: SharedString = parts.join(" · ").into();

        self.render_stat_card(cx, i18n_persistence(cx, "card_last_save"), value, Some(hint), accent)
    }

    fn render_changes_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let accent = if m.rdb_changes_since_last_save > 0 {
            Some(theme.warning)
        } else {
            None
        };
        let hint = if m.rdb_changes_since_last_save == 0 {
            i18n_persistence(cx, "changes_clean")
        } else {
            i18n_persistence(cx, "changes_pending")
        };
        self.render_stat_card(
            cx,
            i18n_persistence(cx, "card_changes"),
            m.rdb_changes_since_last_save.to_string().into(),
            Some(hint),
            accent,
        )
    }

    fn render_aof_status_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (value, accent, hint) = if m.aof_enabled {
            (
                i18n_persistence(cx, "aof_enabled"),
                None,
                Some(i18n_persistence(cx, "aof_enabled_hint")),
            )
        } else {
            (
                i18n_persistence(cx, "aof_disabled"),
                Some(theme.muted_foreground),
                Some(i18n_persistence(cx, "aof_disabled_hint")),
            )
        };
        self.render_stat_card(cx, i18n_persistence(cx, "card_aof_status"), value, hint, accent)
    }

    fn render_aof_size_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        let current = humansize::format_size(
            m.aof_current_size,
            humansize::FormatSizeOptions::default().decimal_places(1),
        );
        let mut hint_parts: Vec<String> = Vec::new();
        if m.aof_base_size == 0 {
            hint_parts.push(i18n_persistence(cx, "aof_no_baseline").to_string());
        } else {
            let ratio = m.aof_current_size as f64 / m.aof_base_size as f64;
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            hint_parts.push(
                rust_i18n::t!(
                    "persistence.aof_growth",
                    ratio = format!("{:.2}", ratio),
                    locale = locale
                )
                .to_string(),
            );
        }
        if m.aof_last_rewrite_time_sec >= 0 {
            let dur = format_duration(Duration::from_secs(m.aof_last_rewrite_time_sec as u64));
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            hint_parts.push(rust_i18n::t!("persistence.last_duration", duration = dur, locale = locale).to_string());
        }
        let hint: SharedString = hint_parts.join(" · ").into();
        self.render_stat_card(
            cx,
            i18n_persistence(cx, "card_aof_size"),
            current.into(),
            Some(hint),
            None,
        )
    }

    fn render_stat_grid(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_4()
            .gap_2()
            .flex_wrap()
            .items_stretch()
            .child(self.render_last_save_card(m, cx))
            .child(self.render_changes_card(m, cx))
            .child(self.render_aof_status_card(m, cx))
            .when(m.aof_enabled, |this| this.child(self.render_aof_size_card(m, cx)))
    }

    // ── Policy / path summary ──────────────────────────────────────────
    fn render_policy_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(cfg) = self.config.as_ref() else {
            return div()
                .px_4()
                .child(
                    Label::new(i18n_persistence(cx, "policy_loading"))
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
                .into_any_element();
        };
        if cfg.unavailable {
            return div()
                .px_4()
                .child(
                    Label::new(i18n_persistence(cx, "policy_unavailable"))
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
                .into_any_element();
        }

        let save_display = if cfg.save.trim().is_empty() {
            i18n_persistence(cx, "save_disabled").to_string()
        } else {
            // Redis stores as "900 1 300 10 60 10000" — group into pairs for readability.
            format_save_policy(&cfg.save)
        };

        let path_display = {
            let dir = if cfg.dir.is_empty() { "—" } else { cfg.dir.as_str() };
            let rdb = if cfg.dbfilename.is_empty() {
                "dump.rdb"
            } else {
                cfg.dbfilename.as_str()
            };
            if cfg.appendonly == "yes" {
                let aof = if cfg.appendfilename.is_empty() {
                    "appendonly.aof"
                } else {
                    cfg.appendfilename.as_str()
                };
                format!("{dir}/{rdb} · {aof}")
            } else {
                format!("{dir}/{rdb}")
            }
        };

        let aof_line = if cfg.appendonly == "yes" {
            format!(
                "appendonly=yes · fsync={} · auto-rewrite {}% / {}",
                if cfg.appendfsync.is_empty() {
                    "—"
                } else {
                    cfg.appendfsync.as_str()
                },
                if cfg.auto_aof_rewrite_percentage.is_empty() {
                    "—"
                } else {
                    cfg.auto_aof_rewrite_percentage.as_str()
                },
                if cfg.auto_aof_rewrite_min_size.is_empty() {
                    "—"
                } else {
                    cfg.auto_aof_rewrite_min_size.as_str()
                },
            )
        } else {
            i18n_persistence(cx, "aof_disabled_hint").to_string()
        };

        v_flex()
            .px_4()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(i18n_persistence(cx, "policy_title")).font_semibold())
                    .child(div().flex_1())
                    .child(
                        Button::new("persistence-open-config")
                            .ghost()
                            .small()
                            .label(i18n_persistence(cx, "open_config"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Config, cx));
                                });
                            }),
                    ),
            )
            .child(
                v_flex()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .p_3()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(i18n_persistence(cx, "policy_save"))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .w(px(80.)),
                            )
                            .child(
                                Label::new(save_display)
                                    .text_xs()
                                    .font_family(get_mono_font_family())
                                    .text_color(theme.foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(i18n_persistence(cx, "policy_aof"))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .w(px(80.)),
                            )
                            .child(
                                Label::new(aof_line)
                                    .text_xs()
                                    .font_family(get_mono_font_family())
                                    .text_color(theme.foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(i18n_persistence(cx, "policy_path"))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .w(px(80.)),
                            )
                            .child(
                                Label::new(path_display)
                                    .text_xs()
                                    .font_family(get_mono_font_family())
                                    .text_color(theme.foreground),
                            ),
                    ),
            )
            .into_any_element()
    }

    // ── Cluster per-node table ─────────────────────────────────────────
    fn render_nodes_table(&self, nodes: &[PersistenceNodeSnapshot], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(nodes.len() + 1);
        rows.push(
            h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .child(
                    Label::new(i18n_persistence(cx, "node_col_addr"))
                        .text_xs()
                        .text_color(muted)
                        .w(px(180.)),
                )
                .child(
                    Label::new(i18n_persistence(cx, "node_col_last_save"))
                        .text_xs()
                        .text_color(muted)
                        .w(px(140.)),
                )
                .child(
                    Label::new(i18n_persistence(cx, "node_col_changes"))
                        .text_xs()
                        .text_color(muted)
                        .w(px(90.)),
                )
                .child(
                    Label::new(i18n_persistence(cx, "node_col_status"))
                        .text_xs()
                        .text_color(muted)
                        .w(px(120.)),
                )
                .child(
                    Label::new(i18n_persistence(cx, "node_col_aof"))
                        .text_xs()
                        .text_color(muted)
                        .w(px(100.)),
                )
                .into_any_element(),
        );
        for n in nodes {
            let last = if n.rdb_last_save_time <= 0 {
                i18n_persistence(cx, "state_never").to_string()
            } else {
                let elapsed = unix_ts().saturating_sub(n.rdb_last_save_time).max(0) as u64;
                format_duration(Duration::from_secs(elapsed)) + " ago"
            };
            let status = if n.rdb_bgsave_in_progress {
                i18n_persistence(cx, "bgsave_in_progress").to_string()
            } else if n.rdb_last_bgsave_success {
                i18n_persistence(cx, "state_ok").to_string()
            } else {
                i18n_persistence(cx, "state_failed").to_string()
            };
            let status_color = if n.rdb_bgsave_in_progress {
                theme.warning
            } else if n.rdb_last_bgsave_success {
                theme.green
            } else {
                theme.danger
            };
            let aof = if !n.aof_enabled {
                i18n_persistence(cx, "aof_disabled").to_string()
            } else if n.aof_rewrite_in_progress {
                i18n_persistence(cx, "bgrewriteaof_in_progress").to_string()
            } else {
                humansize::format_size(
                    n.aof_current_size,
                    humansize::FormatSizeOptions::default().decimal_places(1),
                )
            };
            rows.push(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Label::new(n.label.clone())
                            .text_xs()
                            .font_family(get_mono_font_family())
                            .w(px(180.)),
                    )
                    .child(Label::new(last).text_xs().w(px(140.)))
                    .child(
                        Label::new(n.rdb_changes_since_last_save.to_string())
                            .text_xs()
                            .w(px(90.)),
                    )
                    .child(Label::new(status).text_xs().text_color(status_color).w(px(120.)))
                    .child(Label::new(aof).text_xs().w(px(100.)))
                    .into_any_element(),
            );
        }
        v_flex()
            .px_4()
            .gap_2()
            .child(Label::new(i18n_persistence(cx, "nodes_title")).font_semibold())
            .child(
                v_flex()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .overflow_hidden()
                    .children(rows),
            )
    }

    // ── Action cards ───────────────────────────────────────────────────
    fn render_action_card(
        &self,
        cx: &mut Context<Self>,
        card: PersistenceActionCard,
        on_click: impl Fn(&mut ZedisPersistence, &mut Window, &mut Context<ZedisPersistence>) + 'static,
    ) -> impl IntoElement {
        let PersistenceActionCard {
            id,
            title_key,
            description_key,
            button_label_key,
            in_progress_label_key,
            in_progress,
            in_progress_elapsed_sec,
            can_write,
            loading,
        } = card;
        let theme = cx.theme();
        let disabled = !can_write || loading || in_progress;

        let tooltip: Option<SharedString> = if !can_write {
            Some(i18n_persistence(cx, "readonly_tooltip"))
        } else if loading {
            Some(i18n_persistence(cx, "loading_tooltip"))
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
        self.render_action_card(
            cx,
            PersistenceActionCard {
                id: "persistence-bgsave",
                title_key: "bgsave_title",
                description_key: "bgsave_description",
                button_label_key: "bgsave_button",
                in_progress_label_key: "bgsave_in_progress",
                in_progress: m.rdb_bgsave_in_progress,
                in_progress_elapsed_sec: m.rdb_current_bgsave_time_sec,
                can_write: self.can_write(cx),
                loading: m.loading,
            },
            Self::open_bgsave_dialog,
        )
    }

    fn render_bgrewriteaof_card(&self, m: &RedisMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_action_card(
            cx,
            PersistenceActionCard {
                id: "persistence-bgrewriteaof",
                title_key: "bgrewriteaof_title",
                description_key: "bgrewriteaof_description",
                button_label_key: "bgrewriteaof_button",
                in_progress_label_key: "bgrewriteaof_in_progress",
                in_progress: m.aof_rewrite_in_progress,
                in_progress_elapsed_sec: m.aof_current_rewrite_time_sec,
                can_write: self.can_write(cx),
                loading: m.loading,
            },
            Self::open_bgrewriteaof_dialog,
        )
    }

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

fn format_unix_local(ts: i64) -> String {
    format_unix_secs(ts).unwrap_or_else(|| ts.to_string())
}

/// Turn Redis `save` config (`900 1 300 10 60 10000`) into `900s/1 · 300s/10 · 60s/10000`.
fn format_save_policy(raw: &str) -> String {
    let nums: Vec<&str> = raw.split_whitespace().collect();
    if nums.is_empty() {
        return raw.to_string();
    }
    let mut parts = Vec::new();
    for chunk in nums.chunks(2) {
        if chunk.len() == 2 {
            parts.push(format!("{}s/{}", chunk[0], chunk[1]));
        } else {
            parts.push(chunk[0].to_string());
        }
    }
    parts.join(" · ")
}

impl Render for ZedisPersistence {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let Some(m) = self.metrics(cx) else {
            return ZedisSkeletonLoading::new()
                .text(i18n_common(cx, "loading"))
                .into_any_element();
        };

        let nodes = self.persistence_nodes(cx);

        let mut body = v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .overflow_y_scrollbar();
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
        if let Some(banner) = self.render_stale_banner(&m, cx) {
            body = body.child(banner);
        }

        body = body.child(div().h(px(8.)));
        body = body.child(self.render_stat_grid(&m, cx));
        body = body.child(div().h(px(12.)));
        body = body.child(self.render_policy_section(cx));

        if !nodes.is_empty() {
            body = body.child(div().h(px(12.)));
            body = body.child(self.render_nodes_table(&nodes, cx));
        }

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
                // BGSAVE denied / missing (managed clouds): the cards stay,
                // greyed, and this says why.
                .when_some(
                    self.server_state.read(cx).blocked_by(Capability::PersistenceWrite),
                    |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                )
                .child(self.render_bgsave_card(&m, cx))
                .when(m.aof_enabled, |this| this.child(self.render_bgrewriteaof_card(&m, cx))),
        );
        body = body.child(div().h(px(16.)));

        body.into_any_element()
    }
}
