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

use crate::{
    assets::CustomIconName,
    connection::{Capability, ScriptRunOutcome, get_connection_manager, run_script},
    db::{LuaScript, LuaScriptManager},
    error::Error,
    helpers::{get_mono_font_family, unix_ts},
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_lua_scripts,
    },
};
use ahash::AHashMap;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState, TabSize},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use tracing::info;
use uuid::Uuid;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// SHA prefix length shown in the card header — full 40 chars are
/// pointless in a list, eight unambiguously identify the script.
const SHA_PREVIEW_CHARS: usize = 12;

/// In-flight script editor. `target_id` is `Some` when editing an
/// existing entry; `None` for a brand-new script.
struct EditForm {
    target_id: Option<String>,
    name: Entity<InputState>,
    code: Entity<InputState>,
    default_keys: Entity<InputState>,
    default_args: Entity<InputState>,
}

/// State for the inline Run panel that hangs off each card.
struct RunForm {
    keys: Entity<InputState>,
    args: Entity<InputState>,
    /// Most recent run outcome, formatted for display. `None` until
    /// the user clicks Run for the first time on this card.
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
    /// Per-card Run form state, keyed by script id. Lazily created on
    /// first expand, cached afterwards so re-expanding is instant.
    run_forms: AHashMap<String, RunForm>,
    /// Which cards currently show their Run / code panels.
    run_expanded: AHashMap<String, bool>,
    code_expanded: AHashMap<String, bool>,
    /// Lazily-created read-only Lua viewers for the inline code
    /// preview (one per script id). Cleared on every reload so an
    /// edit landing in the DB flushes the cached source view.
    code_viewers: AHashMap<String, Entity<InputState>>,
    edit_form: Option<EditForm>,
    error: Option<SharedString>,
    /// Script id currently running. Used to disable the Run button
    /// for that card while in flight; other cards stay clickable.
    running: Option<String>,
    saving: bool,
    _run_task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisLuaScriptLibrary {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if let ServerEvent::ServerSelected(_) = event {
                // Server change clears any pending Run state since
                // the next run will go to a different host — keep
                // the library itself though, it's global.
                this.run_forms.clear();
                this.run_expanded.clear();
                this.code_viewers.clear();
                this.error = None;
                cx.notify();
            }
        }));
        let mut this = Self {
            server_state,
            scripts: Vec::new(),
            run_forms: AHashMap::new(),
            run_expanded: AHashMap::new(),
            code_expanded: AHashMap::new(),
            code_viewers: AHashMap::new(),
            edit_form: None,
            error: None,
            running: None,
            saving: false,
            _run_task: None,
            _save_task: None,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    /// Pull the latest library snapshot from the local DB cache. Pure
    /// in-memory operation — no network round-trip needed.
    fn reload(&mut self, cx: &mut gpui::Context<Self>) {
        self.scripts = LuaScriptManager::list_with_id();
        // Drop stale code viewers / run forms tied to ids that may
        // have been deleted. Cheap to recreate on next render.
        self.code_viewers.clear();
        cx.notify();
    }

    fn toggle_code(&mut self, id: String, cx: &mut gpui::Context<Self>) {
        let entry = self.code_expanded.entry(id).or_insert(false);
        *entry = !*entry;
        cx.notify();
    }

    fn toggle_run(&mut self, id: String, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // Defense in depth — the run toggle is hidden without EvalScript.
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        let want_open = !*self.run_expanded.entry(id.clone()).or_insert(false);
        self.run_expanded.insert(id.clone(), want_open);
        if want_open && !self.run_forms.contains_key(&id) {
            // Build the Run form lazily — most scripts in a library
            // aren't run during a given session, so creating one
            // InputState per script up-front would waste entities.
            let script = match LuaScriptManager::get(&id) {
                Ok(s) => s,
                Err(_) => return,
            };
            let keys_default = script.default_keys.join("\n");
            let args_default = script.default_args.join("\n");
            // `auto_grow(2, 6)` makes the input multi-line — required
            // because placeholders / defaults are line-separated lists
            // and feeding `\n` into a single-line input panics
            // `shape_line` during render.
            let keys = cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder(i18n_lua_scripts(cx, "keys_placeholder"))
                    .default_value(keys_default)
            });
            let args = cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder(i18n_lua_scripts(cx, "args_placeholder"))
                    .default_value(args_default)
            });
            self.run_forms.insert(id, RunForm { keys, args, last: None });
        }
        cx.notify();
    }

    /// Open the unified create/edit form. `existing` pre-fills the
    /// fields; `None` opens a blank form for a new script.
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
                SharedString::from(
                    "-- Lua script. KEYS / ARGV are populated from the run form.\nreturn redis.call('GET', KEYS[1])",
                ),
                SharedString::default(),
                SharedString::default(),
            ),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_lua_scripts(cx, "name_placeholder"))
                .default_value(name_val)
        });
        let code = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("lua")
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
            // Multi-line: one KEY per line, same constraint as the
            // run-form keys input.
            InputState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(i18n_lua_scripts(cx, "default_keys_placeholder"))
                .default_value(keys_val)
        });
        let default_args = cx.new(|cx| {
            InputState::new(window, cx)
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
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut gpui::Context<Self>) {
        self.edit_form = None;
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

        // Preserve existing usage counters when editing — only the
        // code / name / defaults are being changed.
        let existing = LuaScriptManager::get(&id).ok();
        let now = unix_ts();
        let created_at = existing.as_ref().map(|s| s.created_at).unwrap_or(now);
        let calls = existing.as_ref().map(|s| s.calls).unwrap_or(0);
        let evalsha_hits = existing.as_ref().map(|s| s.evalsha_hits).unwrap_or(0);
        // Recompute SHA every time — code may have changed, and even
        // the same code would produce the same hash, so the cost is
        // a single redis::Script::new call.
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
        // Successful save → close form, refresh list, drop stale
        // cached editor for this id so the next code-preview render
        // picks up the new source.
        self.edit_form = None;
        self.error = None;
        self.code_viewers.remove(&id);
        self.reload(cx);
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
                        this.reload(cx);
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

    /// Execute the script for the given card. Reads KEYS / ARGS from
    /// that card's Run form, dispatches `EVALSHA` (with `SCRIPT LOAD +
    /// EVAL` fallback), and records the hit/miss into the script's
    /// lifetime counters.
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
            let result: Result<ScriptRunOutcome> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.running = None;
                match result {
                    Ok(outcome) => {
                        // Persist the hit/miss to disk so the rate
                        // accumulates across sessions.
                        if let Err(e) = LuaScriptManager::record_call(&id_for_task, outcome.was_hit) {
                            tracing::warn!(error = %e, "failed to record script call");
                        }
                        if let Some(form) = this.run_forms.get_mut(&id_for_task) {
                            form.last = Some(RunResult {
                                formatted: outcome.formatted,
                                was_hit: outcome.was_hit,
                                error: false,
                            });
                        }
                        this.reload(cx);
                    }
                    Err(e) => {
                        // Errors don't count as hits/misses — the
                        // script never reached the cache check.
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
}

/// Split a multi-line input field into trimmed, non-empty entries.
/// Used to parse KEYS / ARGS / default_keys / default_args — the user
/// types one value per line, blank lines and surrounding whitespace
/// are ignored.
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

/// DJB2-style stable hash so element IDs derived from script UUIDs
/// compile to `u32` for gpui's `ElementId::From<(&str, u32)>`.
fn id_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

impl gpui::Render for ZedisLuaScriptLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let header = self.render_header(cx).into_any_element();

        if self.edit_form.is_some() {
            let form_panel = self.render_form_panel(cx).into_any_element();
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(form_panel),
                )
                .into_any_element();
        }

        let body: gpui::AnyElement = if self.scripts.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_lua_scripts(cx, "empty")).text_color(muted))
                .into_any_element()
        } else {
            let scripts_snapshot = self.scripts.clone();
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(scripts_snapshot.len());
            for (id, script) in &scripts_snapshot {
                rows.push(
                    self.render_card(id.clone(), script.clone(), window, cx)
                        .into_any_element(),
                );
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

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(header)
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(body))
            .into_any_element()
    }
}

