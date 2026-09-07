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

//! Redis ACL manager view.
//!
//! Two tabs over one object — this server's ACL:
//!   * **Users**: every ACL user (via `ACL USERS` + `ACL GETUSER`) with
//!     their flags / commands / patterns, an add/edit/delete rules editor
//!     that round-trips as `ACL SETUSER`, a per-user `ACL DRYRUN` tester
//!     (Redis 7.0+) and `ACL GENPASS` in the editor.
//!   * **Security log**: `ACL LOG` — the commands, keys, channels and
//!     authentications the server refused, which is where an application's
//!     `NOPERM` actually explains itself.
//!
//! `ACL SETUSER` only changes the *running* configuration. On a server with
//! an `aclfile` the header therefore offers `ACL SAVE` / `ACL LOAD`, and on
//! one without it says so, because otherwise every edit made here quietly
//! disappears on restart.

use crate::views::unavailable_chip;
use crate::{
    assets::CustomIconName,
    connection::{
        AclDryRun, AclLogEntry, AclUser, Capability, ServerCommand, acl_del_user, acl_dryrun, acl_file, acl_genpass,
        acl_get_user, acl_list, acl_load, acl_log, acl_log_reset, acl_save, acl_set_user, acl_whoami, floors,
        get_connection_manager, split_acl_rules,
    },
    error::Error,
    helpers::{format_duration, format_unix_secs, get_mono_font_family},
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
        dialog_button_props, escalate_dangerous_body, i18n_acl, i18n_common,
    },
};
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    label::Label,
    v_flex,
};
use rust_i18n::t;
use std::time::Duration;
use tracing::error;
use zedis_ui::{TextColumn, ZedisDialog, ZedisTextTable};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Add/remove `token` in a whitespace-separated rule string. Used by the
/// chip buttons in the editor — clicking the same chip twice undoes itself.
/// Tokens are expected to be whitespace-free literals like `+@read`, `~*`,
/// `nopass`. Order is preserved otherwise.
fn toggle_rule_token(current: &str, token: &str) -> String {
    let mut tokens: Vec<&str> = current.split_whitespace().collect();
    if let Some(idx) = tokens.iter().position(|t| *t == token) {
        tokens.remove(idx);
    } else {
        tokens.push(token);
    }
    tokens.join(" ")
}

const COL_LOG_TIME: &str = "log_time";
const COL_LOG_USER: &str = "log_user";
const COL_LOG_REASON: &str = "log_reason";
const COL_LOG_OBJECT: &str = "log_object";
const COL_LOG_CONTEXT: &str = "log_context";
const COL_LOG_COUNT: &str = "log_count";
const COL_LOG_CLIENT: &str = "log_client";

/// Payload cells after the seven columns: what the two numeric columns
/// actually sort by.
const LOG_CELL_WHEN: usize = 7;
const LOG_CELL_COUNT: usize = 8;

/// One `ACL LOG` entry as table cells. `time` is the entry's creation
/// timestamp where the server reports one (Redis 7.0+) and its age
/// otherwise, so a Redis 6 log still reads in order.
fn log_cells(entry: &AclLogEntry) -> Vec<SharedString> {
    let (when, when_sort) = if entry.timestamp_created > 0 {
        let seconds = entry.timestamp_created / 1_000;
        (
            format_unix_secs(seconds).unwrap_or_default().into(),
            seconds.to_string(),
        )
    } else {
        // Age counts *down* into the past, so negate it to sort like a
        // timestamp: the most recent event first.
        (
            SharedString::from(format_duration(Duration::from_secs(entry.age_seconds.max(0.0) as u64))),
            (-(entry.age_seconds as i64)).to_string(),
        )
    };
    vec![
        when,
        entry.username.clone().into(),
        entry.reason.clone().into(),
        entry.object.clone().into(),
        entry.context.clone().into(),
        entry.count.to_string().into(),
        entry.client_info.clone().into(),
        when_sort.into(),
        entry.count.to_string().into(),
    ]
}

