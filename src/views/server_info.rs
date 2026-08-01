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

//! Raw `INFO` browser: every field the server reports, in one filterable
//! table. The structured panels (Metrics / Persistence / Topology / ...)
//! cover the common fields; this page exists for the long tail —
//! `errorstats`, `latencystats`, fork/COW costs, replication sync
//! counters, uptime — without dropping to the terminal.
//!
//! Fetches `INFO everything` (Redis 7+), degrading to `INFO all` then
//! plain `INFO` on older servers (unknown sections reply empty rather
//! than erroring). On clusters every master is queried and the section
//! column carries the node address, so filtering a field name compares
//! it across nodes.

use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
    i18n_common, i18n_server_info,
};
use chrono::Local;
use gpui::{ClipboardItem, Edges, Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use redis::cmd;
use tracing::{error, info};
use zedis_ui::{ZedisDivider, help_popover};

use crate::assets::CustomIconName;

const SECTION_COL_WIDTH: f32 = 220.;
const FIELD_COL_WIDTH: f32 = 300.;

/// One `field: value` line of an INFO reply, tagged with its section (and
/// node address on clusters).
#[derive(Clone, Debug)]
struct InfoRow {
    section: SharedString,
    field: SharedString,
    value: SharedString,
}

/// Split one node's INFO text into `(section, [(field, value)])` groups.
/// Lines outside any `# Section` header land in an unnamed section (never
/// produced by real servers, tolerated for robustness).
fn parse_info_sections(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: (String, Vec<(String, String)>) = (String::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('#') {
            if !current.1.is_empty() {
                sections.push(std::mem::take(&mut current));
            }
            current.0 = name.trim().to_string();
        } else if let Some((key, value)) = line.split_once(':') {
            current.1.push((key.to_string(), value.to_string()));
        }
    }
    if !current.1.is_empty() {
        sections.push(current);
    }
    sections
}

/// Flatten per-node INFO texts into display rows. Multi-node (cluster)
/// results prefix the section with the node address so identical fields
/// stay distinguishable — and comparable under one filter.
fn build_rows(replies: &[(String, String)]) -> Vec<InfoRow> {
    let multi_node = replies.len() > 1;
    let mut rows = Vec::new();
    for (node, text) in replies {
        for (section, fields) in parse_info_sections(text) {
            let section_label: SharedString = if multi_node {
                format!("{node} · {section}").into()
            } else {
                section.into()
            };
            for (field, value) in fields {
                rows.push(InfoRow {
                    section: section_label.clone(),
                    field: field.into(),
                    value: value.into(),
                });
            }
        }
    }
    rows
}

/// Case-insensitive substring filter over section, field, and value.
fn filter_rows(rows: &[InfoRow], filter: &str) -> Vec<InfoRow> {
    if filter.is_empty() {
        return rows.to_vec();
    }
    let needle = filter.to_lowercase();
    rows.iter()
        .filter(|r| {
            r.field.to_lowercase().contains(&needle)
                || r.section.to_lowercase().contains(&needle)
                || r.value.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

struct InfoTableDelegate {
    rows: Vec<InfoRow>,
    columns: Vec<Column>,
}

impl InfoTableDelegate {
    fn new(window: &mut Window, cx: &mut gpui::App) -> Self {
        let content_width = content_area_width(window, cx).as_f32();
        let value_w = content_width - SECTION_COL_WIDTH - FIELD_COL_WIDTH - 26.;
        let paddings = Edges {
            top: px(2.),
            bottom: px(2.),
            left: px(10.),
            right: px(10.),
        };
        let columns = [
            ("section", SECTION_COL_WIDTH),
            ("field", FIELD_COL_WIDTH),
            ("value", value_w),
        ]
        .into_iter()
        .map(|(key, w)| {
            let mut c = Column::new(key, SharedString::default()).width(w);
            c.paddings = Some(paddings);
            c
        })
        .collect();
        Self {
            rows: Vec::new(),
            columns,
        }
    }
}

impl TableDelegate for InfoTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _cx: &gpui::App) -> Column {
        self.columns[ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, p| this.paddings(p))
            .child(
                Label::new(i18n_server_info(cx, column.key.as_ref()))
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (value, muted) = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => (r.section.clone(), true),
                1 => (r.field.clone(), false),
                _ => (r.value.clone(), false),
            })
            .unwrap_or_else(|| ("--".into(), false));
        let column = &self.columns[col_ix];

        let group_name: SharedString = format!("info-td-{row_ix}-{col_ix}").into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, p| this.paddings(p))
            .group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(value.clone())
                    .when(muted, |this| this.text_color(cx.theme().muted_foreground))
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id((group_name.clone(), 0_usize))
                    .invisible()
                    .group_hover(group_name.clone(), |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                    .child(
                        Button::new((group_name.clone(), 1_usize))
                            .ghost()
                            .icon(IconName::Copy)
                            .on_click(move |_, window, cx: &mut gpui::App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                                window.push_notification(Notification::info(copied_message.clone()), cx);
                            }),
                    ),
            )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _: &mut Window, _: &mut gpui::Context<TableState<Self>>) {}
}

