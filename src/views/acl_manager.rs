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
//! Lists every ACL user (via `ACL USERS` + `ACL GETUSER`), shows their flags
//! / commands / patterns, and lets the operator add/edit/delete them via a
//! plain rules editor that round-trips as `ACL SETUSER`.

use crate::{
    assets::CustomIconName,
    connection::{AclUser, acl_del_user, acl_get_user, acl_list, acl_set_user, acl_whoami, get_connection_manager},
    error::Error,
    states::{Route, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_acl, i18n_common},
};
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use rust_i18n::t;
use tracing::error;
use zedis_ui::ZedisDialog;

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

pub struct ZedisAclManager {
    server_state: Entity<ZedisServerState>,
    users: Vec<AclUser>,
    whoami: SharedString,
    error: Option<SharedString>,
    unsupported: bool,
    loading: bool,
    pending_notification: Option<Notification>,
    _fetch_task: Option<Task<()>>,
    _mutate_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisAclManager {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| match event {
            ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated => this.fetch(cx),
            _ => {}
        }));
        let mut this = Self {
            server_state,
            users: Vec::new(),
            whoami: SharedString::default(),
            error: None,
            unsupported: false,
            loading: false,
            pending_notification: None,
            _fetch_task: None,
            _mutate_task: None,
            _subscriptions: subscriptions,
        };
        this.fetch(cx);
        this
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
                    acl_whoami(&mut conn).await?
                };
                let mut users = Vec::with_capacity(listing.usernames.len());
                for name in &listing.usernames {
                    match acl_get_user(&mut conn, name.as_ref()).await {
                        Ok(u) => users.push(u),
                        Err(e) => {
                            error!(error = %e, user = name.as_ref(), "ACL GETUSER failed");
                        }
                    }
                }
                Ok::<(Vec<AclUser>, SharedString, bool), Error>((users, whoami, listing.unsupported))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((users, whoami, unsupported)) => {
                        this.users = users;
                        this.whoami = whoami;
                        this.unsupported = unsupported;
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
        let username_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_acl(cx, "username_placeholder"))
                .default_value(target.username.clone())
        });
        let rules_state = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder(i18n_acl(cx, "rules_placeholder"))
                .default_value(target.to_rules_text())
        });

        let title = if is_new {
            i18n_acl(cx, "add_user_title")
        } else {
            i18n_acl(cx, "edit_user_title")
        };

        // Capture all i18n strings up front — the dialog `child` closure has
        // no `cx` parameter, so anything pulled from the locale must be cloned
        // into the closure ahead of time.
        let username_label = i18n_acl(cx, "username");
        let rules_label = i18n_acl(cx, "rules_help");
        let presets_label = i18n_acl(cx, "presets");
        let preset_full = i18n_acl(cx, "preset_full");
        let preset_readonly = i18n_acl(cx, "preset_readonly");
        let preset_disabled = i18n_acl(cx, "preset_disabled");
        let preset_clear = i18n_acl(cx, "preset_clear");
        let status_label = i18n_acl(cx, "status_chips");
        let categories_label = i18n_acl(cx, "category_chips");
        let wildcards_label = i18n_acl(cx, "wildcards");

        let body_username = username_state.clone();
        let body_rules = rules_state.clone();

        let username_state_for_submit = username_state.clone();
        let rules_state_for_submit = rules_state.clone();

        ZedisDialog::new(title)
            .w(px(620.))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "save"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                // Toggle a literal token on the rules textarea (add if missing,
                // remove if present). Order otherwise preserved.
                let make_chip = |id: &'static str, token: &'static str| {
                    let rules = body_rules.clone();
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
                };
                // Replace the entire rules textarea with a templated string.
                let make_preset = |id: &'static str, label: SharedString, rules_text: &'static str| {
                    let rules = body_rules.clone();
                    Button::new(id)
                        .small()
                        .outline()
                        .label(label)
                        .on_click(move |_, window, cx| {
                            rules.update(cx, |state, cx| {
                                state.set_value(SharedString::from(rules_text), window, cx);
                            });
                        })
                };

                v_flex()
                    .gap_3()
                    // w_full() picks up the dialog's explicit width set above,
                    // giving the chip rows a real constraint to wrap against.
                    .w_full()
                    .child(Label::new(username_label.clone()))
                    .child(Input::new(&body_username).appearance(true).disabled(!is_new))
                    .child(Label::new(presets_label.clone()).text_xs())
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .child(make_preset("acl-preset-full", preset_full.clone(), "on +@all ~* &*"))
                            .child(make_preset(
                                "acl-preset-ro",
                                preset_readonly.clone(),
                                "on -@all +@read ~* &*",
                            ))
                            .child(make_preset("acl-preset-off", preset_disabled.clone(), "off"))
                            .child(make_preset("acl-preset-clear", preset_clear.clone(), "")),
                    )
                    .child(Label::new(status_label.clone()).text_xs())
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .child(make_chip("acl-chip-on", "on"))
                            .child(make_chip("acl-chip-off", "off"))
                            .child(make_chip("acl-chip-nopass", "nopass"))
                            .child(make_chip("acl-chip-sanitize", "sanitize-payload"))
                            .child(make_chip("acl-chip-skip-sanitize", "skip-sanitize-payload"))
                            .child(make_chip("acl-chip-resetpass", "resetpass")),
                    )
                    .child(Label::new(categories_label.clone()).text_xs())
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .child(make_chip("acl-chip-all", "+@all"))
                            .child(make_chip("acl-chip-read", "+@read"))
                            .child(make_chip("acl-chip-write", "+@write"))
                            .child(make_chip("acl-chip-keyspace", "+@keyspace"))
                            .child(make_chip("acl-chip-pubsub", "+@pubsub"))
                            .child(make_chip("acl-chip-scripting", "+@scripting"))
                            .child(make_chip("acl-chip-no-dangerous", "-@dangerous"))
                            .child(make_chip("acl-chip-no-admin", "-@admin")),
                    )
                    .child(Label::new(wildcards_label.clone()).text_xs())
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .child(make_chip("acl-chip-allkeys", "~*"))
                            .child(make_chip("acl-chip-allchans", "&*"))
                            .child(make_chip("acl-chip-resetkeys", "resetkeys"))
                            .child(make_chip("acl-chip-resetchans", "resetchannels")),
                    )
                    .child(Label::new(rules_label.clone()))
                    .child(Input::new(&body_rules).appearance(true))
            })
            .on_ok(move |_, _window, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let username = username_state_for_submit.read(cx).value().to_string();
                let rules = rules_state_for_submit.read(cx).value().to_string();
                let username = username.trim().to_string();
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
        let rules_vec: Vec<String> = rules.split_whitespace().map(|s| s.to_string()).collect();
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                acl_set_user(&mut conn, &username, &rules_vec).await
            });
            let result: Result<()> = task.await;
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
        let entity = cx.entity().downgrade();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_acl(cx, "delete_user_title");
        let message = t!("acl.delete_user_prompt", user = username.as_ref(), locale = locale).to_string();
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
            let result: Result<()> = task.await;
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
}

impl gpui::Render for ZedisAclManager {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
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
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
                                });
                            }),
                    )
                    .child(Icon::new(IconName::CircleUser))
                    .child(Label::new(title).text_color(cx.theme().foreground))
                    .child(Label::new(count_label).text_color(muted).text_sm())
                    .when(!self.whoami.is_empty(), |this| {
                        this.child(
                            Label::new(format!("WHOAMI = {}", self.whoami))
                                .text_color(muted)
                                .text_xs(),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
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
                    .child(
                        Button::new("acl-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_acl(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.fetch(cx);
                            })),
                    ),
            );

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
            .child(header)
            .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(body))
            .into_any_element()
    }
}

impl ZedisAclManager {
    fn render_user_row(&self, idx: usize, user: AclUser, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let red = cx.theme().red;
        let user_for_edit = user.clone();
        let user_for_delete = user.username.clone();
        let is_default = user.username.as_ref() == "default";

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
                            .child(
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
                                        this.confirm_delete(user_for_delete.clone(), window, cx);
                                    })),
                            ),
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
