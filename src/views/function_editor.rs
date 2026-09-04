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

//! Redis 7+ Functions library manager.
//!
//! Surfaces library lifecycle (`FUNCTION LIST/LOAD/DELETE/FLUSH/DUMP/
//! RESTORE/STATS`) plus per-function trial runs via `FCALL` /
//! `FCALL_RO`. New library and Edit share the same Lua editor; the
//! only difference is pre-fill and whether `REPLACE` defaults on.

use crate::views::unavailable_chip;
use crate::{
    assets::CustomIconName,
    connection::{
        Capability, FunctionLibrary, FunctionMeta, FunctionRestorePolicy, FunctionStats, KillTarget, function_delete,
        function_dump, function_fcall, function_flush, function_list, function_load, function_restore, function_stats,
        get_connection_manager, validate_library_source,
    },
    error::Error,
    helpers::get_mono_font_family,
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, dialog_button_props,
        escalate_dangerous_body, i18n_common, i18n_functions,
    },
};
use ahash::{AHashMap, AHashSet};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use gpui::{ClipboardItem, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, Input, InputState, TabSize},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use tracing::info;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

const CODE_PREVIEW_MAX_HEIGHT: f32 = 280.0;

/// Built-in Lua library templates for the load form.
struct CodeTemplate {
    id: &'static str,
    /// i18n key under `[functions]` for the chip label.
    label_key: &'static str,
    source: &'static str,
}

const TEMPLATES: &[CodeTemplate] = &[
    CodeTemplate {
        id: "hello",
        label_key: "template_hello",
        source: "#!lua name=mylib\n\nredis.register_function(\n  'hello',\n  function(keys, args)\n    return 'hello from mylib'\n  end\n)\n",
    },
    CodeTemplate {
        id: "echo",
        label_key: "template_echo",
        source: "#!lua name=echo_lib\n\nredis.register_function(\n  'echo',\n  function(keys, args)\n    return { keys = keys, args = args }\n  end\n)\n",
    },
    CodeTemplate {
        id: "get",
        label_key: "template_get",
        source: "#!lua name=kv_lib\n\nredis.register_function({\n  function_name = 'get',\n  callback = function(keys, args)\n    return redis.call('GET', keys[1])\n  end,\n  flags = { 'no-writes' }\n})\n",
    },
    CodeTemplate {
        id: "incr",
        label_key: "template_incr",
        source: "#!lua name=counter_lib\n\nredis.register_function(\n  'incrby',\n  function(keys, args)\n    local n = tonumber(args[1]) or 1\n    return redis.call('INCRBY', keys[1], n)\n  end\n)\n",
    },
];

/// In-flight create / edit form. `target_name` is `Some(...)` when
/// editing an existing library so we know what to refresh after the
/// LOAD succeeds, plus REPLACE defaults to true.
struct EditForm {
    target_name: Option<SharedString>,
    code: Entity<EditorState>,
    replace: bool,
}

/// Inline FCALL trial-run form hanging off a function name.
struct RunForm {
    keys: Entity<InputState>,
    args: Entity<InputState>,
    /// Prefer `FCALL_RO` when true (function declared `no-writes`).
    readonly: bool,
    last: Option<RunResult>,
}

#[derive(Debug, Clone)]
struct RunResult {
    formatted: String,
    error: bool,
}