/// The security-log grid: fixed columns with the client info taking the
/// rest, filtered and exported through the shared text table.
fn log_table(window: &mut Window, cx: &mut gpui::App) -> ZedisTextTable {
    let content_width = content_area_width(window, cx).as_f32();
    let time_w = 190.;
    let user_w = 130.;
    let reason_w = 110.;
    let object_w = 200.;
    let context_w = 110.;
    let count_w = 90.;
    let client_w = (content_width - time_w - user_w - reason_w - object_w - context_w - count_w - 26.).max(200.);
    let title = |key: &'static str| i18n_acl(cx, key);
    let columns = vec![
        TextColumn::new(COL_LOG_TIME, title(COL_LOG_TIME), time_w).sort_by_cell(LOG_CELL_WHEN),
        TextColumn::new(COL_LOG_USER, title(COL_LOG_USER), user_w).sortable(),
        TextColumn::new(COL_LOG_REASON, title(COL_LOG_REASON), reason_w).sortable(),
        TextColumn::new(COL_LOG_OBJECT, title(COL_LOG_OBJECT), object_w).sortable(),
        TextColumn::new(COL_LOG_CONTEXT, title(COL_LOG_CONTEXT), context_w).sortable(),
        TextColumn::new(COL_LOG_COUNT, title(COL_LOG_COUNT), count_w).sort_by_cell(LOG_CELL_COUNT),
        TextColumn::new(COL_LOG_CLIENT, title(COL_LOG_CLIENT), client_w),
    ];
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
}

/// How many `ACL LOG` entries are read at a time. The server's own
/// `acllog-max-len` (128 by default) is the real ceiling; this caps the
/// reply on one configured to keep far more.
const LOG_FETCH_LIMIT: u64 = 512;

/// The two halves of the ACL page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AclTab {
    #[default]
    Users,
    Log,
}

