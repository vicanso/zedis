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

//! An administrator's audit (ADR 18): what went through the bridge's door,
//! newest first, narrowed by account, event, server and how far back, a page
//! at a time — and the line picked in the table shown whole under it, since
//! a line says more than a row has columns for.

use super::*;
use crate::connection::{BridgeAuditPage, BridgeAuditQuery, get_servers};
use gpui::Subscription;
use gpui_kit::component::{
    input::{Input, InputState},
    table::{DataTable, TableEvent, TableState},
};
use jiff::Timestamp;
use zedis_ui::{TextColumn, ZedisSelect, ZedisSelectEvent, ZedisTextTable, fitted_table};

const TABLE_HEIGHT: f32 = 300.;
const TIME_COLUMN_WIDTH: f32 = 170.;
const ACCOUNT_COLUMN_WIDTH: f32 = 140.;
const EVENT_COLUMN_WIDTH: f32 = 150.;
const SERVER_COLUMN_WIDTH: f32 = 160.;

pub(super) const AUDIT_ACCOUNT_ID: &str = "account-audit-account";
pub(super) const AUDIT_EVENT_ID: &str = "account-audit-event";
pub(super) const AUDIT_SEARCH_ID: &str = "account-audit-search";
pub(super) const AUDIT_MORE_ID: &str = "account-audit-more";

/// How far back a search reaches, after "any time": an hour, a day, a week,
/// thirty days — each with its label.
const PERIODS: [(i64, &str); 4] = [
    (3_600, "audit_last_hour"),
    (86_400, "audit_last_day"),
    (7 * 86_400, "audit_last_week"),
    (30 * 86_400, "audit_last_month"),
];

pub(super) struct AuditForm {
    /// The lines shown, newest first, as the bridge sent them.
    pub(super) lines: Vec<String>,
    pub(super) table: Entity<TableState<ZedisTextTable>>,
    account: Entity<InputState>,
    event: Entity<InputState>,
    /// The server entries the page sees, by id, in the picker's order after
    /// its "any server".
    pub(super) servers: Vec<String>,
    server: Entity<ZedisSelect>,
    /// Which is picked: 0 is any.
    pub(super) server_index: usize,
    period: Entity<ZedisSelect>,
    /// 0 is any time; else a [`PERIODS`] entry, one on.
    pub(super) period_index: usize,
    /// The question the lines answer — what *Older lines* asks again.
    asked: BridgeAuditQuery,
    next: Option<String>,
    stopped_early: bool,
    /// The line picked in the table, by its place in `lines`.
    selected: Option<usize>,
    busy: bool,
    /// Whether the first page was asked for.
    pub(super) loaded: bool,
    pub(super) status: Option<Status>,
    _subscriptions: Vec<Subscription>,
}

impl AuditForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ZedisAccount>) -> Self {
        let columns = vec![
            TextColumn::new("time", i18n_account(cx, "col_time"), TIME_COLUMN_WIDTH),
            TextColumn::new("account", i18n_account(cx, "col_account"), ACCOUNT_COLUMN_WIDTH),
            TextColumn::new("event", i18n_account(cx, "col_event"), EVENT_COLUMN_WIDTH),
            TextColumn::new("server", i18n_account(cx, "col_server"), SERVER_COLUMN_WIDTH).fills(),
        ];
        let table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
            .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"));
        let table = cx.new(|cx| TableState::new(table, window, cx));
        let table_subscription = cx.subscribe(&table, |this, table, event, cx| {
            match event {
                TableEvent::SelectRow(row) => {
                    // The row's last cell, never drawn, is its line's place.
                    this.audit.selected = table
                        .read(cx)
                        .delegate()
                        .visible_iter()
                        .nth(*row)
                        .and_then(|cells| cells.last())
                        .and_then(|at| at.parse().ok());
                }
                TableEvent::ClearSelection => this.audit.selected = None,
                _ => return,
            }
            cx.notify();
        });
        // The page's server list is what the account sees, which is what the
        // bridge would narrow to anyway.
        let (servers, names): (Vec<String>, Vec<String>) = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|server| (server.id, server.name))
            .unzip();
        let server_labels = std::iter::once(i18n_account(cx, "audit_any_server").to_string())
            .chain(names)
            .collect();
        let server = cx.new(|cx| ZedisSelect::new(server_labels, Some(0), window, cx));
        let period_labels = std::iter::once(i18n_account(cx, "audit_any_time").to_string())
            .chain(PERIODS.iter().map(|(_, key)| i18n_account(cx, key).to_string()))
            .collect();
        let period = cx.new(|cx| ZedisSelect::new(period_labels, Some(0), window, cx));
        let subscriptions = vec![
            table_subscription,
            cx.subscribe(&server, |this, _, ZedisSelectEvent::Change(index), _| {
                this.audit.server_index = *index;
            }),
            cx.subscribe(&period, |this, _, ZedisSelectEvent::Change(index), _| {
                this.audit.period_index = *index;
            }),
        ];
        Self {
            lines: Vec::new(),
            table,
            account: cx.new(|cx| InputState::new(window, cx).placeholder(i18n_account(cx, "audit_any_account"))),
            event: cx.new(|cx| InputState::new(window, cx).placeholder(i18n_account(cx, "audit_any_event"))),
            servers,
            server,
            server_index: 0,
            period,
            period_index: 0,
            asked: BridgeAuditQuery::default(),
            next: None,
            stopped_early: false,
            selected: None,
            busy: false,
            loaded: false,
            status: None,
            _subscriptions: subscriptions,
        }
    }

    /// The server picked, by id; `None` for any.
    fn server_id(&self) -> Option<String> {
        self.server_index
            .checked_sub(1)
            .and_then(|at| self.servers.get(at))
            .cloned()
    }

    /// Where the period picked starts, in Unix milliseconds; `None` for any
    /// time.
    fn since_ms(&self) -> Option<i64> {
        let (secs, _) = PERIODS.get(self.period_index.checked_sub(1)?)?;
        Some(Timestamp::now().as_millisecond() - secs * 1000)
    }

    /// The table's rows: time, account, event, server, and the line's place.
    fn rows(&self) -> Vec<Vec<SharedString>> {
        self.lines
            .iter()
            .enumerate()
            .map(|(at, line)| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
                let text = |field: &str| SharedString::from(value[field].as_str().unwrap_or_default().to_string());
                let time = value["ts"]
                    .as_str()
                    .and_then(|ts| ts.parse::<Timestamp>().ok())
                    .and_then(|ts| u64::try_from(ts.as_second()).ok())
                    .map(when)
                    .unwrap_or_default();
                let server = value["server"]["name"].as_str().unwrap_or_default().to_string();
                vec![
                    time.into(),
                    text("account"),
                    text("event"),
                    server.into(),
                    at.to_string().into(),
                ]
            })
            .collect()
    }
}

