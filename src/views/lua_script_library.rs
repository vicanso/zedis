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

//! Locally persisted Lua script library with EVALSHA-first execution.
//!
//! Each saved script has a name, source code, precomputed SHA1, and
//! lifetime hit-rate counters. The Run panel pre-fills KEYS / ARGS
//! from the saved defaults so re-running is one click. EVALSHA hit
//! vs. miss is recorded after every successful run so the user can
//! spot scripts that keep getting flushed out of Redis's cache.
//! Server cache status is probed via `SCRIPT EXISTS` / `SCRIPT LOAD`.

use crate::views::unavailable_chip;
use crate::{
    assets::CustomIconName,
    connection::{
        Capability, ScriptRunOutcome, get_connection_manager, max_keys_index, run_script, script_exists, script_flush,
        script_load,
    },
    db::{LuaScript, LuaScriptExport, LuaScriptManager},
    error::Error,
    helpers::{get_mono_font_family, unix_ts},
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, dialog_button_props,
        escalate_dangerous_body, i18n_common, i18n_lua_scripts,
    },
};
use ahash::AHashMap;
use gpui::{ClipboardItem, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, Input, InputState, TabSize, Textarea, TextareaState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use tracing::info;
use uuid::Uuid;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// SHA prefix length shown in the card header.
const SHA_PREVIEW_CHARS: usize = 12;

struct CodeTemplate {
    id: &'static str,
    label_key: &'static str,
    name: &'static str,
    source: &'static str,
    default_keys: &'static str,
    default_args: &'static str,
}

const TEMPLATES: &[CodeTemplate] = &[
    CodeTemplate {
        id: "get",
        label_key: "template_get",
        name: "get_key",
        source: "-- GET KEYS[1]\nreturn redis.call('GET', KEYS[1])\n",
        default_keys: "mykey",
        default_args: "",
    },
    CodeTemplate {
        id: "set",
        label_key: "template_set",
        name: "set_key",
        source: "-- SET KEYS[1] ARGV[1]\nreturn redis.call('SET', KEYS[1], ARGV[1])\n",
        default_keys: "mykey",
        default_args: "value",
    },
    CodeTemplate {
        id: "incr_ttl",
        label_key: "template_incr_ttl",
        name: "incr_with_ttl",
        source: "-- INCR KEYS[1]; expire if first write (ARGV[1]=ttl seconds)\nlocal n = redis.call('INCR', KEYS[1])\nif n == 1 then\n  redis.call('EXPIRE', KEYS[1], tonumber(ARGV[1]) or 60)\nend\nreturn n\n",
        default_keys: "counter:demo",
        default_args: "3600",
    },
    CodeTemplate {
        id: "hgetall",
        label_key: "template_hgetall",
        name: "hgetall",
        source: "-- HGETALL KEYS[1]\nreturn redis.call('HGETALL', KEYS[1])\n",
        default_keys: "hash:demo",
        default_args: "",
    },
];

/// In-flight script editor. `target_id` is `Some` when editing an
/// existing entry; `None` for a brand-new script.
struct EditForm {
    target_id: Option<String>,
    name: Entity<InputState>,
    code: Entity<EditorState>,
    default_keys: Entity<TextareaState>,
    default_args: Entity<TextareaState>,
}

/// State for the inline Run panel that hangs off each card.
struct RunForm {
    keys: Entity<TextareaState>,
    args: Entity<TextareaState>,
    last: Option<RunResult>,
}

#[derive(Debug, Clone)]
struct RunResult {
    formatted: String,
    was_hit: bool,
    error: bool,
}

pub struct ZedisLuaScriptLibrary {
    server_state: Entity<ZedisServerState>,
    scripts: Vec<(String, LuaScript)>,
    run_forms: AHashMap<String, RunForm>,
    run_expanded: AHashMap<String, bool>,
    code_expanded: AHashMap<String, bool>,
    code_viewers: AHashMap<String, Entity<EditorState>>,
    /// Server-side SCRIPT EXISTS cache, keyed by script id.
    /// `None` = not probed yet; `Some(true/false)` = last probe.
    cache_status: AHashMap<String, bool>,
    filter: Entity<InputState>,
    edit_form: Option<EditForm>,
    error: Option<SharedString>,
    running: Option<String>,
    saving: bool,
    pending_notification: Option<Notification>,
    _run_task: Option<Task<()>>,
    _probe_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisLuaScriptLibrary {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if let ServerEvent::ServerSelected(_) = event {
                this.run_forms.clear();
                this.run_expanded.clear();
                this.code_viewers.clear();
                this.cache_status.clear();
                this.error = None;
                this.probe_cache(cx);
                cx.notify();
            }
        }));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_lua_scripts(cx, "filter_placeholder")));
        let mut this = Self {
            server_state,
            scripts: Vec::new(),
            run_forms: AHashMap::new(),
            run_expanded: AHashMap::new(),
            code_expanded: AHashMap::new(),
            code_viewers: AHashMap::new(),
            cache_status: AHashMap::new(),
            filter,
            edit_form: None,
            error: None,
            running: None,
            saving: false,
            pending_notification: None,
            _run_task: None,
            _probe_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh_list(cx);
        this.probe_cache(cx);
        this
    }

    /// Refresh the in-memory list without dropping code viewers
    /// (callers that change source should invalidate specifically).
    fn refresh_list(&mut self, cx: &mut gpui::Context<Self>) {
        self.scripts = LuaScriptManager::list_with_id();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut gpui::Context<Self>) {
        self.refresh_list(cx);
        self.probe_cache(cx);
    }

    fn toggle_code(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        let entry = self.code_expanded.entry(id).or_insert(false);
        *entry = !*entry;
        cx.notify();
    }

    fn toggle_run(&mut self, id: String, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        let want_open = !*self.run_expanded.entry(id.clone()).or_insert(false);
        self.run_expanded.insert(id.clone(), want_open);
        if want_open && !self.run_forms.contains_key(&id) {
            let script = match LuaScriptManager::get(&id) {
                Ok(s) => s,
                Err(_) => return,
            };
            let keys_default = script.default_keys.join("\n");
            let args_default = script.default_args.join("\n");
            let keys = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder(i18n_lua_scripts(cx, "keys_placeholder"))
                    .default_value(keys_default)
            });
            let args = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder(i18n_lua_scripts(cx, "args_placeholder"))
                    .default_value(args_default)
            });
            self.run_forms.insert(id, RunForm { keys, args, last: None });
        }
        cx.notify();
    }

    fn open_form(&mut self, existing: Option<&(String, LuaScript)>, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let (target_id, name_val, code_val, keys_val, args_val) = match existing {
            Some((id, s)) => (
                Some(id.clone()),
                SharedString::from(s.name.clone()),
                SharedString::from(s.code.clone()),
                SharedString::from(s.default_keys.join("\n")),
                SharedString::from(s.default_args.join("\n")),
            ),
            None => (
                None,
                SharedString::default(),
                SharedString::from(TEMPLATES[0].source),
                SharedString::from(TEMPLATES[0].default_keys),
                SharedString::from(TEMPLATES[0].default_args),
            ),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_lua_scripts(cx, "name_placeholder"))
                .default_value(name_val)
        });
        let code = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("lua")
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .searchable(true)
                .soft_wrap(false)
                .default_value(code_val)
        });
        let default_keys = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(i18n_lua_scripts(cx, "default_keys_placeholder"))
                .default_value(keys_val)
        });
        let default_args = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(i18n_lua_scripts(cx, "default_args_placeholder"))
                .default_value(args_val)
        });
        self.edit_form = Some(EditForm {
            target_id,
            name,
            code,
            default_keys,
            default_args,
        });
        self.error = None;
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut gpui::Context<Self>) {
        self.edit_form = None;
        cx.notify();
    }

    fn apply_template(&mut self, template_id: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(tpl) = TEMPLATES.iter().find(|t| t.id == template_id) else {
            return;
        };
        let Some(form) = self.edit_form.as_ref() else {
            return;
        };
        // Only pre-fill name when creating a new script (or empty name).
        let current_name = form.name.read(cx).value().to_string();
        if form.target_id.is_none() || current_name.trim().is_empty() {
            form.name.update(cx, |s, cx| {
                s.set_value(tpl.name.to_string(), window, cx);
            });
        }
        form.code.update(cx, |s, cx| {
            s.set_value(tpl.source.to_string(), window, cx);
        });
        form.default_keys.update(cx, |s, cx| {
            s.set_value(tpl.default_keys.to_string(), window, cx);
        });
        form.default_args.update(cx, |s, cx| {
            s.set_value(tpl.default_args.to_string(), window, cx);
        });
        cx.notify();
    }

    fn submit_form(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.edit_form.as_ref() else { return };
        let name = form.name.read(cx).value().to_string().trim().to_string();
        let code = form.code.read(cx).value().to_string();
        if name.is_empty() {
            self.error = Some(i18n_lua_scripts(cx, "name_required"));
            cx.notify();
            return;
        }
        if code.trim().is_empty() {
            self.error = Some(i18n_lua_scripts(cx, "code_required"));
            cx.notify();
            return;
        }
        let id = form.target_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
        let default_keys = parse_lines(&form.default_keys.read(cx).value());
        let default_args = parse_lines(&form.default_args.read(cx).value());
        // Default KEYS are optional pre-fills for the Run panel — do not
        // block save when they under-supply KEYS[n] references in code.

        let existing = LuaScriptManager::get(&id).ok();
        let now = unix_ts();
        let created_at = existing.as_ref().map(|s| s.created_at).unwrap_or(now);
        let calls = existing.as_ref().map(|s| s.calls).unwrap_or(0);
        let evalsha_hits = existing.as_ref().map(|s| s.evalsha_hits).unwrap_or(0);
        let sha = redis::Script::new(&code).get_hash().to_string();

        let script = LuaScript {
            name,
            code,
            sha,
            default_keys,
            default_args,
            calls,
            evalsha_hits,
            created_at,
            updated_at: now,
        };
        if let Err(e) = LuaScriptManager::upsert(&id, script) {
            self.error = Some(e.to_string().into());
            cx.notify();
            return;
        }
        self.edit_form = None;
        self.error = None;
        self.code_viewers.remove(&id);
        self.cache_status.remove(&id);
        self.refresh_list(cx);
        self.probe_cache(cx);
    }

    fn confirm_delete(&mut self, id: String, name: SharedString, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let entity = cx.entity().downgrade();
        let title = i18n_lua_scripts(cx, "delete_title");
        let message: SharedString = format!(
            "{}\n\n{}: {}",
            i18n_lua_scripts(cx, "delete_message"),
            i18n_lua_scripts(cx, "delete_script_label"),
            name,
        )
        .into();
        let id_for_task = id.clone();
        ZedisDialog::new_alert(title, message)
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_lua_scripts(cx, "delete_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let id_clone = id_for_task.clone();
                this.update(cx, |this, cx| match LuaScriptManager::delete(&id_clone) {
                    Ok(()) => {
                        info!(id = %id_clone, "lua script deleted");
                        this.run_forms.remove(&id_clone);
                        this.run_expanded.remove(&id_clone);
                        this.code_expanded.remove(&id_clone);
                        this.code_viewers.remove(&id_clone);
                        this.cache_status.remove(&id_clone);
                        this.refresh_list(cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                });
                true
            })
            .open(window, cx);
    }

    fn duplicate(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        let Ok(src) = LuaScriptManager::get(&id) else {
            return;
        };
        let new_id = Uuid::now_v7().to_string();
        let now = unix_ts();
        let copy = LuaScript {
            name: format!("{} (copy)", src.name),
            code: src.code,
            sha: src.sha,
            default_keys: src.default_keys,
            default_args: src.default_args,
            calls: 0,
            evalsha_hits: 0,
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = LuaScriptManager::upsert(&new_id, copy) {
            self.error = Some(e.to_string().into());
            cx.notify();
            return;
        }
        self.refresh_list(cx);
        self.probe_cache(cx);
    }

    fn run(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        if self.running.is_some() {
            return;
        }
        let Some(form) = self.run_forms.get(&id) else { return };
        let script = match LuaScriptManager::get(&id) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e.to_string().into());
                cx.notify();
                return;
            }
        };
        let keys = parse_lines(&form.keys.read(cx).value());
        let args = parse_lines(&form.args.read(cx).value());
        let needed = max_keys_index(&script.code);
        if needed > keys.len() {
            if let Some(form) = self.run_forms.get_mut(&id) {
                form.last = Some(RunResult {
                    formatted: format!(
                        "{}: need {needed} KEYS, got {}",
                        i18n_lua_scripts(cx, "keys_count_mismatch"),
                        keys.len()
                    ),
                    was_hit: false,
                    error: true,
                });
            }
            cx.notify();
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            self.error = Some(i18n_lua_scripts(cx, "no_server").clone());
            cx.notify();
            return;
        }

        let id_for_task = id.clone();
        let code = script.code.clone();
        let sha = script.sha.clone();
        self.running = Some(id.clone());
        self.error = None;
        self._run_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                run_script(&mut conn, &code, &sha, &keys, &args).await
            });
            let result: Result<ScriptRunOutcome> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running = None;
                match result {
                    Ok(outcome) => {
                        if let Err(e) = LuaScriptManager::record_call(&id_for_task, outcome.was_hit) {
                            tracing::warn!(error = %e, "failed to record script call");
                        }
                        // Successful run implies the digest is now cached.
                        this.cache_status.insert(id_for_task.clone(), true);
                        if let Some(form) = this.run_forms.get_mut(&id_for_task) {
                            form.last = Some(RunResult {
                                formatted: outcome.formatted,
                                was_hit: outcome.was_hit,
                                error: false,
                            });
                        }
                        // Refresh counters without wiping code viewers.
                        this.refresh_list(cx);
                    }
                    Err(e) => {
                        if let Some(form) = this.run_forms.get_mut(&id_for_task) {
                            form.last = Some(RunResult {
                                formatted: e.to_string(),
                                was_hit: false,
                                error: true,
                            });
                        }
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn save_run_as_defaults(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.run_forms.get(&id) else { return };
        let keys = parse_lines(&form.keys.read(cx).value());
        let args = parse_lines(&form.args.read(cx).value());
        let Ok(mut script) = LuaScriptManager::get(&id) else {
            return;
        };
        script.default_keys = keys;
        script.default_args = args;
        script.updated_at = unix_ts();
        if let Err(e) = LuaScriptManager::upsert(&id, script) {
            self.error = Some(e.to_string().into());
            cx.notify();
            return;
        }
        self.pending_notification = Some(Notification::info(i18n_lua_scripts(cx, "defaults_saved")));
        self.refresh_list(cx);
    }

    fn warm_script(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        let Ok(script) = LuaScriptManager::get(&id) else {
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            self.error = Some(i18n_lua_scripts(cx, "no_server").clone());
            cx.notify();
            return;
        }
        let code = script.code.clone();
        let expected_sha = script.sha.clone();
        let id_for_task = id.clone();
        self._probe_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let returned = script_load(&mut conn, &code).await?;
                Ok::<_, Error>((returned, expected_sha))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok((returned, expected)) => {
                        if returned != expected {
                            this.error = Some(format!("SHA mismatch: client={expected} server={returned}").into());
                        } else {
                            this.cache_status.insert(id_for_task, true);
                            this.pending_notification = Some(Notification::info(i18n_lua_scripts(cx, "warm_ok")));
                        }
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn probe_cache(&mut self, cx: &mut gpui::Context<Self>) {
        if self.scripts.is_empty() {
            self.cache_status.clear();
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let pairs: Vec<(String, String)> = self.scripts.iter().map(|(id, s)| (id.clone(), s.sha.clone())).collect();
        self._probe_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let shas: Vec<String> = pairs.iter().map(|(_, s)| s.clone()).collect();
                let flags = script_exists(&mut conn, &shas).await?;
                let mut map = AHashMap::new();
                for ((id, _), ok) in pairs.into_iter().zip(flags) {
                    map.insert(id, ok);
                }
                Ok::<_, Error>(map)
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                if let Ok(map) = result {
                    this.cache_status = map;
                    cx.notify();
                }
            });
        }));
    }

    fn confirm_flush(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = i18n_lua_scripts(cx, "flush_title");
        let message = i18n_lua_scripts(cx, "flush_message");
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, message))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_lua_scripts(cx, "flush_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let server_id_inner = server_id.clone();
                this.update(cx, |this, cx| {
                    this.error = None;
                    this._probe_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let mut conn = get_connection_manager().get_connection(&server_id_inner, db).await?;
                            script_flush(&mut conn, false).await
                        });
                        let result: Result<()> = task.await.map_err(Into::into);
                        let _ = handle.update(cx, |this, cx| match result {
                            Ok(()) => {
                                info!("SCRIPT FLUSH succeeded");
                                for flag in this.cache_status.values_mut() {
                                    *flag = false;
                                }
                                this.pending_notification = Some(Notification::info(i18n_lua_scripts(cx, "flush_ok")));
                                cx.notify();
                            }
                            Err(e) => {
                                this.error = Some(e.to_string().into());
                                cx.notify();
                            }
                        });
                    }));
                });
                true
            })
            .open(window, cx);
    }

    fn export_to_clipboard(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let items = LuaScriptManager::export_all();
        match serde_json::to_string_pretty(&items) {
            Ok(json) => {
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                self.pending_notification = Some(Notification::info(i18n_lua_scripts(cx, "export_ok")));
            }
            Err(e) => {
                self.error = Some(e.to_string().into());
            }
        }
        cx.notify();
    }

    fn import_from_clipboard(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            self.error = Some(i18n_lua_scripts(cx, "import_empty"));
            cx.notify();
            return;
        };
        let Some(text) = item.text() else {
            self.error = Some(i18n_lua_scripts(cx, "import_empty"));
            cx.notify();
            return;
        };
        let parsed: std::result::Result<Vec<LuaScriptExport>, _> = serde_json::from_str(text.trim());
        let items = match parsed {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => {
                self.error = Some(i18n_lua_scripts(cx, "import_empty"));
                cx.notify();
                return;
            }
            Err(_) => {
                self.error = Some(i18n_lua_scripts(cx, "import_bad"));
                cx.notify();
                return;
            }
        };
        let entity = cx.entity().downgrade();
        let title = i18n_lua_scripts(cx, "import_title");
        let message: SharedString = format!(
            "{}\n\n{}: {}",
            i18n_lua_scripts(cx, "import_message"),
            i18n_lua_scripts(cx, "import_count_label"),
            items.len(),
        )
        .into();
        ZedisDialog::new_alert(title, message)
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_lua_scripts(cx, "import_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                this.update(cx, |this, cx| {
                    let now = unix_ts();
                    let mut ok = 0usize;
                    for item in &items {
                        if item.name.trim().is_empty() || item.code.trim().is_empty() {
                            continue;
                        }
                        let id = Uuid::now_v7().to_string();
                        let sha = redis::Script::new(&item.code).get_hash().to_string();
                        let script = LuaScript {
                            name: item.name.clone(),
                            code: item.code.clone(),
                            sha,
                            default_keys: item.default_keys.clone(),
                            default_args: item.default_args.clone(),
                            calls: 0,
                            evalsha_hits: 0,
                            created_at: now,
                            updated_at: now,
                        };
                        if LuaScriptManager::upsert(&id, script).is_ok() {
                            ok += 1;
                        }
                    }
                    this.pending_notification = Some(Notification::info(format!(
                        "{}: {ok}",
                        i18n_lua_scripts(cx, "import_ok")
                    )));
                    this.refresh_list(cx);
                    this.probe_cache(cx);
                });
                true
            })
            .open(window, cx);
    }

    fn filtered_scripts(&self, cx: &gpui::Context<Self>) -> Vec<(String, LuaScript)> {
        let q = self.filter.read(cx).value().to_string();
        let q = q.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.scripts.clone();
        }
        self.scripts
            .iter()
            .filter(|(_, s)| s.name.to_ascii_lowercase().contains(&q) || s.sha.to_ascii_lowercase().starts_with(&q))
            .cloned()
            .collect()
    }
}

fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn format_hit_rate(calls: u64, hits: u64) -> String {
    if calls == 0 {
        "—".to_string()
    } else {
        let pct = (hits as f64 * 100.0) / calls as f64;
        format!("{pct:.0}%")
    }
}

fn id_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

impl gpui::Render for ZedisLuaScriptLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let muted = cx.theme().muted_foreground;
        let header = self.render_header(cx).into_any_element();

        if self.edit_form.is_some() {
            let form_panel = self.render_form_panel(cx).into_any_element();
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(form_panel)
                .into_any_element();
        }

        let filtered = self.filtered_scripts(cx);
        let body: gpui::AnyElement = if self.scripts.is_empty() {
            v_flex()
                .items_center()
                .justify_center()
                .size_full()
                .gap_3()
                .child(Label::new(i18n_lua_scripts(cx, "empty")).text_color(muted))
                .child(
                    Button::new("lua-empty-new")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(i18n_lua_scripts(cx, "new_script"))
                        .on_click(cx.listener(|this, _, w, cx| this.open_form(None, w, cx))),
                )
                .into_any_element()
        } else if filtered.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_lua_scripts(cx, "filter_empty")).text_color(muted))
                .into_any_element()
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(filtered.len());
            for (id, script) in filtered {
                rows.push(self.render_card(id, script, window, cx).into_any_element());
            }
            v_flex().gap_2().p_3().w_full().children(rows).into_any_element()
        };

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        let filter_bar = self.render_filter_bar(cx).into_any_element();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(header)
            .child(filter_bar)
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(body))
            .into_any_element()
    }
}

