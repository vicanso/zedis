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

use crate::views::config_doc::ConfigDocMap;
use crate::{
    assets::CustomIconName,
    connection::{Capability, DangerKind, get_connection_manager, get_server, get_servers},
    error::Error,
    helpers::{ConfigEditAction, card_background, get_mono_font_family},
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common,
        i18n_config_editor,
    },
    views::{ZedisCopyKeyDialog, config_doc::load_config_docs, confirm_dangerous_command},
};
use gpui::{App, Entity, FocusHandle, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, NumberInput},
    label::Label,
    notification::Notification,
    spinner::Spinner,
    v_flex,
};
use redis::cmd;
use std::collections::{BTreeSet, HashMap};
use tracing::error;
use zedis_ui::{ZedisDialog, ZedisSelect, ZedisSelectEvent, help_popover};

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

/// A section the config parameters are grouped into. `label` is a fixed
/// technical string (a prefix pattern, shown mono); `desc_key` is an i18n key
/// (under `config_editor`) for the human description. A parameter joins the
/// first group any of its `prefixes` matches (`starts_with`); anything left
/// over falls into a synthetic "others" section.
struct ConfigGroup {
    label: &'static str,
    desc_key: &'static str,
    prefixes: &'static [&'static str],
}

/// Groups in display order. Prefixes are chosen specific enough to avoid
/// cross-group collisions (e.g. `set-max` not `set-`, so `set-proc-title`
/// doesn't land in data types).
/// Order: the parameters people most often view/tune first (memory,
/// persistence, network, security, replication, logging, latency), then the
/// situational/advanced groups (cluster, TLS, active defrag, data-type
/// tuning, scripting). The "others" bucket always renders after all of these.
const CONFIG_GROUPS: &[ConfigGroup] = &[
    ConfigGroup {
        label: "maxmemory-*",
        desc_key: "group_memory",
        prefixes: &["maxmemory"],
    },
    ConfigGroup {
        label: "aof-* / appendonly",
        desc_key: "group_aof",
        prefixes: &[
            "appendonly",
            "appendfsync",
            "appendfilename",
            "appenddirname",
            "aof",
            "auto-aof-rewrite",
            "no-appendfsync-on-rewrite",
        ],
    },
    ConfigGroup {
        label: "rdb-* / snapshot",
        desc_key: "group_rdb",
        prefixes: &[
            "save",
            "rdb",
            "dbfilename",
            "dir",
            "stop-writes-on-bgsave-error",
            "sanitize-dump-payload",
        ],
    },
    ConfigGroup {
        label: "client-* / network",
        desc_key: "group_network",
        prefixes: &[
            "maxclients",
            "timeout",
            "tcp",
            "client",
            "bind",
            "port",
            "unixsocket",
            "socket",
            "protected-mode",
        ],
    },
    ConfigGroup {
        label: "acl-* / auth",
        desc_key: "group_security",
        prefixes: &[
            "requirepass",
            "acl",
            "enable-protected",
            "enable-debug",
            "enable-module",
            "rename-command",
        ],
    },
    ConfigGroup {
        label: "repl-* / replica-*",
        desc_key: "group_replication",
        prefixes: &[
            "repl",
            "slave",
            "min-replicas",
            "min-slaves",
            "master",
            "propagation-error",
        ],
    },
    ConfigGroup {
        label: "log-* / syslog",
        desc_key: "group_logging",
        prefixes: &["logfile", "loglevel", "syslog", "crash-"],
    },
    ConfigGroup {
        label: "latency / slowlog",
        desc_key: "group_observability",
        prefixes: &["latency", "slowlog"],
    },
    ConfigGroup {
        label: "cluster-*",
        desc_key: "group_cluster",
        prefixes: &["cluster"],
    },
    ConfigGroup {
        label: "tls-*",
        desc_key: "group_tls",
        prefixes: &["tls"],
    },
    ConfigGroup {
        label: "active-defrag-*",
        desc_key: "group_defrag",
        prefixes: &["activedefrag", "active-defrag"],
    },
    ConfigGroup {
        label: "hash / list / set / zset",
        desc_key: "group_datatypes",
        prefixes: &[
            "hash-max",
            "list-max",
            "list-compress",
            "set-max",
            "zset-max",
            "stream-node-max",
            "hll-sparse",
            "activerehashing",
            "proto-max-bulk-len",
        ],
    },
    ConfigGroup {
        label: "lua / functions",
        desc_key: "group_scripting",
        prefixes: &["lua", "functions", "busy-reply-threshold"],
    },
];