/// What a field holds, trimmed; nothing for an empty one.
fn narrowing(text: &str) -> Option<String> {
    Some(text.trim().to_string()).filter(|text| !text.is_empty())
}

impl ZedisAccount {
    /// Ask for the newest page of what the fields narrow to — or, with
    /// `older`, the page after the last one, appended.
    pub(super) fn load_audit(&mut self, older: bool, cx: &mut Context<Self>) {
        let query = if older {
            BridgeAuditQuery {
                before: self.audit.next.clone(),
                ..self.audit.asked.clone()
            }
        } else {
            BridgeAuditQuery {
                account: narrowing(&self.audit.account.read(cx).value()),
                event: narrowing(&self.audit.event.read(cx).value()),
                server: self.audit.server_id(),
                since_ms: self.audit.since_ms(),
                ..BridgeAuditQuery::default()
            }
        };
        self.audit.loaded = true;
        self.audit.busy = true;
        self.audit.status = None;
        cx.notify();
        let request = self.bridge.audit(query.clone());
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.audit.busy = false;
                match result {
                    Ok(page) => this.take_audit_page(query, older, page, cx),
                    Err(e) => {
                        let known = [(BridgeAccountErrorKind::NotFound, "audit_none")];
                        this.audit.status = Status::bad(refusal(cx, &e, &known));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn take_audit_page(&mut self, asked: BridgeAuditQuery, older: bool, page: BridgeAuditPage, cx: &mut Context<Self>) {
        if !older {
            self.audit.lines.clear();
            self.audit.selected = None;
        }
        self.audit.lines.extend(page.lines);
        self.audit.asked = BridgeAuditQuery { before: None, ..asked };
        self.audit.next = page.next;
        self.audit.stopped_early = page.stopped_early;
        let rows = self.audit.rows();
        self.audit.table.update(cx, |table, cx| {
            // A new question's rows are other lines: a highlight left on the
            // row that was picked would point at one nobody picked.
            if !older {
                table.clear_selection(cx);
            }
            table.delegate_mut().set_rows(rows);
        });
    }

    pub(super) fn render_audit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let form = &self.audit;
        let table = fitted_table(
            &form.table,
            DataTable::new(&form.table)
                .stripe(true)
                .bordered(true)
                .scrollbar_visible(true, false),
        );
        let empty = form.loaded && !form.busy && form.lines.is_empty() && form.status.is_none();
        let detail = form
            .selected
            .and_then(|at| form.lines.get(at))
            .map(|line| {
                let pretty = serde_json::from_str::<serde_json::Value>(line)
                    .and_then(|value| serde_json::to_string_pretty(&value))
                    .unwrap_or_else(|_| line.clone());
                div()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .font_family(get_mono_font_family())
                    .text_xs()
                    .child(Label::new(pretty).whitespace_normal())
                    .into_any_element()
            })
            .unwrap_or_else(|| note(cx, i18n_account(cx, "audit_select")).into_any_element());
        v_flex()
            .gap_3()
            .child(note(cx, i18n_account(cx, "audit_intro")))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(200.)).child(Input::new(&form.account).id(AUDIT_ACCOUNT_ID)))
                    // The rest of the row: its placeholder names two events.
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(240.))
                            .child(Input::new(&form.event).id(AUDIT_EVENT_ID)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(200.)).child(form.server.clone()))
                    .child(div().w(px(200.)).child(form.period.clone()))
                    .child(div().flex_1())
                    .child(
                        Button::new(AUDIT_SEARCH_ID)
                            .outline()
                            .icon(gpui_kit::assets::IconName::Search)
                            .label(i18n_account(cx, "audit_search"))
                            .loading(form.busy)
                            .disabled(form.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.load_audit(false, cx))),
                    ),
            )
            .children(status_line(cx, &form.status))
            .child(div().w_full().h(px(TABLE_HEIGHT)).child(table))
            .children(empty.then(|| note(cx, i18n_account(cx, "audit_empty"))))
            .children(
                form.stopped_early
                    .then(|| note(cx, i18n_account(cx, "audit_stopped_early"))),
            )
            .children(form.next.is_some().then(|| {
                Button::new(AUDIT_MORE_ID)
                    .ghost()
                    .label(i18n_account(cx, "audit_more"))
                    .disabled(form.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.load_audit(true, cx)))
            }))
            .child(detail)
            .into_any_element()
    }
}