pub struct ZedisFunctionEditor {
    server_state: Entity<ZedisServerState>,
    libraries: Vec<FunctionLibrary>,
    stats: Option<FunctionStats>,
    /// Library names whose code panel is currently expanded inline.
    expanded: AHashSet<SharedString>,
    /// Function names whose FCALL panel is open.
    run_expanded: AHashSet<SharedString>,
    run_forms: AHashMap<SharedString, RunForm>,
    /// Lazily-created read-only Lua editors for inline code previews.
    code_editors: AHashMap<SharedString, Entity<EditorState>>,
    filter: Entity<InputState>,
    /// `true` when `FUNCTION LIST` reported unknown command.
    unsupported: bool,
    edit_form: Option<EditForm>,
    error: Option<SharedString>,
    loading: bool,
    submitting: bool,
    running: Option<SharedString>,
    deleting: Option<SharedString>,
    /// Toast deferred to `render` so async completions can notify without a `Window`.
    pending_notification: Option<Notification>,
    _fetch_task: Option<Task<()>>,
    _mutate_task: Option<Task<()>>,
    _run_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisFunctionEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| match event {
            ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated => {
                this.libraries.clear();
                this.stats = None;
                this.expanded.clear();
                this.run_expanded.clear();
                this.run_forms.clear();
                this.code_editors.clear();
                this.error = None;
                this.unsupported = false;
                this.fetch(cx);
            }
            _ => {}
        }));
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_functions(cx, "filter_placeholder").to_string()));
        let mut this = Self {
            server_state,
            libraries: Vec::new(),
            stats: None,
            expanded: AHashSet::new(),
            run_expanded: AHashSet::new(),
            run_forms: AHashMap::new(),
            code_editors: AHashMap::new(),
            filter,
            unsupported: false,
            edit_form: None,
            error: None,
            loading: false,
            submitting: false,
            running: None,
            deleting: None,
            pending_notification: None,
            _fetch_task: None,
            _mutate_task: None,
            _run_task: None,
            _subscriptions: subscriptions,
        };
        this.fetch(cx);
        this
    }

    /// Pull library list + engine stats. Always asks `WITHCODE` so the
    /// inline expand toggle is instant.
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
                let listing = function_list(&mut conn, true).await?;
                let stats = if listing.unsupported {
                    None
                } else {
                    function_stats(&mut conn).await.ok()
                };
                Ok::<_, Error>((listing, stats))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((listing, stats)) => {
                        this.unsupported = listing.unsupported;
                        this.libraries = listing.libraries;
                        this.stats = stats;
                        this.code_editors.clear();
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

    fn toggle_expanded(&mut self, name: SharedString, cx: &mut gpui::Context<Self>) {
        if !self.expanded.insert(name.clone()) {
            self.expanded.remove(&name);
        }
        cx.notify();
    }

    fn toggle_run(&mut self, fn_name: SharedString, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.run_expanded.insert(fn_name.clone()) {
            self.run_expanded.remove(&fn_name);
            cx.notify();
            return;
        }
        if !self.run_forms.contains_key(&fn_name) {
            // Prefer RO when the listed flags include no-writes.
            let readonly = self
                .libraries
                .iter()
                .flat_map(|l| l.functions.iter())
                .find(|f| f.name == fn_name.as_ref())
                .map(|f| f.flags.iter().any(|flag| flag.eq_ignore_ascii_case("no-writes")))
                .unwrap_or(false);
            let keys = cx
                .new(|cx| InputState::new(window, cx).placeholder(i18n_functions(cx, "keys_placeholder").to_string()));
            let args = cx
                .new(|cx| InputState::new(window, cx).placeholder(i18n_functions(cx, "args_placeholder").to_string()));
            self.run_forms.insert(
                fn_name,
                RunForm {
                    keys,
                    args,
                    readonly,
                    last: None,
                },
            );
        }
        cx.notify();
    }

    /// Open the unified create/edit form. `existing` is the library to
    /// pre-fill from, or `None` for a new library.
    fn open_form(&mut self, existing: Option<&FunctionLibrary>, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let default_value: SharedString = existing
            .and_then(|l| l.code.clone().map(SharedString::from))
            .unwrap_or_else(|| SharedString::from(TEMPLATES[0].source));
        let code = cx.new(|cx| {
            // Pass "lua" literally — Lua is registered manually via
            // `register_extra_languages()` at app startup.
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
                .default_value(default_value)
        });
        let target_name = existing.map(|l| SharedString::from(l.name.clone()));
        let replace = target_name.is_some();
        self.edit_form = Some(EditForm {
            target_name,
            code,
            replace,
        });
        self.error = None;
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut gpui::Context<Self>) {
        self.edit_form = None;
        cx.notify();
    }

    fn toggle_replace(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(form) = self.edit_form.as_mut() {
            form.replace = !form.replace;
            cx.notify();
        }
    }

    fn apply_template(&mut self, template_id: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(tpl) = TEMPLATES.iter().find(|t| t.id == template_id) else {
            return;
        };
        let Some(form) = self.edit_form.as_ref() else {
            return;
        };
        let source = tpl.source.to_string();
        form.code.update(cx, |state, cx| {
            state.set_value(source, window, cx);
        });
        cx.notify();
    }

    fn submit(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::FunctionWrite) {
            return;
        }
        let Some(form) = self.edit_form.as_ref() else { return };
        let code = form.code.read(cx).value().to_string();
        if let Err(err) = validate_library_source(&code) {
            let msg = if let crate::connection::LibraryValidateError::InvalidName(ref name) = err {
                SharedString::from(format!("{}: {name}", i18n_functions(cx, err.i18n_key())))
            } else {
                i18n_functions(cx, err.i18n_key())
            };
            self.error = Some(msg);
            cx.notify();
            return;
        }
        let replace = form.replace;
        let target_name = form.target_name.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.submitting = true;
        self.error = None;
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                function_load(&mut conn, &code, replace).await
            });
            let result: Result<String> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.submitting = false;
                match result {
                    Ok(loaded_name) => {
                        info!(library = loaded_name.as_str(), "FUNCTION LOAD succeeded");
                        let _ = target_name;
                        this.edit_form = None;
                        this.fetch(cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn run_fcall(&mut self, fn_name: SharedString, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        if self.running.is_some() {
            return;
        }
        let Some(form) = self.run_forms.get(fn_name.as_ref()) else {
            return;
        };
        let keys = parse_lines(&form.keys.read(cx).value());
        let args = parse_lines(&form.args.read(cx).value());
        let readonly = form.readonly;
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            self.error = Some(i18n_functions(cx, "no_server"));
            cx.notify();
            return;
        }
        let name_for_task = fn_name.to_string();
        self.running = Some(fn_name.clone());
        self.error = None;
        self._run_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                function_fcall(&mut conn, &name_for_task, &keys, &args, readonly).await
            });
            let result: Result<String> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running = None;
                if let Some(form) = this.run_forms.get_mut(fn_name.as_ref()) {
                    match result {
                        Ok(formatted) => {
                            form.last = Some(RunResult {
                                formatted,
                                error: false,
                            });
                        }
                        Err(e) => {
                            form.last = Some(RunResult {
                                formatted: e.to_string(),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn toggle_run_readonly(&mut self, fn_name: SharedString, cx: &mut gpui::Context<Self>) {
        if let Some(form) = self.run_forms.get_mut(&fn_name) {
            form.readonly = !form.readonly;
            cx.notify();
        }
    }

    fn confirm_delete(&mut self, library: SharedString, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::FunctionWrite) {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = i18n_functions(cx, "delete_title");
        let message: SharedString = format!(
            "{}\n\n{}: {}",
            i18n_functions(cx, "delete_message"),
            i18n_functions(cx, "delete_library_label"),
            library,
        )
        .into();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        let library_for_task = library.clone();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, message))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_functions(cx, "delete_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let lib = library_for_task.clone();
                let server_id_inner = server_id.clone();
                this.update(cx, |this, cx| {
                    this.deleting = Some(lib.clone());
                    this.error = None;
                    let log_name = lib.clone();
                    this._mutate_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let mut conn = get_connection_manager().get_connection(&server_id_inner, db).await?;
                            function_delete(&mut conn, lib.as_ref()).await
                        });
                        let result: Result<()> = task.await.map_err(Into::into);
                        let _ = handle.update(cx, |this, cx| {
                            this.deleting = None;
                            match result {
                                Ok(()) => {
                                    info!(library = %log_name, "FUNCTION DELETE succeeded");
                                    this.fetch(cx);
                                }
                                Err(e) => {
                                    this.error = Some(e.to_string().into());
                                }
                            }
                            cx.notify();
                        });
                    }));
                });
                true
            })
            .open(window, cx);
    }

    fn confirm_flush(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::FunctionWrite) {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = i18n_functions(cx, "flush_title");
        let message = i18n_functions(cx, "flush_message");
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, message))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_functions(cx, "flush_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let server_id_inner = server_id.clone();
                this.update(cx, |this, cx| {
                    this.error = None;
                    this._mutate_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let mut conn = get_connection_manager().get_connection(&server_id_inner, db).await?;
                            function_flush(&mut conn, false).await
                        });
                        let result: Result<()> = task.await.map_err(Into::into);
                        let _ = handle.update(cx, |this, cx| match result {
                            Ok(()) => {
                                info!("FUNCTION FLUSH succeeded");
                                this.fetch(cx);
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

    fn dump_to_clipboard(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.error = None;
        self._mutate_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                function_dump(&mut conn).await
            });
            let result: Result<Vec<u8>> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(bytes) => {
                        let encoded = B64.encode(bytes);
                        cx.write_to_clipboard(ClipboardItem::new_string(encoded));
                        this.pending_notification = Some(Notification::info(i18n_functions(cx, "dump_copied")));
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn restore_from_clipboard(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.server_state.read(cx).can(Capability::FunctionWrite) {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            self.error = Some(i18n_functions(cx, "restore_empty_clipboard"));
            cx.notify();
            return;
        };
        let Some(text) = item.text() else {
            self.error = Some(i18n_functions(cx, "restore_empty_clipboard"));
            cx.notify();
            return;
        };
        let trimmed = text.trim();
        let payload = match B64.decode(trimmed) {
            Ok(b) => b,
            Err(_) => {
                self.error = Some(i18n_functions(cx, "restore_bad_payload"));
                cx.notify();
                return;
            }
        };
        let entity = cx.entity().downgrade();
        let title = i18n_functions(cx, "restore_title");
        let message = i18n_functions(cx, "restore_message");
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, message))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_functions(cx, "restore_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _w, cx| {
                let Some(this) = entity.upgrade() else { return true };
                let server_id_inner = server_id.clone();
                let payload = payload.clone();
                this.update(cx, |this, cx| {
                    this.error = None;
                    this._mutate_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let mut conn = get_connection_manager().get_connection(&server_id_inner, db).await?;
                            function_restore(&mut conn, &payload, FunctionRestorePolicy::Replace).await
                        });
                        let result: Result<()> = task.await.map_err(Into::into);
                        let _ = handle.update(cx, |this, cx| match result {
                            Ok(()) => {
                                info!("FUNCTION RESTORE succeeded");
                                this.fetch(cx);
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

    fn filtered_libraries(&self, cx: &gpui::Context<Self>) -> Vec<FunctionLibrary> {
        let q = self.filter.read(cx).value().to_string();
        let q = q.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.libraries.clone();
        }
        self.libraries
            .iter()
            .filter(|lib| {
                lib.name.to_ascii_lowercase().contains(&q)
                    || lib.functions.iter().any(|f| f.name.to_ascii_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }
}

/// Split a multi-line KEYS / ARGV field into trimmed non-empty entries.
fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

impl gpui::Render for ZedisFunctionEditor {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let muted = cx.theme().muted_foreground;
        let header = self.render_header(cx).into_any_element();

        // Edit/New form takeover — sticky footer keeps Load visible.
        if self.edit_form.is_some() {
            let form_panel = self.render_form_panel(window, cx).into_any_element();
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(form_panel)
                .into_any_element();
        }

        let filtered = self.filtered_libraries(cx);
        let body: gpui::AnyElement = if self.unsupported {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_functions(cx, "unsupported")).text_color(muted))
                .into_any_element()
        } else if self.loading && self.libraries.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_common(cx, "loading")).text_color(muted))
                .into_any_element()
        } else if self.libraries.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_functions(cx, "empty")).text_color(muted))
                .into_any_element()
        } else if filtered.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_functions(cx, "filter_empty")).text_color(muted))
                .into_any_element()
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(filtered.len());
            for lib in filtered {
                rows.push(self.render_library_card(lib, window, cx).into_any_element());
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

        let stats_bar = self.render_stats_bar(cx).into_any_element();
        let filter_bar = self.render_filter_bar(cx).into_any_element();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(header)
            .child(stats_bar)
            .child(filter_bar)
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(body))
            .into_any_element()
    }
}