/// The group index a config key belongs to, or `None` for the "others" bucket.
fn config_group_index(key: &str) -> Option<usize> {
    CONFIG_GROUPS
        .iter()
        .position(|g| g.prefixes.iter().any(|p| key.starts_with(p)))
}

/// Reference description for a well-known config parameter, shown in the
/// scrollable help popover behind the card's `?`. `(en, cn)` pairs: the
/// English is extracted from the bundled official redis.conf (reflowed to
/// Markdown); the Chinese is a faithful translation of it. Returns Chinese
/// when `zh` is set, English otherwise. Parameters without an entry show
/// no `?`.
pub struct ZedisConfigEditor {
    server_state: Entity<ZedisServerState>,
    /// Tracked on the root so a config edit can grab focus (see the edit
    /// handler), letting the `Esc` capture handler cancel the edit before the
    /// keystroke bubbles up to the global "back" binding.
    focus_handle: FocusHandle,
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
    /// Searchable-style dropdown (`ZedisSelect`, matching the settings UI) and
    /// its change subscription for the active enum edit; `None` otherwise. A
    /// stateful view entity, so it's created on entering an enum edit rather
    /// than inline per render.
    enum_select: Option<(Entity<ZedisSelect>, Subscription)>,
    loading: bool,
    /// True while a cross-server compare fetches the target's `CONFIG GET *`,
    /// so the header shows the same spinner as the initial load (the fetch can
    /// be slow against a remote / cluster target).
    comparing: bool,
    /// Set when the `CONFIG GET *` load fails, so the body shows the error
    /// instead of a misleading empty "no data" panel.
    error: Option<SharedString>,
    pending_notification: Option<Notification>,
    /// Active cross-server config comparison (`None` = normal editor view).
    diff: Option<ConfigDiff>,
    /// redis.conf help for the current UI language. Loaded once when this
    /// view is created (and reloaded only if the locale flips while open);
    /// not re-fetched on scroll/repaint.
    config_docs: ConfigDocMap,
    /// Locale flag the `config_docs` map was loaded for (`true` = zh).
    config_docs_zh: bool,
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
                this.enum_select = None;
                this.configs.clear();
                this.load_configs(cx);
            }
        }));

        let config_docs_zh = cx.global::<ZedisGlobalStore>().read(cx).locale().starts_with("zh");
        let mut this = Self {
            server_state,
            focus_handle: cx.focus_handle(),
            configs: Vec::new(),
            filter_state,
            filter: String::new(),
            editing_key: None,
            edit_state,
            number_state,
            editing_bool: false,
            editing_enum: SharedString::default(),
            enum_select: None,
            loading: false,
            comparing: false,
            error: None,
            pending_notification: None,
            diff: None,
            config_docs: load_config_docs(config_docs_zh),
            config_docs_zh,
            _subscriptions: subscriptions,
        };
        this.load_configs(cx);
        this
    }

    /// Reload help JSON only when the UI language no longer matches the map.
    fn refresh_docs_if_locale_changed(&mut self, cx: &App) {
        let zh = cx.global::<ZedisGlobalStore>().read(cx).locale().starts_with("zh");
        if zh != self.config_docs_zh {
            self.config_docs = load_config_docs(zh);
            self.config_docs_zh = zh;
        }
    }

    fn load_configs(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        // No active connection yet — e.g. a restored `config` route recreated
        // this view before `ServerSelected` wired up the server. Querying with
        // an empty id would hit `get_server("")` → "Redis config not found".
        // The ServerSelected subscription reloads once the connection is ready.
        if server_id.is_empty() {
            return;
        }
        self.loading = true;
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
        // Defense in depth — the pencil is hidden without ConfigWrite.
        if !self.server_state.read(cx).can(Capability::ConfigWrite) {
            return;
        }
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
                this.enum_select = None;
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
        // Show the header spinner immediately; the target fetch below is async.
        self.comparing = true;
        cx.notify();
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
            let _ = handle.update(cx, |this, cx| {
                this.comparing = false;
                match result {
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
                }
            });
        })
        .detach();
    }

    fn close_diff(&mut self, cx: &mut Context<Self>) {
        self.diff = None;
        cx.notify();
    }

    /// Build the enum-edit dropdown (`ZedisSelect`, matching the settings UI)
    /// for `value` among `options`. The server's current value is always
    /// included so an option from a newer Redis isn't lost. The subscription
    /// mirrors the selection into `editing_enum` for the eventual save.
    fn build_enum_select(
        &mut self,
        options: &[&str],
        value: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut opts: Vec<SharedString> = options.iter().map(|s| SharedString::from(*s)).collect();
        if !opts.iter().any(|o| o.as_ref() == value.as_ref()) {
            opts.insert(0, value.clone());
        }
        let selected = opts.iter().position(|o| o.as_ref() == value.as_ref());
        let items: Vec<String> = opts.iter().map(|o| o.to_string()).collect();
        self.editing_enum = value.clone();
        let select = cx.new(|cx| ZedisSelect::new(items, selected, window, cx));
        let opts_for_sub = opts;
        let subscription = cx.subscribe(&select, move |this, _sel, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(index) = event;
            if let Some(v) = opts_for_sub.get(*index) {
                this.editing_enum = v.clone();
                cx.notify();
            }
        });
        self.enum_select = Some((select, subscription));
    }

    /// One config parameter as a card: key + edit pencil + value in display
    /// mode; a highlighted card with the editor control + Save/Cancel while
    /// editing. Theme colors are copied to `Copy` locals before any
    /// `cx.listener` closure (borrowing `cx` inside one won't compile).
    fn render_card(
        &self,
        key: SharedString,
        value: SharedString,
        font_family: &SharedString,
        docs: &ConfigDocMap,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        // Shared card surface, matching the server cards (Home).
        let card_bg = card_background(cx);
        let radius = cx.theme().radius;
        let kind = config_kind(&key, &value);
        // CONFIG SET is a server write — hide the pencil without
        // Capability::ConfigWrite (read-only keeps values + help visible).
        let can_write = self.server_state.read(cx).can(Capability::ConfigWrite);

        if self.editing_key.as_ref() == Some(&key) {
            let save_key = key.clone();
            let editor = match kind {
                ConfigKind::Enum(_) => self
                    .enum_select
                    .as_ref()
                    .map(|(s, _)| s.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                ConfigKind::Bool => Checkbox::new("config-bool-edit")
                    .checked(self.editing_bool)
                    .label(if self.editing_bool { "yes" } else { "no" })
                    .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                        this.editing_bool = *checked;
                        cx.notify();
                    }))
                    .into_any_element(),
                ConfigKind::Number => NumberInput::new(&self.number_state).w_full().into_any_element(),
                ConfigKind::Text => Input::new(&self.edit_state)
                    .w_full()
                    .font_family(font_family.clone())
                    .appearance(true)
                    .into_any_element(),
            };
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(primary)
                .rounded(radius)
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div().min_w_0().overflow_hidden().child(
                                Label::new(key.clone())
                                    .text_sm()
                                    .text_color(muted)
                                    .font_family(font_family.clone()),
                            ),
                        )
                        .child(
                            Label::new(i18n_config_editor(cx, "editing"))
                                .text_xs()
                                .text_color(primary),
                        ),
                )
                .child(editor)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("config-save")
                                .small()
                                .primary()
                                .flex_1()
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
                                .flex_1()
                                .label(i18n_common(cx, "cancel"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing_key = None;
                                    this.enum_select = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element()
        } else {
            let edit_key = key.clone();
            let edit_value = value.clone();
            // Official redis.conf description for the `?` help popover;
            // `None` → no `?` shown. Map is loaded once per paint by the caller.
            let doc = docs.get(key.as_ref()).cloned();
            let value_el = if matches!(kind, ConfigKind::Bool) {
                let on = value.as_ref() == "yes";
                h_flex()
                    .items_center()
                    .gap_1p5()
                    .child(div().size_2().rounded_full().bg(if on { green } else { muted }))
                    .child(
                        Label::new(value.clone())
                            .text_sm()
                            .text_color(fg)
                            .font_family(font_family.clone()),
                    )
                    .into_any_element()
            } else {
                Label::new(value.clone())
                    .text_sm()
                    .text_ellipsis()
                    .text_color(fg)
                    .font_family(font_family.clone())
                    .into_any_element()
            };
            div()
                .id(SharedString::from(format!("config-card-{key}")))
                .flex_1()
                .min_w_0()
                .min_h(px(72.))
                .p_3()
                .border_1()
                .border_color(border)
                .rounded(radius)
                .bg(card_bg)
                .hover(|this| this.border_color(primary))
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .child(
                            div().min_w_0().overflow_hidden().child(
                                Label::new(key.clone())
                                    .text_sm()
                                    .text_color(muted)
                                    .font_family(font_family.clone()),
                            ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap_1()
                                // Per-parameter help: a `?` that opens a
                                // scrollable popover with the full official
                                // redis.conf description; shown only when one
                                // exists for this key.
                                .when_some(doc, |this, doc| {
                                    this.child(help_popover(SharedString::from(format!("config-help-{key}")), doc))
                                })
                                .when(can_write, |this| {
                                    this.child(
                                        Button::new(SharedString::from(format!("config-edit-{key}")))
                                            .xsmall()
                                            .ghost()
                                            .icon(Icon::new(CustomIconName::FilePenLine))
                                            .tooltip(i18n_config_editor(cx, "edit_tooltip"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.editing_key = Some(edit_key.clone());
                                                let kind = config_kind(&edit_key, &edit_value);
                                                match kind {
                                                    ConfigKind::Enum(options) => {
                                                        this.build_enum_select(options, &edit_value, window, cx)
                                                    }
                                                    ConfigKind::Bool => {
                                                        this.editing_bool = edit_value.as_ref() == "yes"
                                                    }
                                                    // Focus the input so the user can type at once — and so the
                                                    // Esc-to-cancel capture handler is on the focus path.
                                                    ConfigKind::Number => this.number_state.update(cx, |state, cx| {
                                                        state.set_value(edit_value.clone(), window, cx);
                                                        state.focus(window, cx);
                                                    }),
                                                    ConfigKind::Text => this.edit_state.update(cx, |state, cx| {
                                                        state.set_value(edit_value.clone(), window, cx);
                                                        state.focus(window, cx);
                                                    }),
                                                }
                                                // Bool / enum have no text input to focus; focus the editor root
                                                // so Esc still reaches the capture handler above.
                                                if matches!(kind, ConfigKind::Bool | ConfigKind::Enum(_)) {
                                                    this.focus_handle.focus(window, cx);
                                                }
                                                cx.notify();
                                            })),
                                    )
                                }),
                        ),
                )
                .child(div().mt_2().overflow_hidden().child(value_el))
                .into_any_element()
        }
    }

    /// A titled section: a header (accent bar + mono label + description +
    /// count) over a responsive grid of parameter cards.
    fn render_group(&self, group: ConfigGroupSection<'_>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ConfigGroupSection {
            label,
            desc,
            configs,
            cols,
            font_family,
            docs,
        } = group;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        // The theme's primary, same accent the sidebar and key tree use for their
        // selection bars — so a named theme restyles all three together.
        let accent_bar = cx.theme().primary;

        let mut cards: Vec<gpui::AnyElement> = Vec::with_capacity(configs.len());
        for (k, v) in configs {
            cards.push(self.render_card(k.clone(), v.clone(), font_family, docs, cx));
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().w(px(3.)).h(px(15.)).rounded_sm().bg(accent_bar))
                    .child(
                        Label::new(label)
                            .font_family(font_family.clone())
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(fg),
                    )
                    .child(Label::new(desc).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .border_1()
                            .border_color(border)
                            .child(Label::new(configs.len().to_string()).text_xs().text_color(muted)),
                    ),
            )
            .child(div().grid().grid_cols(cols).items_start().gap_3().children(cards))
            .into_any_element()
    }
}