pub struct ZedisAclManager {
    server_state: Entity<ZedisServerState>,
    tab: AclTab,
    users: Vec<AclUser>,
    whoami: SharedString,
    /// The server's `aclfile`, when it has one — what decides whether
    /// `ACL SAVE` / `LOAD` are offered at all.
    acl_file: Option<SharedString>,
    error: Option<SharedString>,
    unsupported: bool,
    loading: bool,
    /// `ACL LOG` rows. Fetched only once the log tab is opened, and by its
    /// own Refresh after that — a security log does not need polling.
    log_table: Entity<TableState<ZedisTextTable>>,
    log_count: usize,
    log_loading: bool,
    pending_notification: Option<Notification>,
    _fetch_task: Option<Task<()>>,
    _log_task: Option<Task<()>>,
    _mutate_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisAclManager {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| match event {
            ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated => this.fetch(cx),
            _ => {}
        }));
        let log_table = cx.new(|cx| TableState::new(log_table(window, cx), window, cx));
        let mut this = Self {
            server_state,
            tab: AclTab::Users,
            users: Vec::new(),
            whoami: SharedString::default(),
            acl_file: None,
            error: None,
            unsupported: false,
            loading: false,
            log_table,
            log_count: 0,
            log_loading: false,
            pending_notification: None,
            _fetch_task: None,
            _log_task: None,
            _mutate_task: None,
            _subscriptions: subscriptions,
        };
        this.fetch(cx);
        this
    }

    fn set_tab(&mut self, tab: AclTab, cx: &mut gpui::Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        // First visit to the log tab pulls it; later visits keep what is
        // there until Refresh.
        if tab == AclTab::Log && self.log_count == 0 && !self.log_loading {
            self.fetch_log(cx);
        }
        cx.notify();
    }

    /// Whether `ACL LOG` is usable here — the log tab is otherwise a chip
    /// naming the reason.
    fn log_block(&self, cx: &gpui::App) -> Option<crate::connection::CommandStatus> {
        self.server_state.read(cx).command_block(ServerCommand::AclLog)
    }

    fn fetch_log(&mut self, cx: &mut gpui::Context<Self>) {
        if self.log_loading || self.log_block(cx).is_some() {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self.log_loading = true;
        cx.notify();
        self._log_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_log(&mut conn, LOG_FETCH_LIMIT).await
            });
            let result: Result<Vec<AclLogEntry>> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.log_loading = false;
                match result {
                    Ok(entries) => {
                        let rows: Vec<Vec<SharedString>> = entries.iter().map(log_cells).collect();
                        this.log_count = rows.len();
                        this.log_table
                            .update(cx, |state, _| state.delegate_mut().set_rows(rows));
                    }
                    Err(e) => {
                        // A NOPERM / unknown-subcommand reply degrades the
                        // feature matrix (and explains itself once).
                        let explained = this
                            .server_state
                            .update(cx, |state, cx| state.note_command_error(&e, cx));
                        if !explained {
                            this.pending_notification = Some(Notification::error(e.to_string()));
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn fetch(&mut self, cx: &mut gpui::Context<Self>) {
        if self.loading {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self.loading = true;
        self._fetch_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let listing = acl_list(&mut conn).await?;
                let whoami = if listing.unsupported {
                    SharedString::default()
                } else {
                    acl_whoami(&mut conn).await?.into()
                };
                let mut users = Vec::with_capacity(listing.usernames.len());
                for name in &listing.usernames {
                    match acl_get_user(&mut conn, name.as_ref()).await {
                        Ok(u) => users.push(u),
                        Err(e) => {
                            error!(error = %e, user = name.as_str(), "ACL GETUSER failed");
                        }
                    }
                }
                // Whether edits made here survive a restart hangs on this.
                let file = if listing.unsupported {
                    None
                } else {
                    acl_file(&mut conn).await?
                };
                Ok::<(Vec<AclUser>, SharedString, bool, Option<String>), Error>((
                    users,
                    whoami,
                    listing.unsupported,
                    file,
                ))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((users, whoami, unsupported, file)) => {
                        this.users = users;
                        this.whoami = whoami;
                        this.unsupported = unsupported;
                        this.acl_file = file.map(SharedString::from);
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn open_editor(&mut self, target: AclUser, is_new: bool, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let entity = cx.entity().downgrade();
        let server_state = self.server_state.clone();
        let editor = cx.new(|cx| ZedisAclEditor::new(server_state, &target, is_new, window, cx));

        let title = if is_new {
            i18n_acl(cx, "add_user_title")
        } else {
            i18n_acl(cx, "edit_user_title")
        };

        let body = editor.clone();
        ZedisDialog::new(title)
            .w(px(620.))
            .ok_text(i18n_common(cx, "save"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "save"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let form = editor.read(cx);
                let username = form.username_state.read(cx).value().trim().to_string();
                let rules = form.rules_state.read(cx).value().to_string();
                if username.is_empty() {
                    return true;
                }
                this.update(cx, |this, cx| {
                    this.submit_set_user(username, rules, cx);
                });
                true
            })
            .open(window, cx);
    }

    fn submit_set_user(&mut self, username: String, rules: String, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        // Not a plain whitespace split: a `( … )` selector group must reach
        // SETUSER as one argument — splitting it used to break saving any
        // rules line that contained a selector.
        let rules_vec: Vec<String> = split_acl_rules(&rules);
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_set_user(&mut conn, &username, &rules_vec).await
            });
            let result: Result<()> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                this.pending_notification = Some(match result {
                    Ok(()) => {
                        let msg = t!("acl.set_user_success", locale = locale).to_string();
                        Notification::success(msg)
                    }
                    Err(e) => {
                        let msg = t!("acl.set_user_failed", error = e.to_string(), locale = locale).to_string();
                        Notification::error(msg)
                    }
                });
                this.fetch(cx);
            });
        }));
    }

    fn confirm_delete(&mut self, username: SharedString, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::AclWrite) {
            return;
        }
        let entity = cx.entity().downgrade();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_acl(cx, "delete_user_title");
        let message = t!("acl.delete_user_prompt", user = username.as_ref(), locale = locale).to_string();
        let message = escalate_dangerous_body(cx, &server_id, message);
        let username_for_run = username.clone();
        ZedisDialog::new_alert(title, message)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, _, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let user = username_for_run.clone();
                this.update(cx, |this, cx| this.submit_delete(user, cx));
                true
            })
            .open(window, cx);
    }

    fn submit_delete(&mut self, username: SharedString, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_del_user(&mut conn, username.as_ref()).await
            });
            let result: Result<()> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                this.pending_notification = Some(match result {
                    Ok(()) => Notification::success(t!("acl.delete_user_success", locale = locale).to_string()),
                    Err(e) => {
                        let msg = t!("acl.delete_user_failed", error = e.to_string(), locale = locale).to_string();
                        Notification::error(msg)
                    }
                });
                this.fetch(cx);
            });
        }));
    }

    /// `ACL SAVE` / `ACL LOAD` / `ACL LOG RESET` all share this: one
    /// command, one toast, then reload what it changed.
    fn run_acl_op(&mut self, op: AclOp, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                match op {
                    AclOp::Save => acl_save(&mut conn).await,
                    AclOp::Load => acl_load(&mut conn).await,
                    AclOp::LogReset => acl_log_reset(&mut conn).await,
                }
            });
            let result: Result<()> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                match result {
                    Ok(()) => {
                        this.pending_notification =
                            Some(Notification::success(t!(op.done_key(), locale = &locale).to_string()));
                        match op {
                            AclOp::LogReset => {
                                this.log_count = 0;
                                this.log_table.update(cx, |state, _| state.delegate_mut().clear());
                            }
                            // LOAD replaces every user; SAVE leaves the
                            // running config alone but a reload costs
                            // nothing and confirms what landed.
                            AclOp::Save | AclOp::Load => this.fetch(cx),
                        }
                    }
                    Err(e) => {
                        let explained = this
                            .server_state
                            .update(cx, |state, cx| state.note_command_error(&e, cx));
                        if !explained {
                            let msg = t!("acl.op_failed", error = e.to_string(), locale = &locale).to_string();
                            this.pending_notification = Some(Notification::error(msg));
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    /// `ACL LOAD` throws away every runtime change, and `ACL LOG RESET`
    /// every recorded event — both ask first, escalated on a tagged server.
    fn confirm_acl_op(&mut self, op: AclOp, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let (Some(title_key), Some(body_key)) = (op.confirm_title_key(), op.confirm_body_key()) else {
            self.run_acl_op(op, cx);
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = t!(title_key, locale = locale).to_string();
        let body = t!(body_key, locale = locale).to_string();
        let body = escalate_dangerous_body(cx, &server_id, body);
        let entity = cx.entity().downgrade();
        ZedisDialog::new_alert(title, body)
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, _window, cx| {
                if let Some(this) = entity.upgrade() {
                    this.update(cx, |this, cx| this.run_acl_op(op, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// The `ACL DRYRUN` tester for one user: would they be allowed to run
    /// this command, without running it.
    fn open_dryrun(&mut self, username: SharedString, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = t!("acl.dryrun_title", user = username.as_ref(), locale = locale).to_string();
        let view = cx.new(|cx| ZedisAclDryRun::new(self.server_state.clone(), username, window, cx));
        ZedisDialog::new(title)
            .icon(IconName::CircleUser)
            .w(px(560.))
            .child(move || view.clone())
            .ok_text(i18n_common(cx, "close"))
            .open(window, cx);
    }
}

/// The three one-shot ACL commands the header offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AclOp {
    Save,
    Load,
    LogReset,
}

impl AclOp {
    fn done_key(self) -> &'static str {
        match self {
            AclOp::Save => "acl.save_done",
            AclOp::Load => "acl.load_done",
            AclOp::LogReset => "acl.log_reset_done",
        }
    }

    /// `None` for the one that changes nothing a user can lose.
    fn confirm_title_key(self) -> Option<&'static str> {
        match self {
            AclOp::Save => None,
            AclOp::Load => Some("acl.load_confirm_title"),
            AclOp::LogReset => Some("acl.log_reset_confirm_title"),
        }
    }

    fn confirm_body_key(self) -> Option<&'static str> {
        match self {
            AclOp::Save => None,
            AclOp::Load => Some("acl.load_confirm_body"),
            AclOp::LogReset => Some("acl.log_reset_confirm_body"),
        }
    }
}

impl gpui::Render for ZedisAclManager {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // ACL SETUSER / DELUSER are server writes (Capability::AclWrite);
        // read-only keeps the list and whoami visible but hides the editors.
        let can_write = self.server_state.read(cx).can(Capability::AclWrite);
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let muted = cx.theme().muted_foreground;
        let title = i18n_acl(cx, "title");
        let count_label = if self.users.is_empty() {
            String::new()
        } else {
            format!("({})", self.users.len())
        };
        let on_users = self.tab == AclTab::Users;
        let tab_button = |id: &'static str, key: &'static str, tab: AclTab, cx: &mut gpui::Context<Self>| {
            let active = self.tab == tab;
            Button::new(id)
                .xsmall()
                .when(active, |b| b.primary())
                .when(!active, |b| b.outline())
                .label(i18n_acl(cx, key))
                .on_click(cx.listener(move |this, _, _w, cx| this.set_tab(tab, cx)))
        };
        let header = h_flex()
            .items_center()
            .justify_between()
            .px_4()
            .h(px(40.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("acl-back")
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
                    .child(Icon::new(IconName::CircleUser))
                    .child(Label::new(title).text_color(cx.theme().foreground))
                    .when(!self.unsupported, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(tab_button("acl-tab-users", "tab_users", AclTab::Users, cx))
                                .child(tab_button("acl-tab-log", "tab_log", AclTab::Log, cx)),
                        )
                    })
                    .when(on_users, |this| {
                        this.child(Label::new(count_label.clone()).text_color(muted).text_sm())
                            .when(!self.whoami.is_empty(), |this| {
                                this.child(
                                    Label::new(format!("WHOAMI = {}", self.whoami))
                                        .text_color(muted)
                                        .text_xs(),
                                )
                            })
                    })
                    .when(!on_users && self.log_count > 0, |this| {
                        this.child(Label::new(format!("({})", self.log_count)).text_color(muted).text_sm())
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(on_users && can_write, |this| {
                        this.child(
                            Button::new("acl-add-user")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label(i18n_acl(cx, "add_user"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let target = AclUser {
                                        flags: vec!["on".into()],
                                        enabled: true,
                                        ..Default::default()
                                    };
                                    this.open_editor(target, true, window, cx);
                                })),
                        )
                    })
                    // Only a server with an aclfile can save or load one;
                    // elsewhere the hint under the header says why not.
                    .when(on_users && can_write && self.acl_file.is_some(), |this| {
                        this.child(
                            Button::new("acl-save")
                                .outline()
                                .small()
                                .icon(Icon::new(CustomIconName::Save))
                                .label(i18n_acl(cx, "save"))
                                .tooltip(i18n_acl(cx, "save_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_acl_op(AclOp::Save, window, cx);
                                })),
                        )
                        .child(
                            Button::new("acl-load")
                                .outline()
                                .small()
                                .icon(Icon::new(CustomIconName::Undo2))
                                .label(i18n_acl(cx, "load"))
                                .tooltip(i18n_acl(cx, "load_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_acl_op(AclOp::Load, window, cx);
                                })),
                        )
                    })
                    .when(on_users, |this| {
                        this.when_some(
                            self.server_state.read(cx).blocked_by(Capability::AclWrite),
                            |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                        )
                    })
                    .when(!on_users && can_write && self.log_count > 0, |this| {
                        this.child(
                            Button::new("acl-log-reset")
                                .outline()
                                .small()
                                .icon(IconName::CircleX)
                                .label(i18n_acl(cx, "log_reset"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_acl_op(AclOp::LogReset, window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("acl-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(if on_users {
                                i18n_acl(cx, "refresh_tooltip")
                            } else {
                                i18n_acl(cx, "log_refresh_tooltip")
                            })
                            .loading(if on_users { self.loading } else { self.log_loading })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                if this.tab == AclTab::Users {
                                    this.fetch(cx);
                                } else {
                                    this.fetch_log(cx);
                                }
                            })),
                    ),
            );

        // Where the users actually live. `ACL SETUSER` only changes the
        // running configuration, so a server with an aclfile needs a SAVE
        // and one without needs to say that a restart undoes this page.
        let persistence_hint: Option<SharedString> =
            (on_users && !self.unsupported && self.error.is_none()).then(|| match &self.acl_file {
                Some(path) => {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    t!("acl.file_label", path = path.as_ref(), locale = locale)
                        .to_string()
                        .into()
                }
                None => i18n_acl(cx, "runtime_only_hint"),
            });

        if !on_users {
            let log_body = if let Some(status) = self.log_block(cx) {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_full()
                    .child(unavailable_chip(cx, ServerCommand::AclLog, status))
                    .into_any_element()
            } else if self.log_count == 0 {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_full()
                    .px_6()
                    .child(
                        Label::new(if self.log_loading {
                            i18n_common(cx, "loading")
                        } else {
                            i18n_acl(cx, "log_empty")
                        })
                        .text_color(muted)
                        .whitespace_normal(),
                    )
                    .into_any_element()
            } else {
                DataTable::new(&self.log_table)
                    .stripe(true)
                    .bordered(false)
                    .scrollbar_visible(true, true)
                    .into_any_element()
            };
            return v_flex()
                .size_full()
                .overflow_hidden()
                .font_family(get_mono_font_family())
                .child(header)
                .child(div().flex_1().w_full().min_h_0().child(log_body))
                .into_any_element();
        }

        let body = if self.unsupported {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_acl(cx, "unsupported")).text_color(muted))
                .into_any_element()
        } else if let Some(err) = &self.error {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(err.clone()).text_color(cx.theme().red))
                .into_any_element()
        } else if self.users.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(
                    Label::new(if self.loading {
                        i18n_common(cx, "loading")
                    } else {
                        i18n_acl(cx, "empty")
                    })
                    .text_color(muted),
                )
                .into_any_element()
        } else {
            let users = self.users.clone();
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(users.len());
            for (idx, user) in users.into_iter().enumerate() {
                rows.push(self.render_user_row(idx, user, cx).into_any_element());
            }
            v_flex().gap_2().p_4().w_full().children(rows).into_any_element()
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .child(header)
            .children(persistence_hint.map(|hint| {
                div()
                    .w_full()
                    .flex_none()
                    .px_4()
                    .py_1()
                    .child(Label::new(hint).text_xs().text_color(muted).whitespace_normal())
            }))
            .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(body))
            .into_any_element()
    }
}

