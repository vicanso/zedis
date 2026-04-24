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

use crate::{
    assets::CustomIconName,
    connection::get_connection_manager,
    error::Error,
    helpers::get_font_family,
    states::{ServerEvent, ZedisServerState, i18n_common, i18n_config_editor},
};
use gpui::{Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    notification::Notification,
    v_flex,
};
use redis::cmd;
use std::collections::HashMap;
use tracing::error;

type Result<T, E = Error> = std::result::Result<T, E>;

pub struct ZedisConfigEditor {
    server_state: Entity<ZedisServerState>,
    configs: Vec<(SharedString, SharedString)>,
    filter_state: Entity<InputState>,
    filter: String,
    editing_key: Option<SharedString>,
    edit_state: Entity<InputState>,
    loading: bool,
    pending_notification: Option<Notification>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisConfigEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_state = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by key..."));
        let edit_state = cx.new(|cx| InputState::new(window, cx));
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.subscribe(&filter_state, |this, state, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter = state.read(cx).value().to_string();
                cx.notify();
            }
        }));

        subscriptions.push(cx.subscribe(&server_state, |this, _server_state, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.editing_key = None;
                this.configs.clear();
                this.load_configs(cx);
            }
        }));

        let mut this = Self {
            server_state,
            configs: Vec::new(),
            filter_state,
            filter: String::new(),
            editing_key: None,
            edit_state,
            loading: false,
            pending_notification: None,
            _subscriptions: subscriptions,
        };
        this.load_configs(cx);
        this
    }

    fn load_configs(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let map: HashMap<String, String> = cmd("CONFIG").arg("GET").arg("*").query_async(&mut conn).await?;
                let mut configs: Vec<(SharedString, SharedString)> =
                    map.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
                configs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Ok(configs)
            });
            let result: Result<Vec<(SharedString, SharedString)>> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(configs) => this.configs = configs,
                    Err(e) => error!(error = %e, "load configs failed"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_config(&mut self, key: SharedString, value: SharedString, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        cx.spawn(async move |handle, cx| {
            let key_clone = key.clone();
            let value_clone = value.clone();
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("CONFIG")
                    .arg("SET")
                    .arg(key.as_str())
                    .arg(value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            });
            let result: Result<()> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.editing_key = None;
                match result {
                    Ok(()) => {
                        if let Some(entry) = this.configs.iter_mut().find(|(k, _)| k == &key_clone) {
                            entry.1 = value_clone;
                        }
                        this.pending_notification = Some(Notification::success(i18n_config_editor(cx, "save_success")));
                    }
                    Err(e) => {
                        let msg: SharedString = format!("{}: {}", i18n_config_editor(cx, "save_failed"), e).into();
                        this.pending_notification = Some(Notification::error(msg));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ZedisConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let font_family: SharedString = get_font_family().into();
        let filter = self.filter.to_lowercase();

        let filtered: Vec<(SharedString, SharedString)> = self
            .configs
            .iter()
            .filter(|(k, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
            .cloned()
            .collect();

        let editing_key = self.editing_key.clone();
        let edit_state = self.edit_state.clone();

        let stripe_bg = cx.theme().table_even;
        let rows = filtered.into_iter().enumerate().map(|(row_ix, (key, value))| {
            let is_editing = editing_key.as_ref() == Some(&key);
            let is_stripe = row_ix % 2 != 0;

            if is_editing {
                let save_key = key.clone();
                let edit_state_save = edit_state.clone();
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(is_stripe, |this| this.bg(stripe_bg))
                    .child(
                        div()
                            .w(px(280.0))
                            .flex_none()
                            .child(Label::new(key.clone()).text_sm().text_color(cx.theme().foreground)),
                    )
                    .child(
                        div().flex_1().child(
                            Input::new(&edit_state)
                                .font_family(font_family.clone())
                                .appearance(true),
                        ),
                    )
                    .child(
                        Button::new("config-save")
                            .small()
                            .primary()
                            .label(i18n_common(cx, "save"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                let v = edit_state_save.read(cx).value();
                                this.save_config(save_key.clone(), v, cx);
                            })),
                    )
                    .child(
                        Button::new("config-cancel")
                            .small()
                            .ghost()
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_key = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else {
                let edit_key = key.clone();
                let edit_value = value.clone();
                let edit_state_click = edit_state.clone();
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(is_stripe, |this| this.bg(stripe_bg))
                    .child(
                        div()
                            .w(px(280.0))
                            .flex_none()
                            .child(Label::new(key.clone()).text_sm().text_color(cx.theme().foreground)),
                    )
                    .child(
                        div().flex_1().overflow_hidden().child(
                            Label::new(value)
                                .text_sm()
                                .text_ellipsis()
                                .text_color(cx.theme().muted_foreground),
                        ),
                    )
                    .child(
                        Button::new(format!("config-edit-{key}"))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(CustomIconName::FilePenLine))
                            .tooltip(i18n_config_editor(cx, "edit_tooltip"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.editing_key = Some(edit_key.clone());
                                edit_state_click.update(cx, |state, cx| {
                                    state.set_value(edit_value.clone(), window, cx);
                                });
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        });

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Label::new(i18n_config_editor(cx, "title"))
                            .text_sm()
                            .font_family(font_family.clone()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("config-reload")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_common(cx, "reload"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_key = None;
                                this.load_configs(cx);
                            })),
                    )
                    .child(div().w(px(200.0)).child(Input::new(&self.filter_state).small())),
            )
            .child(if self.configs.is_empty() && !self.loading {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Label::new(i18n_config_editor(cx, "no_data")).text_color(cx.theme().muted_foreground))
                    .into_any_element()
            } else {
                div()
                    .id("config-editor-body")
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element()
            })
    }
}