/// Inputs for [`ZedisConfigEditor::render_group`].
struct ConfigGroupSection<'a> {
    label: SharedString,
    desc: SharedString,
    configs: &'a [(SharedString, SharedString)],
    cols: u16,
    font_family: &'a SharedString,
    docs: &'a ConfigDocMap,
}

impl Render for ZedisConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let font_family: SharedString = get_mono_font_family().into();
        let filter = self.filter.to_lowercase();

        let filtered: Vec<(SharedString, SharedString)> = self
            .configs
            .iter()
            .filter(|(k, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
            .cloned()
            .collect();

        // `stripe_bg` is still used by the cross-server diff view below.
        let stripe_bg = cx.theme().table_even;

        // Group the filtered configs into ordered sections; anything not
        // matching a known group falls into the synthetic "others" section.
        let mut buckets: Vec<Vec<(SharedString, SharedString)>> = vec![Vec::new(); CONFIG_GROUPS.len()];
        let mut others: Vec<(SharedString, SharedString)> = Vec::new();
        for (k, v) in filtered {
            match config_group_index(&k) {
                Some(i) => buckets[i].push((k, v)),
                None => others.push((k, v)),
            }
        }
        // Responsive card-grid column count via the content-width proxy.
        let cols: u16 = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .map(|w| {
                let w = w.as_f32();
                if w > 1200. {
                    4
                } else if w > 900. {
                    3
                } else if w > 600. {
                    2
                } else {
                    1
                }
            })
            .unwrap_or(1);

        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            // While editing, declare the `ConfigEdit` key context so `escape`
            // maps to Cancel — deeper than the workspace's back binding, so it
            // wins; with no edit active there's no context and `escape` returns
            // to the editor as usual. Requires the editor to hold focus (the
            // edit handler focuses the input or this root).
            .when(self.editing_key.is_some(), |this| this.key_context("ConfigEdit"))
            .on_action(cx.listener(|this, _: &ConfigEditAction, _window, cx| {
                this.editing_key = None;
                this.enum_select = None;
                cx.notify();
            }))
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
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(
                        Label::new(i18n_config_editor(cx, "title"))
                            .text_sm()
                            .font_family(font_family.clone()),
                    )
                    // Inline loading indicator in the header — more noticeable
                    // than a centered body spinner while `CONFIG GET *` is slow
                    // (covers both the initial load and a cross-server compare).
                    .when(self.loading || self.comparing, |this| {
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
                                this.enum_select = None;
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
                let hover_bg = cx.theme().table_hover;
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
                            .id(("config-diff-row", i))
                            .w_full()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .border_b_1()
                            .border_color(border)
                            .when(is_stripe, |this| this.bg(stripe_bg))
                            .hover(move |this| this.bg(hover_bg))
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
                // View-scoped map: loaded in `new`, not on every scroll/repaint.
                self.refresh_docs_if_locale_changed(cx);
                let mut sections = v_flex().w_full().gap_6().px_4().py_3();
                for (i, group) in CONFIG_GROUPS.iter().enumerate() {
                    if buckets[i].is_empty() {
                        continue;
                    }
                    sections = sections.child(self.render_group(
                        ConfigGroupSection {
                            label: SharedString::from(group.label),
                            desc: i18n_config_editor(cx, group.desc_key),
                            configs: &buckets[i],
                            cols,
                            font_family: &font_family,
                            docs: &self.config_docs,
                        },
                        cx,
                    ));
                }
                if !others.is_empty() {
                    sections = sections.child(self.render_group(
                        ConfigGroupSection {
                            label: i18n_config_editor(cx, "group_others"),
                            desc: i18n_config_editor(cx, "group_others_desc"),
                            configs: &others,
                            cols,
                            font_family: &font_family,
                            docs: &self.config_docs,
                        },
                        cx,
                    ));
                }
                div()
                    .id("config-editor-body")
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(sections)
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
