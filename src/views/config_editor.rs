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
    connection::{DangerKind, get_connection_manager, get_server, get_servers},
    error::Error,
    helpers::get_font_family,
    states::{
        Route, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_config_editor,
    },
    views::{ZedisCopyKeyDialog, confirm_dangerous_command},
};
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants, DropdownButton},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, NumberInput},
    label::Label,
    menu::PopupMenuItem,
    notification::Notification,
    spinner::Spinner,
    v_flex,
};
use redis::cmd;
use std::collections::{BTreeSet, HashMap};
use tracing::error;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// A computed `CONFIG GET *` diff between the active server and another:
/// only the parameters whose values differ, plus the two column labels.
struct ConfigDiff {
    local_label: SharedString,
    other_label: SharedString,
    /// `(parameter, local value, other value)`; a value absent on one side is
    /// an empty string.
    rows: Vec<(SharedString, SharedString, SharedString)>,
}

/// Editing control inferred for a config value. A curated catalog of known
/// enum parameters (by name) is consulted first; otherwise the type is guessed
/// from the current value: `yes`/`no` → checkbox, a parseable number → numeric
/// input, anything else → plain text.
#[derive(Clone, Copy, PartialEq)]
enum ConfigKind {
    /// Known enum parameter — pick from a fixed candidate list (a dropdown).
    Enum(&'static [&'static str]),
    Bool,
    Number,
    Text,
}

/// Candidate values for well-known enum config parameters. Curated and kept
/// version-independent (only options stable across Redis versions); the editor
/// always also offers the server's current value, so an option added in a newer
/// Redis is never lost. Parameters not listed fall back to value inference.
fn config_enum_options(key: &str) -> Option<&'static [&'static str]> {
    let opts: &[&str] = match key {
        "maxmemory-policy" => &[
            "noeviction",
            "allkeys-lru",
            "allkeys-lfu",
            "allkeys-random",
            "volatile-lru",
            "volatile-lfu",
            "volatile-random",
            "volatile-ttl",
        ],
        "appendfsync" => &["everysec", "always", "no"],
        "loglevel" => &["debug", "verbose", "notice", "warning", "nothing"],
        "tls-auth-clients" => &["no", "yes", "optional"],
        "repl-diskless-load" => &["disabled", "on-empty-db", "swapdb"],
        "sanitize-dump-payload" => &["no", "yes", "clients"],
        "propagation-error-behavior" => &["ignore", "panic", "panic-on-replicas"],
        "cluster-preferred-endpoint-type" => &["ip", "hostname", "unknown-endpoint"],
        "supervised" => &["no", "upstart", "systemd", "auto"],
        "oom-score-adj" => &["no", "yes", "relative", "absolute"],
        "acl-pubsub-default" => &["resetchannels", "allchannels"],
        "syslog-facility" => &[
            "user", "local0", "local1", "local2", "local3", "local4", "local5", "local6", "local7",
        ],
        // yes/no/local tri-state guards — not plain booleans.
        "enable-protected-configs" | "enable-debug-command" | "enable-module-command" => &["no", "yes", "local"],
        _ => return None,
    };
    Some(opts)
}

fn config_kind(key: &str, value: &str) -> ConfigKind {
    if let Some(opts) = config_enum_options(key) {
        return ConfigKind::Enum(opts);
    }
    match value {
        "yes" | "no" => ConfigKind::Bool,
        _ if !value.is_empty() && value.parse::<f64>().is_ok() => ConfigKind::Number,
        _ => ConfigKind::Text,
    }
}

