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

#[cfg(not(target_family = "wasm"))]
mod debugger;
mod render;

use crate::{
    connection::{
        Capability, ScriptRunOutcome, ServerDb, floors, max_keys_index, run_script, script_exists, script_flush,
        script_load, script_sha1,
    },
    db::{LuaScript, LuaScriptExport, LuaScriptManager},
    error::Error,
    helpers::{parse_lines, unix_ts},
    states::{
        GlobalEvent, ServerEvent, ZedisGlobalStore, ZedisServerState, ask_for_fresh_page_data, dialog_button_props,
        escalate_dangerous_body, i18n_common, i18n_lua_scripts,
    },
};
use ahash::AHashMap;
use gpui::{AnyView, ClipboardItem, Entity, SharedString, Subscription, Task, Window, prelude::*};
use gpui_kit::component::{
    input::{EditorState, InputState, TabSize, TextareaState},
    notification::Notification,
};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

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
    /// The page's *Shared* box (ADR 17); always `false` on the desktop,
    /// which has no accounts to share with.
    shared: bool,
    name: Entity<InputState>,
    code: Entity<EditorState>,
    default_keys: Entity<TextareaState>,
    default_args: Entity<TextareaState>,
}

/// State for the inline Run panel that hangs off each card.
struct RunForm {
    keys: Entity<TextareaState>,
    args: Entity<TextareaState>,
    /// Run as `EVALSHA_RO` (Redis 7.0+): the server refuses writes inside
    /// the script — the Functions panel's FCALL_RO toggle, for scripts.
    readonly: bool,
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
    /// Each script shared, so the filtered list a frame draws — and each
    /// card's Edit and Copy buttons — hold a handle instead of a copy of the
    /// script and its source.
    scripts: Vec<(String, Arc<LuaScript>)>,
    run_forms: AHashMap<String, RunForm>,
    run_expanded: AHashMap<String, bool>,
    code_expanded: AHashMap<String, bool>,
    code_viewers: AHashMap<String, Entity<EditorState>>,
    /// Server-side SCRIPT EXISTS cache, keyed by script id.
    /// `None` = not probed yet; `Some(true/false)` = last probe.
    cache_status: AHashMap<String, bool>,
    filter: Entity<InputState>,
    edit_form: Option<EditForm>,
    /// The Lua debugger, drawn in place of the list while it is open. A
    /// view rather than its type, which exists on the desktop only.
    debugger: Option<AnyView>,
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
                // Its session belongs to the server it was opened on;
                // dropping it closes that connection and aborts the script.
                this.debugger = None;
                this.run_forms.clear();
                this.run_expanded.clear();
                this.code_viewers.clear();
                this.cache_status.clear();
                this.error = None;
                this.probe_cache(cx);
                cx.notify();
            }
        }));
        // In the browser, what other accounts saved (ADR 17). An open form
        // keeps what is typed into it; the save is the last word.
        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global, event, cx| {
            if let GlobalEvent::PageDataRefreshed { scripts: true, .. } = event {
                this.take_in_refreshed(cx);
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
            debugger: None,
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
        ask_for_fresh_page_data();
        this
    }

    /// Refresh the in-memory list without dropping code viewers
    /// (callers that change source should invalidate specifically).
    fn refresh_list(&mut self, cx: &mut gpui::Context<Self>) {
        self.scripts = LuaScriptManager::list_with_id()
            .into_iter()
            .map(|(id, script)| (id, Arc::new(script)))
            .collect();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut gpui::Context<Self>) {
        self.refresh_list(cx);
        self.probe_cache(cx);
        ask_for_fresh_page_data();
    }

    /// The list again after a refresh from the bridge, and a code viewer
    /// built again only where its script's source changed or went.
    fn take_in_refreshed(&mut self, cx: &mut gpui::Context<Self>) {
        let before = std::mem::take(&mut self.scripts);
        self.refresh_list(cx);
        let code_of = |scripts: &[(String, Arc<LuaScript>)], id: &str| {
            scripts
                .iter()
                .find(|(script_id, _)| script_id == id)
                .map(|(_, script)| script.code.clone())
        };
        let now = &self.scripts;
        self.code_viewers
            .retain(|id, _| code_of(now, id).is_some_and(|code| Some(code) == code_of(&before, id)));
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
            self.run_forms.insert(
                id,
                RunForm {
                    keys,
                    args,
                    readonly: false,
                    last: None,
                },
            );
        }
        cx.notify();
    }

    fn open_form(&mut self, existing: Option<&(String, LuaScript)>, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let shared = existing.is_some_and(|(_, s)| s.shared);
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
            shared,
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
        let shared = form.shared;
        let default_keys = parse_lines(&form.default_keys.read(cx).value());
        let default_args = parse_lines(&form.default_args.read(cx).value());
        // Default KEYS are optional pre-fills for the Run panel — do not
        // block save when they under-supply KEYS[n] references in code.

        let existing = LuaScriptManager::get(&id).ok();
        let now = unix_ts();
        let created_at = existing.as_ref().map(|s| s.created_at).unwrap_or(now);
        let calls = existing.as_ref().map(|s| s.calls).unwrap_or(0);
        let evalsha_hits = existing.as_ref().map(|s| s.evalsha_hits).unwrap_or(0);
        let sha = script_sha1(&code);

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
            shared,
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
            .danger()
            .ok_text(i18n_lua_scripts(cx, "delete_confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
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
            // A copy is the copier's own until they share it.
            shared: false,
        };
        if let Err(e) = LuaScriptManager::upsert(&new_id, copy) {
            self.error = Some(e.to_string().into());
            cx.notify();
            return;
        }
        self.refresh_list(cx);
        self.probe_cache(cx);
    }

    fn toggle_run_readonly(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        if let Some(form) = self.run_forms.get_mut(&id) {
            form.readonly = !form.readonly;
            cx.notify();
        }
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
        // The toggle only renders on 7.0+, but never send the RO spelling
        // to a server that would answer "unknown command".
        let readonly = form.readonly && self.server_state.read(cx).supports(floors::EVAL_RO);
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
                let at = ServerDb::new(&*server_id, db);
                run_script(&at, &code, &sha, &keys, &args, readonly).await
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
                let at = ServerDb::new(&*server_id, db);
                let returned = script_load(&at, &code).await?;
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
                let at = ServerDb::new(&*server_id, db);
                let shas: Vec<String> = pairs.iter().map(|(_, s)| s.clone()).collect();
                let flags = script_exists(&at, &shas).await?;
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
            .danger()
            .ok_text(i18n_lua_scripts(cx, "flush_confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let server_id_inner = server_id.clone();
                this.update(cx, |this, cx| {
                    this.error = None;
                    this._probe_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let at = ServerDb::new(&*server_id_inner, db);
                            script_flush(&at, false).await
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
                        let sha = script_sha1(&item.code);
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
                            shared: false,
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

    fn filtered_scripts(&self, cx: &gpui::Context<Self>) -> Vec<(String, Arc<LuaScript>)> {
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

    /// Open the Lua debugger on a saved script, in place of the list.
    #[cfg(not(target_family = "wasm"))]
    fn open_debugger(&mut self, id: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        let script = match LuaScriptManager::get(id) {
            Ok(script) => script,
            Err(e) => {
                self.error = Some(e.to_string().into());
                cx.notify();
                return;
            }
        };
        let library = cx.entity().downgrade();
        let server_state = self.server_state.clone();
        let view = cx.new(|cx| debugger::LuaDebugger::new(library, server_state, &script, window, cx));
        self.debugger = Some(view.into());
        cx.notify();
    }

    /// The browser has no debugger (`debugs_lua_scripts`), so nothing draws
    /// the button that would call this.
    #[cfg(target_family = "wasm")]
    fn open_debugger(&mut self, _id: &str, _window: &mut Window, _cx: &mut gpui::Context<Self>) {}
}