impl ZedisAclManager {
    fn render_user_row(&self, idx: usize, user: AclUser, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let can_write = self.server_state.read(cx).can(Capability::AclWrite);
        let muted = cx.theme().muted_foreground;
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let red = cx.theme().red;
        let user_for_edit = user.clone();
        let user_for_delete = user.username.clone();
        let user_for_dryrun = user.username.clone();
        let is_default = user.username.as_str() == "default";
        // `ACL DRYRUN` is Redis 7.0 (ACL v2).
        let supports_dryrun = self.server_state.read(cx).supports(floors::ACL_V2);

        let flags_label = if user.flags.is_empty() {
            "—".to_string()
        } else {
            user.flags.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" ")
        };
        let commands = if user.commands.is_empty() {
            "—".into()
        } else {
            user.commands.clone()
        };
        let keys_summary: SharedString = if user.keys.is_empty() {
            "—".into()
        } else {
            user.keys
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" ")
                .into()
        };
        let channels_summary: SharedString = if user.channels.is_empty() {
            "—".into()
        } else {
            user.channels
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" ")
                .into()
        };

        let status_color = if user.enabled { primary } else { red };

        v_flex()
            .id(("acl-row", idx))
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(bg)
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div().px_1p5().rounded_sm().bg(status_color).child(
                                    Label::new(if user.enabled { "on" } else { "off" })
                                        .text_xs()
                                        .text_color(bg),
                                ),
                            )
                            .child(Label::new(user.username.clone()).text_color(cx.theme().foreground))
                            .child(Label::new(flags_label).text_color(muted).text_xs()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            // Read-only, so it stays available on a locked
                            // connection — "would this user be allowed to"
                            // is exactly what you ask before granting.
                            .when(supports_dryrun, |this| {
                                this.child(
                                    Button::new(("acl-dryrun", idx))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Check)
                                        // Labelled, unlike the edit / delete
                                        // glyphs beside it: a check mark on
                                        // its own reads as "confirm", not
                                        // "test this user's permissions".
                                        .label(i18n_acl(cx, "dryrun_run"))
                                        .tooltip(i18n_acl(cx, "dryrun_tooltip"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_dryrun(user_for_dryrun.clone().into(), window, cx);
                                        })),
                                )
                            })
                            .when(can_write, |this| {
                                this.child(
                                    Button::new(("acl-edit", idx))
                                        .ghost()
                                        .small()
                                        .icon(CustomIconName::FilePenLine)
                                        .tooltip(i18n_acl(cx, "edit_tooltip"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_editor(user_for_edit.clone(), false, window, cx);
                                        })),
                                )
                                .child(
                                    Button::new(("acl-delete", idx))
                                        .ghost()
                                        .small()
                                        .disabled(is_default)
                                        .icon(CustomIconName::FileXCorner)
                                        .tooltip(i18n_acl(cx, "delete_tooltip"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.confirm_delete(user_for_delete.clone().into(), window, cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Label::new(i18n_acl(cx, "commands")).text_color(muted).text_xs())
                    .child(Label::new(commands).text_xs().whitespace_normal()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Label::new(i18n_acl(cx, "keys")).text_color(muted).text_xs())
                    .child(Label::new(keys_summary).text_xs().whitespace_normal()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Label::new(i18n_acl(cx, "channels")).text_color(muted).text_xs())
                    .child(Label::new(channels_summary).text_xs().whitespace_normal()),
            )
            // ACL v2 selectors (7.0+): each additional permission group on
            // its own line, in the exact `( … )` syntax SETUSER accepts —
            // what you read here is what you can paste into the editor.
            .when(!user.selectors.is_empty(), |this| {
                let mut groups = v_flex().gap_0p5().flex_1().min_w_0();
                for selector in &user.selectors {
                    groups = groups.child(Label::new(selector.to_rule_token()).text_xs().whitespace_normal());
                }
                this.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(Label::new(i18n_acl(cx, "selectors")).text_color(muted).text_xs())
                        .child(groups),
                )
            })
            .when(!user.password_digests.is_empty(), |this| {
                let pw_summary: SharedString = user
                    .password_digests
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into();
                this.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(Label::new(i18n_acl(cx, "passwords")).text_color(muted).text_xs())
                        .child(Label::new(pw_summary).text_xs().whitespace_normal()),
                )
            })
    }
}