pub struct ZedisConfigEditor {
    server_state: Entity<ZedisServerState>,
    configs: Vec<(SharedString, SharedString)>,
    filter_state: Entity<InputState>,
    filter: String,
    editing_key: Option<SharedString>,
    edit_state: Entity<InputState>,
    /// Numeric-only input used when editing a value that parses as a number.
    number_state: Entity<InputState>,
    /// Checkbox state used when editing a `yes`/`no` value.
    editing_bool: bool,
    /// Current selection used when editing a known enum value (dropdown).
    editing_enum: SharedString,
    loading: bool,
    /// Set when the `CONFIG GET *` load fails, so the body shows the error
    /// instead of a misleading empty "no data" panel.
    error: Option<SharedString>,
    pending_notification: Option<Notification>,
    /// Active cross-server config comparison (`None` = normal editor view).
    diff: Option<ConfigDiff>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisConfigEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_state = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by key..."));
        let edit_state = cx.new(|cx| InputState::new(window, cx));
        // Numeric values use a NumberInput (with ↑/↓ steppers) for a numeric
        // feel, but the input is NOT pattern-restricted — many "numeric-looking"
        // configs legitimately take spaces / units / multiple segments (e.g.
        // `save 3600 1 300 100`, `maxmemory 100mb`), so hard-blocking keystrokes
        // would trap the user. The steppers safely no-op on non-numeric values.
        let number_state = cx.new(|cx| InputState::new(window, cx));
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
            number_state,
            editing_bool: false,
            editing_enum: SharedString::default(),
            loading: false,
            error: None,
            pending_notification: None,
            diff: None,
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
                    Ok(configs) => {
                        this.configs = configs;
                        this.error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "load configs failed");
                        this.error = Some(e.to_string().into());
                    }
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

    /// Open the server picker (copy dialog reused as a server / db picker) and,
    /// on OK, compare this server's `CONFIG GET *` against the chosen one.
    fn open_compare_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let source_id = server_state.server_id().to_string();
        let source_db = server_state.db();
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.into(), source_db, false, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        ZedisDialog::new(i18n_config_editor(cx, "compare_title"))
            .w(px(460.))
            .ok_text(i18n_config_editor(cx, "compare_title"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_config_editor(cx, "compare_title"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_compare(target_id, target_db, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// Fetch `CONFIG GET *` from the target and store the differing parameters.
    fn run_compare(&mut self, target_id: SharedString, target_db: usize, cx: &mut Context<Self>) {
        let local = self.configs.clone();
        let local_id = self.server_state.read(cx).server_id().to_string();
        let local_label: SharedString = get_server(&local_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| local_id.clone().into());
        let other_label: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&target_id, target_db).await?;
                let map: HashMap<String, String> = cmd("CONFIG").arg("GET").arg("*").query_async(&mut conn).await?;
                Ok::<HashMap<String, String>, Error>(map)
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| match result {
                Ok(other_map) => {
                    let mut local_map: HashMap<String, String> =
                        local.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                    let mut keys: BTreeSet<String> = local_map.keys().cloned().collect();
                    keys.extend(other_map.keys().cloned());
                    let mut rows: Vec<(SharedString, SharedString, SharedString)> = Vec::new();
                    for key in keys {
                        let lv = local_map.remove(&key).unwrap_or_default();
                        let ov = other_map.get(&key).cloned().unwrap_or_default();
                        if lv != ov {
                            rows.push((key.into(), lv.into(), ov.into()));
                        }
                    }
                    this.diff = Some(ConfigDiff {
                        local_label,
                        other_label,
                        rows,
                    });
                    cx.notify();
                }
                Err(e) => {
                    let msg: SharedString = format!("{}: {e}", i18n_config_editor(cx, "compare_failed")).into();
                    this.pending_notification = Some(Notification::error(msg));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn close_diff(&mut self, cx: &mut Context<Self>) {
        self.diff = None;
        cx.notify();
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
        let number_state = self.number_state.clone();
        let editing_bool = self.editing_bool;
        let editing_enum = self.editing_enum.clone();
        let self_handle = cx.entity().downgrade();

        let stripe_bg = cx.theme().table_even;
        let rows = filtered.into_iter().enumerate().map(|(row_ix, (key, value))| {
            let is_editing = editing_key.as_ref() == Some(&key);
            let is_stripe = row_ix % 2 != 0;

            if is_editing {
                let save_key = key.clone();
                let kind = config_kind(&key, &value);
                // Pick the editor control by inferred value type.
                let editor = match kind {
                    ConfigKind::Enum(options) => {
                        // Always include the server's current value, so an option
                        // from a newer Redis (not in our catalog) is selectable.
                        let mut opts: Vec<SharedString> = options.iter().map(|s| SharedString::from(*s)).collect();
                        if !opts.iter().any(|o| o.as_ref() == value.as_ref()) {
                            opts.insert(0, value.clone());
                        }
                        let handle = self_handle.clone();
                        DropdownButton::new("config-enum-edit")
                            .small()
                            .button(
                                Button::new("config-enum-current")
                                    .outline()
                                    .small()
                                    .label(editing_enum.clone()),
                            )
                            .dropdown_menu(move |menu, _w, _cx| {
                                let mut menu = menu;
                                for opt in opts.clone() {
                                    let handle = handle.clone();
                                    let val = opt.clone();
                                    menu = menu.item(PopupMenuItem::new(opt.clone()).on_click(move |_, _w, cx| {
                                        if let Some(this) = handle.upgrade() {
                                            this.update(cx, |this, cx| {
                                                this.editing_enum = val.clone();
                                                cx.notify();
                                            });
                                        }
                                    }));
                                }
                                menu
                            })
                            .into_any_element()
                    }
                    ConfigKind::Bool => Checkbox::new("config-bool-edit")
                        .checked(editing_bool)
                        .label(if editing_bool { "yes" } else { "no" })
                        .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                            this.editing_bool = *checked;
                            cx.notify();
                        }))
                        .into_any_element(),
                    ConfigKind::Number => NumberInput::new(&number_state).small().into_any_element(),
                    ConfigKind::Text => Input::new(&edit_state)
                        .small()
                        .font_family(font_family.clone())
                        .appearance(true)
                        .into_any_element(),
                };
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .min_h(px(34.))
                    .items_center()
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
                    .child(div().flex_1().child(editor))
                    .child(
                        Button::new("config-save")
                            .small()
                            .primary()
                            .label(i18n_common(cx, "save"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let v: SharedString = match kind {
                                    ConfigKind::Enum(_) => this.editing_enum.clone(),
                                    ConfigKind::Bool => if this.editing_bool { "yes" } else { "no" }.into(),
                                    ConfigKind::Number => this.number_state.read(cx).value(),
                                    ConfigKind::Text => this.edit_state.read(cx).value(),
                                };
                                let key = save_key.clone();
                                let server_id = this.server_state.read(cx).server_id().to_string();
                                let line = format!("CONFIG SET {} {}", key, v);
                                let entity = cx.entity().downgrade();
                                let value_for_run = v.clone();
                                let key_for_run = key.clone();
                                let run = move |_: &mut Window, cx: &mut App| {
                                    let Some(this) = entity.upgrade() else { return };
                                    let key = key_for_run.clone();
                                    let value = value_for_run.clone();
                                    this.update(cx, |this, cx| this.save_config(key, value, cx));
                                };
                                if let Ok(server) = get_server(&server_id) {
                                    confirm_dangerous_command(
                                        &server,
                                        &DangerKind::ConfigSet,
                                        Some(&line),
                                        window,
                                        cx,
                                        run,
                                    );
                                } else {
                                    this.save_config(key, v, cx);
                                }
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
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .min_h(px(34.))
                    .items_center()
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
                                // Seed the control matching the value's inferred type.
                                match config_kind(&edit_key, &edit_value) {
                                    ConfigKind::Enum(_) => this.editing_enum = edit_value.clone(),
                                    ConfigKind::Bool => this.editing_bool = edit_value.as_ref() == "yes",
                                    ConfigKind::Number => this.number_state.update(cx, |state, cx| {
                                        state.set_value(edit_value.clone(), window, cx);
                                    }),
                                    ConfigKind::Text => this.edit_state.update(cx, |state, cx| {
                                        state.set_value(edit_value.clone(), window, cx);
                                    }),
                                }
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
                    .items_center()
                    .child(
                        Button::new("config-back")
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
                    .child(
                        Label::new(i18n_config_editor(cx, "title"))
                            .text_sm()
                            .font_family(font_family.clone()),
                    )
                    // Inline loading indicator in the header — more noticeable
                    // than a centered body spinner while `CONFIG GET *` is slow.
                    .when(self.loading, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1p5()
                                .child(Spinner::new().with_size(px(14.)).color(cx.theme().muted_foreground))
                                .child(
                                    Label::new(i18n_common(cx, "loading"))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
                    })
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
                    .child(
                        Button::new("config-compare")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::GitCompareArrows))
                            .tooltip(i18n_config_editor(cx, "compare_tooltip"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_compare_dialog(window, cx))),
                    )
                    .when(self.diff.is_some(), |this| {
                        this.child(
                            Button::new("config-exit-diff")
                                .small()
                                .ghost()
                                .label(i18n_config_editor(cx, "exit_diff"))
                                .on_click(cx.listener(|this, _, _, cx| this.close_diff(cx))),
                        )
                    })
                    .child(div().w(px(200.0)).child(Input::new(&self.filter_state).small())),
            )
            .child(if let Some(diff) = &self.diff {
                let border = cx.theme().border;
                let muted = cx.theme().muted_foreground;
                let fg = cx.theme().foreground;
                let filter = self.filter.to_lowercase();
                let filtered_diff: Vec<&(SharedString, SharedString, SharedString)> = diff
                    .rows
                    .iter()
                    .filter(|(k, _, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
                    .collect();
                if filtered_diff.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Label::new(i18n_config_editor(cx, "no_diff")).text_color(muted))
                        .into_any_element()
                } else {
                    let header_row = h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .w(px(280.0))
                                .flex_none()
                                .child(Label::new(i18n_config_editor(cx, "param")).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.local_label.clone()).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.other_label.clone()).text_xs().text_color(muted)),
                        );
                    let diff_rows = filtered_diff.into_iter().enumerate().map(move |(i, (key, lv, ov))| {
                        let is_stripe = i % 2 != 0;
                        h_flex()
                            .w_full()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .border_b_1()
                            .border_color(border)
                            .when(is_stripe, |this| this.bg(stripe_bg))
                            .child(
                                div()
                                    .w(px(280.0))
                                    .flex_none()
                                    .child(Label::new(key.clone()).text_sm().text_color(fg)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(lv.clone()).text_sm().text_ellipsis().text_color(muted)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(ov.clone()).text_sm().text_ellipsis().text_color(fg)),
                            )
                    });
                    div()
                        .id("config-diff-body")
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(header_row)
                        .children(diff_rows)
                        .into_any_element()
                }
            } else if self.configs.is_empty() && !self.loading {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(err) = &self.error {
                        Label::new(err.clone()).text_color(cx.theme().red)
                    } else {
                        Label::new(i18n_config_editor(cx, "no_data")).text_color(cx.theme().muted_foreground)
                    })
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

#[cfg(test)]
mod tests {
    use super::{ConfigKind, config_enum_options, config_kind};

    #[test]
    fn config_kind_infers_from_value() {
        // yes/no → checkbox.
        assert!(matches!(config_kind("appendonly", "yes"), ConfigKind::Bool));
        assert!(matches!(config_kind("appendonly", "no"), ConfigKind::Bool));
        // Parseable number → numeric input.
        assert!(matches!(config_kind("maxmemory", "0"), ConfigKind::Number));
        assert!(matches!(config_kind("databases", "16"), ConfigKind::Number));
        // Paths / multi-segment / empty → plain text.
        assert!(matches!(config_kind("dir", "./"), ConfigKind::Text));
        assert!(matches!(config_kind("save", "3600 1 300 100"), ConfigKind::Text));
        assert!(matches!(config_kind("requirepass", ""), ConfigKind::Text));
    }

    #[test]
    fn config_kind_catalog_wins_over_value() {
        // Known enum params resolve to Enum even when their value would
        // otherwise infer as Text (or a number).
        assert!(matches!(
            config_kind("maxmemory-policy", "noeviction"),
            ConfigKind::Enum(_)
        ));
        assert!(matches!(config_kind("loglevel", "notice"), ConfigKind::Enum(_)));
    }

    #[test]
    fn config_enum_options_lookup() {
        // Unknown keys fall through (→ value inference).
        assert!(config_enum_options("definitely-not-a-config").is_none());

        let fsync = config_enum_options("appendfsync").expect("appendfsync is a known enum");
        assert_eq!(fsync, &["everysec", "always", "no"]);

        // loglevel includes the easy-to-miss `nothing`.
        let loglevel = config_enum_options("loglevel").expect("loglevel is a known enum");
        assert!(loglevel.contains(&"nothing"));

        // tls-auth-clients was added after the initial catalog.
        let tls = config_enum_options("tls-auth-clients").expect("tls-auth-clients is a known enum");
        assert_eq!(tls, &["no", "yes", "optional"]);

        // All 8 standard eviction policies.
        assert_eq!(config_enum_options("maxmemory-policy").map(|o| o.len()), Some(8));
    }
}