/// Raw INFO browser view — see the module docs.
pub struct ZedisServerInfo {
    server_state: Entity<ZedisServerState>,
    filter_input_state: Entity<InputState>,
    table_state: Entity<TableState<InfoTableDelegate>>,
    /// All parsed rows, unfiltered — the filter narrows a copy into the
    /// table delegate so clearing it is instant.
    rows: Vec<InfoRow>,
    /// Rows currently shown (after filter), for the count chip.
    visible_count: usize,
    loading: bool,
    error: Option<SharedString>,
    refreshed_at: Option<SharedString>,
    refresh_task: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl ZedisServerInfo {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let filter_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_server_info(cx, "filter_placeholder"))
        });
        subscriptions.push(
            cx.subscribe_in(&filter_input_state, window, |view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    view.apply_filter(cx);
                }
            }),
        );
        // Startup restore builds this view *before* the `ServerSelected`
        // announce populates `server_state` (same trap as the memory
        // analyzer's dbsize) — the construction-time refresh sees an empty
        // server id and bails, so re-fetch when the selection lands. Also
        // covers switching servers while staying on this route.
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.refresh(cx);
            }
        }));
        let table_state = cx.new(|cx| TableState::new(InfoTableDelegate::new(window, cx), window, cx));

        info!("Creating new server info view");
        let mut this = Self {
            server_state,
            filter_input_state,
            table_state,
            rows: Vec::new(),
            visible_count: 0,
            loading: false,
            error: None,
            refreshed_at: None,
            refresh_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    /// Re-filter the master row list into the table delegate.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let filter: SharedString = self.filter_input_state.read(cx).value();
        let visible = filter_rows(&self.rows, filter.as_ref());
        self.visible_count = visible.len();
        self.table_state.update(cx, |s, _| s.delegate_mut().rows = visible);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        // No selection yet (startup restore) — stay quiet instead of
        // erroring on an empty id; the ServerSelected subscription will
        // call back in once the connection is announced.
        if server_id.is_empty() {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();

        self.refresh_task = Some(cx.spawn(async move |handle, cx| {
            let result: Result<Vec<(String, String)>, Error> = cx
                .background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_id, db).await?;
                    // `INFO everything` (7+) → `INFO all` → plain `INFO`.
                    // Unknown sections reply with an empty string instead of
                    // an error, so "empty" is the degrade signal.
                    for section in ["everything", "all", ""] {
                        let mut c = cmd("INFO");
                        if !section.is_empty() {
                            c.arg(section);
                        }
                        let (servers, list): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                        if list.iter().any(|text| !text.trim().is_empty()) {
                            return Ok(servers
                                .iter()
                                .zip(list)
                                .map(|(srv, text)| (format!("{}:{}", srv.host, srv.port), text))
                                .collect());
                        }
                    }
                    Ok(Vec::new())
                })
                .await;

            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(replies) => {
                        this.rows = build_rows(&replies);
                        this.refreshed_at = Some(Local::now().format("%H:%M:%S").to_string().into());
                    }
                    Err(e) => {
                        error!(error = %e, "INFO fetch failed");
                        this.error = Some(e.to_string().into());
                    }
                }
                this.loading = false;
                this.apply_filter(cx);
            });
        }));
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("server-info-back")
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
            .child(Icon::new(IconName::Info))
            .child(Label::new(i18n_server_info(cx, "title")).text_color(cx.theme().foreground))
            .child(help_popover("server-info-help", i18n_server_info(cx, "help")));

        let stat_item = |cx: &mut Context<Self>, key: &'static str, value: SharedString| {
            h_flex()
                .gap_1()
                .child(
                    Label::new(i18n_server_info(cx, key))
                        .text_color(cx.theme().muted_foreground)
                        .text_sm(),
                )
                .child(Label::new(value).text_sm().font_weight(gpui::FontWeight::MEDIUM))
        };
        let functions = ZedisDivider::new()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .when(!self.rows.is_empty(), |this| {
                        this.child(stat_item(
                            cx,
                            "fields",
                            format!("{}/{}", self.visible_count, self.rows.len()).into(),
                        ))
                    })
                    .when_some(self.refreshed_at.clone(), |this, at| {
                        this.child(stat_item(cx, "refreshed_at", at))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Input::new(&self.filter_input_state).small().w(px(260.)))
                    .child(
                        Button::new("server-info-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .loading(self.loading)
                            .disabled(self.loading)
                            .label(i18n_server_info(cx, "refresh"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.refresh(cx);
                            })),
                    ),
            );

        h_flex()
            .id("server-info-toolbar")
            .w_full()
            .flex_none()
            .h(px(40.))
            .px_4()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .overflow_x_scroll()
            .child(nav.flex_none())
            .child(div().flex_1())
            .child(functions.flex_none())
    }
}