/// The add/edit form for one ACL user, as a **view entity** rather than
/// elements built inline in the dialog's `child` closure — `InputState` is
/// itself a view, so it wants a stable host (this is the shape
/// `key_tag_dialog` uses).
///
/// The chip rows are laid out as fixed rows instead of one `flex_wrap()` row:
/// a wrapping flex container in the dialog body left the sibling `Input`s
/// unable to render their text or hold a click.
struct ZedisAclEditor {
    server_state: Entity<ZedisServerState>,
    username_state: Entity<InputState>,
    rules_state: Entity<TextareaState>,
    /// Username is the identity of an ACL user, so it is only editable while
    /// creating one.
    is_new: bool,
    _genpass_task: Option<Task<()>>,
}

impl ZedisAclEditor {
    fn new(
        server_state: Entity<ZedisServerState>,
        target: &AclUser,
        is_new: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let username_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_acl(cx, "username_placeholder"))
                .default_value(target.username.clone())
        });
        let rules_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder(i18n_acl(cx, "rules_placeholder"))
                .default_value(target.to_rules_text())
        });
        Self {
            server_state,
            username_state,
            rules_state,
            is_new,
            _genpass_task: None,
        }
    }

    /// `ACL GENPASS` — append the server's own CSPRNG password to the rules
    /// as a `>secret` token. It lands in the textarea in plaintext on
    /// purpose: this is the one moment the password can still be copied,
    /// since `GETUSER` only ever returns its digest.
    fn generate_password(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self._genpass_task = Some(cx.spawn_in(window, async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_genpass(&mut conn, None).await
            });
            let result: Result<String> = task.await.map_err(Into::into);
            let _ = handle.update_in(cx, |this, window, cx| match result {
                Ok(password) => this.rules_state.update(cx, |state, cx| {
                    let current = state.value().to_string();
                    let next = if current.trim().is_empty() {
                        format!(">{password}")
                    } else {
                        format!("{} >{password}", current.trim_end())
                    };
                    state.set_value(SharedString::from(next), window, cx);
                }),
                Err(e) => {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    let msg = t!("acl.op_failed", error = e.to_string(), locale = &locale).to_string();
                    window.push_notification(Notification::error(msg), cx);
                }
            });
        }));
    }

    /// Toggle a literal token on the rules textarea — clicking the same chip
    /// twice undoes itself. Order is otherwise preserved.
    fn chip(&self, id: &'static str, token: &'static str) -> Button {
        let rules = self.rules_state.clone();
        Button::new(id)
            .small()
            .ghost()
            .label(token)
            .on_click(move |_, window, cx| {
                rules.update(cx, |state, cx| {
                    let current = state.value().to_string();
                    let next = toggle_rule_token(&current, token);
                    state.set_value(SharedString::from(next), window, cx);
                });
            })
    }

    /// Replace the whole rules textarea with a templated rule string.
    fn preset(&self, id: &'static str, label: SharedString, rules_text: &'static str) -> Button {
        let rules = self.rules_state.clone();
        Button::new(id)
            .small()
            .outline()
            .label(label)
            .on_click(move |_, window, cx| {
                rules.update(cx, |state, cx| {
                    state.set_value(SharedString::from(rules_text), window, cx);
                });
            })
    }

    fn chip_row(&self, chips: impl IntoIterator<Item = Button>) -> impl IntoElement {
        h_flex().w_full().gap_2().children(chips)
    }
}