impl ZedisLuaScriptLibrary {
    fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        let count = self.scripts.len();
        let count_label = if count == 0 {
            SharedString::default()
        } else {
            SharedString::from(format!("({count})"))
        };
        h_flex()
            .items_center()
            .justify_between()
            .px_4()
            .h(px(40.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("lua-back")
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
                    .child(Icon::new(IconName::SquareTerminal))
                    // The library itself is local; only running needs EVAL.
                    .when_some(
                        self.server_state.read(cx).blocked_by(Capability::EvalScript),
                        |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                    )
                    .child(Label::new(i18n_lua_scripts(cx, "title")).text_color(cx.theme().foreground))
                    .child(Label::new(count_label).text_color(muted).text_sm()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("lua-export")
                            .ghost()
                            .small()
                            .label(i18n_lua_scripts(cx, "export"))
                            .tooltip(i18n_lua_scripts(cx, "export_tooltip"))
                            .disabled(self.scripts.is_empty())
                            .on_click(cx.listener(|this, _, w, cx| this.export_to_clipboard(w, cx))),
                    )
                    .child(
                        Button::new("lua-import")
                            .ghost()
                            .small()
                            .label(i18n_lua_scripts(cx, "import"))
                            .tooltip(i18n_lua_scripts(cx, "import_tooltip"))
                            .on_click(cx.listener(|this, _, w, cx| this.import_from_clipboard(w, cx))),
                    )
                    .when(can_run, |this| {
                        this.child(
                            Button::new("lua-flush")
                                .ghost()
                                .small()
                                .label(i18n_lua_scripts(cx, "flush"))
                                .tooltip(i18n_lua_scripts(cx, "flush_tooltip"))
                                .on_click(cx.listener(|this, _, w, cx| this.confirm_flush(w, cx))),
                        )
                    })
                    .child(
                        Button::new("lua-new")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label(i18n_lua_scripts(cx, "new_script"))
                            .on_click(cx.listener(|this, _, w, cx| this.open_form(None, w, cx))),
                    )
                    .child(
                        Button::new("lua-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_lua_scripts(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _w, cx| this.reload(cx))),
                    ),
            )
    }

    fn render_filter_bar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if self.scripts.is_empty() {
            return div().into_any_element();
        }
        h_flex()
            .items_center()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(280.))
                    .child(Input::new(&self.filter).appearance(true).small()),
            )
            .into_any_element()
    }

    fn render_card(
        &mut self,
        id: String,
        script: LuaScript,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let code_expanded = *self.code_expanded.get(&id).unwrap_or(&false);
        let run_expanded = *self.run_expanded.get(&id).unwrap_or(&false);
        let is_running = self.running.as_ref() == Some(&id);
        let card_hash = id_hash(&id);
        let cached = self.cache_status.get(&id).copied();

        let sha_full = script.sha.clone();
        let sha_short: SharedString = if script.sha.len() > SHA_PREVIEW_CHARS {
            SharedString::from(format!("{}…", &script.sha[..SHA_PREVIEW_CHARS]))
        } else {
            SharedString::from(script.sha.clone())
        };
        let stats_label: SharedString = format!(
            "{} {} · {} {} · {} {}",
            script.calls,
            i18n_lua_scripts(cx, "calls_unit"),
            script.evalsha_hits,
            i18n_lua_scripts(cx, "hits_unit"),
            format_hit_rate(script.calls, script.evalsha_hits),
            i18n_lua_scripts(cx, "hit_rate_unit"),
        )
        .into();

        if code_expanded && !self.code_viewers.contains_key(&id) {
            let value = SharedString::from(script.code.clone());
            let viewer = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("lua")
                    .line_number(true)
                    .indent_guides(true)
                    .soft_wrap(false)
                    .default_value(value)
            });
            self.code_viewers.insert(id.clone(), viewer);
        }

        let id_edit = id.clone();
        let id_delete = id.clone();
        let id_run_toggle = id.clone();
        let id_code_toggle = id.clone();
        let id_run_btn = id.clone();
        let id_dup = id.clone();
        let id_warm = id.clone();
        let script_for_edit = script.clone();
        let id_for_edit = id.clone();
        let name_for_delete = SharedString::from(script.name.clone());
        let code_for_copy = script.code.clone();

        let code_block: Option<gpui::AnyElement> = if code_expanded {
            self.code_viewers.get(&id).map(|viewer| {
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .h(px(220.0))
                    .w_full()
                    .child(
                        Editor::new(viewer)
                            .appearance(false)
                            .bordered(false)
                            .disabled(true)
                            .h_full()
                            .w_full()
                            .font_family(get_mono_font_family()),
                    )
                    .into_any_element()
            })
        } else {
            None
        };

        let run_block: Option<gpui::AnyElement> = if run_expanded {
            self.run_forms.get(&id).map(|form| {
                let result_block: Option<gpui::AnyElement> = form.last.clone().map(|res| {
                    let (badge_label, badge_color) = if res.error {
                        (i18n_lua_scripts(cx, "result_error"), cx.theme().red)
                    } else if res.was_hit {
                        (i18n_lua_scripts(cx, "result_hit"), cx.theme().green)
                    } else {
                        (i18n_lua_scripts(cx, "result_miss"), cx.theme().yellow)
                    };
                    v_flex()
                        .gap_1()
                        .pt_2()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Label::new(i18n_lua_scripts(cx, "result_label"))
                                        .text_xs()
                                        .text_color(muted),
                                )
                                .child(Label::new(badge_label).text_xs().text_color(badge_color)),
                        )
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .bg(cx.theme().muted.opacity(0.4))
                                .child(
                                    Label::new(SharedString::from(res.formatted))
                                        .font_family(get_mono_font_family())
                                        .text_xs()
                                        .whitespace_normal()
                                        .text_color(foreground),
                                ),
                        )
                        .into_any_element()
                });

                let id_save_defaults = id.clone();
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .px_3()
                    .py_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(
                                        Label::new(i18n_lua_scripts(cx, "keys_label"))
                                            .text_xs()
                                            .text_color(muted),
                                    )
                                    // `Textarea` isn't `Sizable` — `text_sm`
                                    // keeps the compact type scale `small()`
                                    // used to bring.
                                    .child(Textarea::new(&form.keys).appearance(true).text_sm()),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(
                                        Label::new(i18n_lua_scripts(cx, "args_label"))
                                            .text_xs()
                                            .text_color(muted),
                                    )
                                    .child(Textarea::new(&form.args).appearance(true).text_sm()),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        Button::new(("lua-run-btn", card_hash))
                                            .primary()
                                            .small()
                                            .icon(IconName::SquareTerminal)
                                            .label(i18n_lua_scripts(cx, "run"))
                                            .disabled(is_running)
                                            .on_click(
                                                cx.listener(move |this, _, _w, cx| this.run(id_run_btn.clone(), cx)),
                                            ),
                                    )
                                    .child(
                                        Button::new(("lua-save-defaults", card_hash))
                                            .outline()
                                            .small()
                                            .label(i18n_lua_scripts(cx, "save_defaults"))
                                            .tooltip(i18n_lua_scripts(cx, "save_defaults_tooltip"))
                                            .on_click(cx.listener(move |this, _, _w, cx| {
                                                this.save_run_as_defaults(id_save_defaults.clone(), cx)
                                            })),
                                    ),
                            ),
                    )
                    .when_some(result_block, |this, block| this.child(block))
                    .into_any_element()
            })
        } else {
            None
        };

        let cache_chip: Option<gpui::AnyElement> = cached.map(|ok| {
            let (label, color) = if ok {
                (i18n_lua_scripts(cx, "cache_hit"), cx.theme().green)
            } else {
                (i18n_lua_scripts(cx, "cache_miss"), cx.theme().yellow)
            };
            div()
                .px_2()
                .rounded_sm()
                .bg(color.opacity(0.18))
                .child(Label::new(label).text_xs().text_color(color))
                .into_any_element()
        });

        v_flex()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .overflow_hidden()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        Label::new(SharedString::from(script.name.clone()))
                            .text_sm()
                            .text_color(foreground),
                    )
                    .child(
                        Button::new(("lua-copy-sha", card_hash))
                            .ghost()
                            .small()
                            .label(sha_short)
                            .tooltip(i18n_lua_scripts(cx, "copy_sha_tooltip"))
                            .on_click(cx.listener(move |_, _, w, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(sha_full.clone()));
                                w.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                            })),
                    )
                    .when_some(cache_chip, |this, chip| this.child(chip))
                    .child(Label::new(stats_label).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        Button::new(("lua-copy-code", card_hash))
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .tooltip(i18n_lua_scripts(cx, "copy_code_tooltip"))
                            .on_click(cx.listener(move |_, _, w, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code_for_copy.clone()));
                                w.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                            })),
                    )
                    .child(
                        Button::new(("lua-toggle-code", card_hash))
                            .ghost()
                            .small()
                            .label(if code_expanded {
                                i18n_lua_scripts(cx, "hide_code")
                            } else {
                                i18n_lua_scripts(cx, "show_code")
                            })
                            .on_click(cx.listener(move |this, _, _w, cx| this.toggle_code(id_code_toggle.clone(), cx))),
                    )
                    .when(can_run, |this| {
                        this.child(
                            Button::new(("lua-warm", card_hash))
                                .ghost()
                                .small()
                                .label(i18n_lua_scripts(cx, "warm"))
                                .tooltip(i18n_lua_scripts(cx, "warm_tooltip"))
                                .on_click(cx.listener(move |this, _, _w, cx| this.warm_script(id_warm.clone(), cx))),
                        )
                        .child(
                            Button::new(("lua-toggle-run", card_hash))
                                .outline()
                                .small()
                                .icon(IconName::SquareTerminal)
                                .label(if run_expanded {
                                    i18n_lua_scripts(cx, "hide_run")
                                } else {
                                    i18n_lua_scripts(cx, "run")
                                })
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.toggle_run(id_run_toggle.clone(), w, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new(("lua-dup", card_hash))
                            .ghost()
                            .small()
                            .label(i18n_lua_scripts(cx, "duplicate"))
                            .tooltip(i18n_lua_scripts(cx, "duplicate_tooltip"))
                            .on_click(cx.listener(move |this, _, _w, cx| this.duplicate(id_dup.clone(), cx))),
                    )
                    .child(
                        Button::new(("lua-edit", card_hash))
                            .outline()
                            .small()
                            .icon(CustomIconName::FilePenLine)
                            .label(i18n_lua_scripts(cx, "edit"))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                let pair = (id_for_edit.clone(), script_for_edit.clone());
                                this.open_form(Some(&pair), w, cx);
                                let _ = id_edit;
                            })),
                    )
                    .child(
                        Button::new(("lua-delete", card_hash))
                            .ghost()
                            .small()
                            .icon(IconName::CircleX)
                            .tooltip(i18n_lua_scripts(cx, "delete_tooltip"))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.confirm_delete(id_delete.clone(), name_for_delete.clone(), w, cx)
                            })),
                    ),
            )
            .when_some(run_block, |this, block| this.child(block))
            .when_some(code_block, |this, block| this.child(block))
    }

    fn render_form_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.edit_form.as_ref() else {
            return div().into_any_element();
        };
        let title_text = if form.target_id.is_some() {
            i18n_lua_scripts(cx, "edit_title")
        } else {
            i18n_lua_scripts(cx, "new_title")
        };
        let saving = self.saving;
        let code_text = form.code.read(cx).value().to_string();
        let name_text = form.name.read(cx).value().to_string();
        let keys = parse_lines(&form.default_keys.read(cx).value());
        let sha = if code_text.trim().is_empty() {
            SharedString::default()
        } else {
            SharedString::from(redis::Script::new(&code_text).get_hash().to_string())
        };
        let needed = max_keys_index(&code_text);
        let name_dup = LuaScriptManager::name_taken(name_text.trim(), form.target_id.as_deref());

        let mut hints: Vec<gpui::AnyElement> = Vec::new();
        if !sha.is_empty() {
            hints.push(
                Label::new(format!("SHA1 {sha}"))
                    .font_family(get_mono_font_family())
                    .text_xs()
                    .text_color(muted)
                    .into_any_element(),
            );
        }
        if name_dup {
            hints.push(
                Label::new(i18n_lua_scripts(cx, "name_duplicate"))
                    .text_xs()
                    .text_color(cx.theme().yellow)
                    .into_any_element(),
            );
        }
        if needed > keys.len() {
            hints.push(
                Label::new(format!(
                    "{}: KEYS[{needed}] vs {} default(s)",
                    i18n_lua_scripts(cx, "keys_count_mismatch"),
                    keys.len()
                ))
                .text_xs()
                .text_color(cx.theme().yellow)
                .into_any_element(),
            );
        } else if needed > 0 {
            hints.push(
                Label::new(format!("KEYS[1..{needed}]"))
                    .text_xs()
                    .text_color(cx.theme().green)
                    .into_any_element(),
            );
        }

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        let mut template_btns: Vec<gpui::AnyElement> = Vec::with_capacity(TEMPLATES.len());
        for tpl in TEMPLATES {
            let tid = tpl.id;
            template_btns.push(
                Button::new(("lua-tpl", id_hash(tid)))
                    .outline()
                    .small()
                    .label(i18n_lua_scripts(cx, tpl.label_key))
                    .disabled(saving)
                    .on_click(cx.listener(move |this, _, w, cx| this.apply_template(tid, w, cx)))
                    .into_any_element(),
            );
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap_2()
                    .p_4()
                    .child(Label::new(title_text).text_sm().text_color(cx.theme().foreground))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Label::new(i18n_lua_scripts(cx, "templates_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .children(template_btns),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Label::new(i18n_lua_scripts(cx, "name_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&form.name).appearance(true)),
                    )
                    .when(!hints.is_empty(), |this| {
                        this.child(h_flex().items_center().gap_3().flex_wrap().children(hints))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_1()
                            .child(
                                Label::new(i18n_lua_scripts(cx, "code_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(200.0))
                                    .w_full()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_sm()
                                    .child(
                                        Editor::new(&form.code)
                                            .appearance(false)
                                            .bordered(false)
                                            .h_full()
                                            .font_family(get_mono_font_family()),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(
                                        Label::new(i18n_lua_scripts(cx, "default_keys_label"))
                                            .text_xs()
                                            .text_color(muted),
                                    )
                                    .child(Textarea::new(&form.default_keys).appearance(true)),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(
                                        Label::new(i18n_lua_scripts(cx, "default_args_label"))
                                            .text_xs()
                                            .text_color(muted),
                                    )
                                    .child(Textarea::new(&form.default_args).appearance(true)),
                            ),
                    ),
            )
            // Sticky footer
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .justify_end()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        Button::new("lua-cancel")
                            .small()
                            .outline()
                            .disabled(saving)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_form(cx))),
                    )
                    .child(
                        Button::new("lua-save")
                            .small()
                            .primary()
                            .disabled(saving)
                            .label(i18n_common(cx, "save"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit_form(cx))),
                    ),
            )
            .into_any_element()
    }
}