impl ZedisLuaScriptLibrary {
    fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
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
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Icon::new(IconName::SquareTerminal))
                    .child(Label::new(i18n_lua_scripts(cx, "title")).text_color(cx.theme().foreground))
                    .child(Label::new(count_label).text_color(muted).text_sm()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
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

    fn render_card(
        &mut self,
        id: String,
        script: LuaScript,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        // EVAL/EVALSHA mutate server state (Capability::EvalScript); the
        // local script library itself (redb) stays editable read-only.
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let code_expanded = *self.code_expanded.get(&id).unwrap_or(&false);
        let run_expanded = *self.run_expanded.get(&id).unwrap_or(&false);
        let is_running = self.running.as_ref() == Some(&id);
        let card_hash = id_hash(&id);

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

        // Lazy code-viewer creation, gated on the expand state.
        if code_expanded && !self.code_viewers.contains_key(&id) {
            let value = SharedString::from(script.code.clone());
            let viewer = cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("lua")
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
        let script_for_edit = script.clone();
        let id_for_edit = id.clone();
        let name_for_delete = SharedString::from(script.name.clone());

        let code_block: Option<gpui::AnyElement> = if code_expanded {
            self.code_viewers.get(&id).map(|viewer| {
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .h(px(220.0))
                    .w_full()
                    .child(
                        Input::new(viewer)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
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
                                    .child(Input::new(&form.keys).appearance(true).small()),
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
                                    .child(Input::new(&form.args).appearance(true).small()),
                            )
                            .child(
                                Button::new(("lua-run-btn", card_hash))
                                    .primary()
                                    .small()
                                    .icon(IconName::Search)
                                    .label(i18n_lua_scripts(cx, "run"))
                                    .disabled(is_running)
                                    .on_click(cx.listener(move |this, _, _w, cx| this.run(id_run_btn.clone(), cx))),
                            ),
                    )
                    .when_some(result_block, |this, block| this.child(block))
                    .into_any_element()
            })
        } else {
            None
        };

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
                        Label::new(sha_short)
                            .font_family(get_mono_font_family())
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Label::new(stats_label).text_xs().text_color(muted))
                    .child(div().flex_1())
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
                            Button::new(("lua-toggle-run", card_hash))
                                .outline()
                                .small()
                                .icon(IconName::Search)
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

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(Label::new(title_text).text_sm().text_color(cx.theme().foreground))
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
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_lua_scripts(cx, "code_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .h(px(300.0))
                            .w_full()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_sm()
                            .child(
                                Input::new(&form.code)
                                    .appearance(false)
                                    .bordered(false)
                                    .focus_bordered(false)
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
                            .child(Input::new(&form.default_keys).appearance(true)),
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
                            .child(Input::new(&form.default_args).appearance(true)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
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