impl gpui::Render for ZedisAclEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .w_full()
            .child(Label::new(i18n_acl(cx, "username")))
            .child(Input::new(&self.username_state).appearance(true).disabled(!self.is_new))
            .child(Label::new(i18n_acl(cx, "presets")).text_xs())
            .child(self.chip_row([
                self.preset("acl-preset-full", i18n_acl(cx, "preset_full"), "on +@all ~* &*"),
                self.preset(
                    "acl-preset-ro",
                    i18n_acl(cx, "preset_readonly"),
                    "on -@all +@read ~* &*",
                ),
                self.preset("acl-preset-off", i18n_acl(cx, "preset_disabled"), "off"),
                self.preset("acl-preset-clear", i18n_acl(cx, "preset_clear"), ""),
            ]))
            .child(Label::new(i18n_acl(cx, "status_chips")).text_xs())
            .child(self.chip_row([
                self.chip("acl-chip-on", "on"),
                self.chip("acl-chip-off", "off"),
                self.chip("acl-chip-nopass", "nopass"),
                self.chip("acl-chip-sanitize", "sanitize-payload"),
                self.chip("acl-chip-skip-sanitize", "skip-sanitize-payload"),
                self.chip("acl-chip-resetpass", "resetpass"),
            ]))
            .child(Label::new(i18n_acl(cx, "category_chips")).text_xs())
            .child(self.chip_row([
                self.chip("acl-chip-all", "+@all"),
                self.chip("acl-chip-read", "+@read"),
                self.chip("acl-chip-write", "+@write"),
                self.chip("acl-chip-keyspace", "+@keyspace"),
                self.chip("acl-chip-pubsub", "+@pubsub"),
                self.chip("acl-chip-scripting", "+@scripting"),
            ]))
            .child(self.chip_row([
                self.chip("acl-chip-no-dangerous", "-@dangerous"),
                self.chip("acl-chip-no-admin", "-@admin"),
            ]))
            .child(Label::new(i18n_acl(cx, "wildcards")).text_xs())
            .child(self.chip_row([
                self.chip("acl-chip-allkeys", "~*"),
                self.chip("acl-chip-allchans", "&*"),
                self.chip("acl-chip-resetkeys", "resetkeys"),
                self.chip("acl-chip-resetchans", "resetchannels"),
            ]))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(Label::new(i18n_acl(cx, "rules_help")))
                    .child(
                        Button::new("acl-genpass")
                            .small()
                            .outline()
                            .icon(IconName::Asterisk)
                            .label(i18n_acl(cx, "genpass"))
                            .tooltip(i18n_acl(cx, "genpass_tooltip"))
                            .on_click(cx.listener(|this, _, window, cx| this.generate_password(window, cx))),
                    ),
            )
            .child(Textarea::new(&self.rules_state).appearance(true))
    }
}