impl ZedisFunctionEditor {
    fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let can_write = self.server_state.read(cx).can(Capability::FunctionWrite);
        let muted = cx.theme().muted_foreground;
        let count = self.libraries.len();
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
                        Button::new("functions-back")
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
                    .child(Icon::new(IconName::Asterisk))
                    .child(Label::new(i18n_functions(cx, "title")).text_color(cx.theme().foreground))
                    .child(Label::new(count_label).text_color(muted).text_sm()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .when_some(
                        self.server_state.read(cx).blocked_by(Capability::FunctionWrite),
                        |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                    )
                    .when(can_write && !self.unsupported, |this| {
                        this.child(
                            Button::new("functions-dump")
                                .ghost()
                                .small()
                                .label(i18n_functions(cx, "dump"))
                                .tooltip(i18n_functions(cx, "dump_tooltip"))
                                .disabled(self.submitting || self.libraries.is_empty())
                                .on_click(cx.listener(|this, _, w, cx| this.dump_to_clipboard(w, cx))),
                        )
                        .child(
                            Button::new("functions-restore")
                                .ghost()
                                .small()
                                .label(i18n_functions(cx, "restore"))
                                .tooltip(i18n_functions(cx, "restore_tooltip"))
                                .disabled(self.submitting)
                                .on_click(cx.listener(|this, _, w, cx| this.restore_from_clipboard(w, cx))),
                        )
                        .child(
                            Button::new("functions-flush")
                                .ghost()
                                .small()
                                .label(i18n_functions(cx, "flush"))
                                .tooltip(i18n_functions(cx, "flush_tooltip"))
                                .disabled(self.submitting || self.libraries.is_empty())
                                .on_click(cx.listener(|this, _, w, cx| this.confirm_flush(w, cx))),
                        )
                    })
                    .when(can_write, |this| {
                        this.child(
                            Button::new("functions-new")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label(i18n_functions(cx, "new_library"))
                                .disabled(self.submitting || self.unsupported)
                                .on_click(cx.listener(|this, _, w, cx| this.open_form(None, w, cx))),
                        )
                    })
                    // Not a write: stopping a runaway FCALL changes no data,
                    // and a read-only ACL user gets the server's answer.
                    .when(!self.unsupported, |this| {
                        this.child(
                            Button::new("functions-kill")
                                .ghost()
                                .small()
                                .label(i18n_common(cx, "kill_function_button"))
                                .tooltip(i18n_common(cx, "kill_function_tooltip"))
                                .on_click(cx.listener(|this, _, _w, cx| {
                                    this.server_state
                                        .update(cx, |state, cx| state.kill_running_script(KillTarget::Function, cx));
                                })),
                        )
                    })
                    .child(
                        Button::new("functions-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_functions(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _w, cx| this.fetch(cx))),
                    ),
            )
    }