impl Render for ZedisServerInfo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.visible_count == 0;

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .child(self.render_toolbar(cx))
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(IconName::CircleX).text_color(cx.theme().danger))
                        .child(Label::new(message).text_sm().text_color(cx.theme().danger)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(is_empty, |this| {
                        this.child(
                            div().size_full().flex().items_center().justify_center().child(
                                Label::new(i18n_server_info(cx, "no_data")).text_color(cx.theme().muted_foreground),
                            ),
                        )
                    })
                    .when(!is_empty, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_fields() {
        let text =
            "# Server\r\nredis_version:7.2.5\r\nuptime_in_seconds:12345\r\n\r\n# Clients\r\nconnected_clients:3\r\n";
        let sections = parse_info_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "Server");
        assert_eq!(
            sections[0].1,
            vec![
                ("redis_version".to_string(), "7.2.5".to_string()),
                ("uptime_in_seconds".to_string(), "12345".to_string()),
            ]
        );
        assert_eq!(sections[1].0, "Clients");
        assert_eq!(sections[1].1, vec![("connected_clients".to_string(), "3".to_string())]);
    }

    #[test]
    fn multi_node_rows_carry_node_prefix_and_filter_matches() {
        let replies = vec![
            ("10.0.0.1:6379".to_string(), "# Stats\nsync_full:2\n".to_string()),
            ("10.0.0.2:6379".to_string(), "# Stats\nsync_full:0\n".to_string()),
        ];
        let rows = build_rows(&replies);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].section.as_ref(), "10.0.0.1:6379 · Stats");
        assert_eq!(rows[1].section.as_ref(), "10.0.0.2:6379 · Stats");

        // One filter compares the field across nodes; value matching works too.
        assert_eq!(filter_rows(&rows, "SYNC_FULL").len(), 2);
        assert_eq!(filter_rows(&rows, "10.0.0.2").len(), 1);
        assert_eq!(filter_rows(&rows, "nosuch").len(), 0);
        assert_eq!(filter_rows(&rows, "").len(), 2);
    }

    #[test]
    fn single_node_keeps_plain_section_names() {
        let replies = vec![("127.0.0.1:6379".to_string(), "# Memory\nused_memory:100\n".to_string())];
        let rows = build_rows(&replies);
        assert_eq!(rows[0].section.as_ref(), "Memory");
        assert_eq!(rows[0].field.as_ref(), "used_memory");
        assert_eq!(rows[0].value.as_ref(), "100");
    }
}