/// The `ACL DRYRUN` tester: one user, one command, and the server's own
/// verdict — without running the command. A view entity because it holds an
/// `Input` (see CLAUDE.md's dialog note).
struct ZedisAclDryRun {
    server_state: Entity<ZedisServerState>,
    username: SharedString,
    command_state: Entity<InputState>,
    /// `Some(Ok(()))` allowed, `Some(Err(reason))` refused, `None` before
    /// the first run.
    verdict: Option<std::result::Result<(), SharedString>>,
    running: bool,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisAclDryRun {
    fn new(
        server_state: Entity<ZedisServerState>,
        username: SharedString,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let command_state = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(i18n_acl(cx, "dryrun_command_placeholder"));
            input.focus(window, cx);
            input
        });
        let subscription = cx.subscribe_in(&command_state, window, |this, _state, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.run(cx);
            }
        });
        Self {
            server_state,
            username,
            command_state,
            verdict: None,
            running: false,
            _task: None,
            _subscriptions: vec![subscription],
        }
    }

    fn run(&mut self, cx: &mut gpui::Context<Self>) {
        let command = self.command_state.read(cx).value().trim().to_string();
        if command.is_empty() || self.running {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        let username = self.username.to_string();
        // The command line as `ACL DRYRUN` wants it: the command and its
        // arguments, already split.
        let args: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        self.running = true;
        self.verdict = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_dryrun(&mut conn, &username, &args).await
            });
            let result: Result<AclDryRun> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running = false;
                this.verdict = Some(match result {
                    Ok(AclDryRun::Allowed) => Ok(()),
                    Ok(AclDryRun::Denied(reason)) => Err(reason.into()),
                    Err(e) => Err(e.to_string().into()),
                });
                cx.notify();
            });
        }));
    }
}