    fn render_stats_bar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(stats) = self.stats.as_ref() else {
            return div().into_any_element();
        };
        if self.unsupported {
            return div().into_any_element();
        }
        let summary: SharedString = format!(
            "{}: {} · {}: {}",
            i18n_functions(cx, "stats_libraries"),
            stats.libraries_count,
            i18n_functions(cx, "stats_functions"),
            stats.functions_count,
        )
        .into();
        let running: Option<SharedString> = stats.running_name.as_ref().map(|name| {
            let ms = stats.running_duration_ms.unwrap_or(0);
            format!("{}: {name} ({ms} ms)", i18n_functions(cx, "stats_running")).into()
        });
        h_flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Label::new(summary).text_xs().text_color(muted))
            .when_some(running, |this, text| {
                this.child(Label::new(text).text_xs().text_color(cx.theme().yellow))
            })
            .into_any_element()
    }

    fn render_filter_bar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if self.unsupported || self.libraries.is_empty() {
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

    fn render_library_card(
        &mut self,
        lib: FunctionLibrary,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let can_write = self.server_state.read(cx).can(Capability::FunctionWrite);
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        let muted = cx.theme().muted_foreground;
        let theme_blue = cx.theme().blue;
        let name = lib.name.clone();
        let name_for_id = name.clone();
        let id_hash: u32 = djb2_hash(name_for_id.as_ref());
        let expanded = self.expanded.contains(name.as_str());
        let deleting = self.deleting.as_deref() == Some(name.as_str());
        let engine_chip = self.chip(lib.engine.clone().into(), theme_blue, cx).into_any_element();
        let funcs_count = lib.functions.len();

        // Function name rows with optional FCALL action.
        let mut func_rows: Vec<gpui::AnyElement> = Vec::with_capacity(funcs_count);
        for f in &lib.functions {
            func_rows.push(self.render_function_row(f, can_run, window, cx).into_any_element());
        }

        if expanded
            && !self.code_editors.contains_key(name.as_str())
            && let Some(code) = lib.code.as_ref()
        {
            let value = code.clone();
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("lua")
                    .line_number(true)
                    .indent_guides(true)
                    .soft_wrap(false)
                    .default_value(value)
            });
            self.code_editors.insert(name.clone().into(), editor);
        }

        let code_block: Option<gpui::AnyElement> = if expanded {
            if let Some(editor) = self.code_editors.get(name.as_str()) {
                Some(
                    div()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .h(px(CODE_PREVIEW_MAX_HEIGHT))
                        .w_full()
                        .child(
                            Editor::new(editor)
                                .appearance(false)
                                .bordered(false)
                                .disabled(true)
                                .h_full()
                                .w_full()
                                .font_family(get_mono_font_family()),
                        )
                        .into_any_element(),
                )
            } else {
                Some(
                    div()
                        .px_3()
                        .py_2()
                        .child(
                            Label::new(i18n_functions(cx, "code_not_loaded"))
                                .text_color(muted)
                                .text_xs(),
                        )
                        .into_any_element(),
                )
            }
        } else {
            None
        };

        let name_for_expand = name.clone();
        let lib_for_edit = lib.clone();
        let name_for_delete = name.clone();
        let name_for_copy = name.clone();

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
                    .child(Label::new(name.clone()).text_color(cx.theme().foreground).text_sm())
                    .child(engine_chip)
                    .child(Label::new(format!("{funcs_count} fn")).text_color(muted).text_xs())
                    .child(div().flex_1())
                    .child(
                        Button::new(("fn-copy", id_hash))
                            .ghost()
                            .small()
                            .tooltip(i18n_functions(cx, "copy_name_tooltip"))
                            .icon(IconName::Copy)
                            .on_click(cx.listener(move |_, _, w, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(name_for_copy.clone()));
                                w.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                            })),
                    )
                    .child(
                        Button::new(("fn-expand", id_hash))
                            .ghost()
                            .small()
                            .label(if expanded {
                                i18n_functions(cx, "hide_code")
                            } else {
                                i18n_functions(cx, "show_code")
                            })
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.toggle_expanded(name_for_expand.clone().into(), cx)
                            })),
                    )
                    .when(can_write, |this| {
                        this.child(
                            Button::new(("fn-edit", id_hash))
                                .outline()
                                .small()
                                .icon(CustomIconName::FilePenLine)
                                .label(i18n_functions(cx, "edit"))
                                .disabled(self.submitting)
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.open_form(Some(&lib_for_edit), w, cx)),
                                ),
                        )
                    })
                    .when(can_write, |this| {
                        this.child(
                            Button::new(("fn-delete", id_hash))
                                .ghost()
                                .small()
                                .icon(IconName::CircleX)
                                .tooltip(i18n_functions(cx, "delete_tooltip"))
                                .disabled(deleting)
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.confirm_delete(name_for_delete.clone().into(), w, cx)
                                })),
                        )
                    }),
            )
            .when(!func_rows.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .px_3()
                        .pb_2()
                        .child(
                            Label::new(i18n_functions(cx, "functions_label"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .children(func_rows),
                )
            })
            .when_some(code_block, |this, block| this.child(block))
    }

    fn render_function_row(
        &mut self,
        meta: &FunctionMeta,
        can_run: bool,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let fn_name = meta.name.clone();
        let id_hash = djb2_hash(&fn_name);
        let run_open = self.run_expanded.contains(fn_name.as_str());
        let is_running = self.running.as_deref() == Some(fn_name.as_str());
        let flags_text = if meta.flags.is_empty() {
            None
        } else {
            Some(format!(
                "({})",
                meta.flags.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ))
        };
        let desc = meta.description.clone();

        let run_block: Option<gpui::AnyElement> = if run_open {
            self.run_forms.get(fn_name.as_str()).map(|form| {
                let result_block: Option<gpui::AnyElement> = form.last.clone().map(|res| {
                    let badge_color = if res.error { cx.theme().red } else { cx.theme().green };
                    let badge_label = if res.error {
                        i18n_functions(cx, "result_error")
                    } else {
                        i18n_functions(cx, "result_ok")
                    };
                    v_flex()
                        .gap_1()
                        .pt_2()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Label::new(i18n_functions(cx, "result_label"))
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

                let fn_for_run = SharedString::from(fn_name.clone());
                let fn_for_ro = SharedString::from(fn_name.clone());
                let readonly = form.readonly;

                div()
                    .mt_1()
                    .ml_2()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(Label::new(i18n_functions(cx, "keys_label")).text_xs().text_color(muted))
                                    .child(Input::new(&form.keys).appearance(true).small()),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .child(Label::new(i18n_functions(cx, "args_label")).text_xs().text_color(muted))
                                    .child(Input::new(&form.args).appearance(true).small()),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        Button::new(("fn-ro", id_hash))
                                            .small()
                                            .when(readonly, |b| b.primary())
                                            .when(!readonly, |b| b.outline())
                                            .label(if readonly { "FCALL_RO" } else { "FCALL" })
                                            .tooltip(i18n_functions(cx, "fcall_ro_tooltip"))
                                            .on_click(cx.listener(move |this, _, _w, cx| {
                                                this.toggle_run_readonly(fn_for_ro.clone(), cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("fn-run-btn", id_hash))
                                            .primary()
                                            .small()
                                            .icon(IconName::Search)
                                            .label(i18n_functions(cx, "run"))
                                            .disabled(is_running)
                                            .on_click(cx.listener(move |this, _, _w, cx| {
                                                this.run_fcall(fn_for_run.clone(), cx)
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

        let fn_for_toggle = SharedString::from(fn_name.clone());
        let fn_for_copy = fn_name.clone();

        v_flex()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Label::new(fn_name.clone())
                            .text_sm()
                            .font_family(get_mono_font_family())
                            .text_color(foreground),
                    )
                    .when_some(flags_text, |this, text| {
                        this.child(Label::new(text).text_xs().text_color(muted))
                    })
                    .when_some(desc, |this, d| this.child(Label::new(d).text_xs().text_color(muted)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("fn-copy-name", id_hash))
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .tooltip(i18n_functions(cx, "copy_fn_tooltip"))
                            .on_click(cx.listener(move |_, _, w, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(fn_for_copy.clone()));
                                w.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                            })),
                    )
                    .when(can_run, |this| {
                        this.child(
                            Button::new(("fn-toggle-run", id_hash))
                                .outline()
                                .small()
                                .icon(IconName::Search)
                                .label(if run_open {
                                    i18n_functions(cx, "hide_run")
                                } else {
                                    i18n_functions(cx, "run")
                                })
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.toggle_run(fn_for_toggle.clone(), w, cx)),
                                ),
                        )
                    }),
            )
            .when_some(run_block, |this, block| this.child(block))
    }

    /// Form layout: title / hint / templates / validation scroll with the
    /// code editor filling remaining height; REPLACE + Cancel/Load sit in
    /// a sticky footer that never scrolls off-screen.
    fn render_form_panel(&self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.edit_form.as_ref() else {
            return div().into_any_element();
        };
        let code_input = form.code.clone();
        let target = form.target_name.clone();
        let replace = form.replace;
        let submitting = self.submitting;
        let code_text = form.code.read(cx).value().to_string();

        let validation_banner: Option<gpui::AnyElement> = match validate_library_source(&code_text) {
            Ok(v) => {
                let name_ok: SharedString =
                    format!("{}: {}", i18n_functions(cx, "validate_ok_name"), v.library_name).into();
                let warn = v.warnings.first().map(|k| i18n_functions(cx, k));
                Some(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .bg(cx.theme().green.opacity(0.12))
                        .child(Label::new(name_ok).text_xs().text_color(cx.theme().green))
                        .when_some(warn, |this, w| {
                            this.child(Label::new(w).text_xs().text_color(cx.theme().yellow))
                        })
                        .into_any_element(),
                )
            }
            Err(err) => {
                let msg = if let crate::connection::LibraryValidateError::InvalidName(ref name) = err {
                    SharedString::from(format!("{}: {name}", i18n_functions(cx, err.i18n_key())))
                } else {
                    i18n_functions(cx, err.i18n_key())
                };
                Some(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .bg(cx.theme().red.opacity(0.12))
                        .child(Label::new(msg).text_xs().text_color(cx.theme().red))
                        .into_any_element(),
                )
            }
        };

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        let title_text = match &target {
            Some(name) => format!("{}: {}", i18n_functions(cx, "edit_title"), name),
            None => i18n_functions(cx, "new_title").to_string(),
        };

        let mut template_btns: Vec<gpui::AnyElement> = Vec::with_capacity(TEMPLATES.len());
        for tpl in TEMPLATES {
            let id = tpl.id;
            template_btns.push(
                Button::new(("fn-tpl", djb2_hash(id)))
                    .outline()
                    .small()
                    .label(i18n_functions(cx, tpl.label_key))
                    .disabled(submitting)
                    .on_click(cx.listener(move |this, _, w, cx| this.apply_template(id, w, cx)))
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
                    .child(Label::new(i18n_functions(cx, "code_hint")).text_xs().text_color(muted))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Label::new(i18n_functions(cx, "templates_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .children(template_btns),
                    )
                    .when_some(validation_banner, |this, b| this.child(b))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(200.0))
                            .w_full()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_sm()
                            .child(
                                Editor::new(&code_input)
                                    .appearance(false)
                                    .bordered(false)
                                    .h_full()
                                    .font_family(get_mono_font_family()),
                            ),
                    ),
            )
            // Sticky footer — always visible while editing.
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        Button::new("functions-replace")
                            .small()
                            .when(replace, |b| b.primary())
                            .when(!replace, |b| b.outline())
                            .label("REPLACE")
                            .on_click(cx.listener(|this, _, _w, cx| this.toggle_replace(cx))),
                    )
                    .child(
                        Label::new(i18n_functions(cx, "replace_hint"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("functions-cancel")
                            .small()
                            .outline()
                            .disabled(submitting)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_form(cx))),
                    )
                    .child(
                        Button::new("functions-submit")
                            .small()
                            .primary()
                            .disabled(submitting)
                            .label(i18n_functions(cx, "submit"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit(cx))),
                    ),
            )
            .into_any_element()
    }

    fn chip(&self, text: SharedString, color: gpui::Hsla, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let bg = color.opacity(0.18);
        let _ = cx;
        div()
            .px_2()
            .rounded_sm()
            .bg(bg)
            .child(Label::new(text).text_xs().text_color(color))
    }
}

/// Tiny stable hash so element IDs derived from library names compile
/// to `u32` (ElementId only accepts primitive tuple seconds).
fn djb2_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}