impl gpui::Render for ZedisAclDryRun {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, success, danger) = (theme.muted_foreground, theme.success, theme.danger);
        let verdict = self.verdict.clone().map(|result| match result {
            Ok(()) => (i18n_acl(cx, "dryrun_allowed"), success),
            Err(reason) if reason.is_empty() => (i18n_acl(cx, "dryrun_denied"), danger),
            Err(reason) => (reason, danger),
        });

        v_flex()
            .gap_3()
            .w_full()
            .child(
                Label::new(i18n_acl(cx, "dryrun_hint"))
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal(),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.command_state).appearance(true).flex_1())
                    .child(
                        Button::new("acl-dryrun-run")
                            .outline()
                            .icon(IconName::Play)
                            .label(i18n_acl(cx, "dryrun_run"))
                            .loading(self.running)
                            .disabled(self.running)
                            .on_click(cx.listener(|this, _, _window, cx| this.run(cx))),
                    ),
            )
            .children(verdict.map(|(text, color)| {
                Label::new(text)
                    .text_sm()
                    .text_color(color)
                    .whitespace_normal()
                    .font_family(get_mono_font_family())
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::toggle_rule_token;

    #[test]
    fn toggle_appends_when_absent() {
        assert_eq!(toggle_rule_token("on +@read", "~*"), "on +@read ~*");
        assert_eq!(toggle_rule_token("", "on"), "on");
    }

    #[test]
    fn toggle_removes_when_present() {
        assert_eq!(toggle_rule_token("on +@read ~*", "+@read"), "on ~*");
        assert_eq!(toggle_rule_token("on", "on"), "");
    }

    #[test]
    fn toggle_only_matches_whole_tokens() {
        // ~user:* and ~* are different tokens — toggling ~* should not touch ~user:*
        assert_eq!(toggle_rule_token("on ~user:* ~*", "~*"), "on ~user:*");
        assert_eq!(toggle_rule_token("on ~user:*", "~*"), "on ~user:* ~*");
    }
}
