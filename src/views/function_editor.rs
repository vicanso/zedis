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
//! Surfaces `FUNCTION LIST`, `FUNCTION LOAD` and `FUNCTION DELETE` —
//! the three commands needed to introspect, install, and remove
//! server-side functions. New library and Edit both share the same
//! Lua editor; the only difference is whether the form pre-fills with
//! the existing library code and whether `REPLACE` is on by default.

use crate::{
    assets::CustomIconName,
    connection::{Capability, FunctionLibrary, function_delete, function_list, function_load, get_connection_manager},
    error::Error,
    helpers::get_mono_font_family,
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, dialog_button_props, escalate_dangerous_body,
        i18n_common, i18n_functions,
    },
};
use ahash::{AHashMap, AHashSet};
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
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

const CODE_PREVIEW_MAX_HEIGHT: f32 = 280.0;

/// In-flight create / edit form. `target_name` is `Some(...)` when
/// editing an existing library so we know what to refresh after the
/// LOAD succeeds, plus REPLACE defaults to true.
struct EditForm {
    target_name: Option<SharedString>,
    code: Entity<InputState>,
    replace: bool,
}

pub struct ZedisFunctionEditor {
    server_state: Entity<ZedisServerState>,
    libraries: Vec<FunctionLibrary>,
    /// Library names whose code panel is currently expanded inline.
    /// Keyed by name to survive reorderings / re-fetches.
    expanded: AHashSet<SharedString>,
    /// Lazily-created read-only Lua editors for inline code previews.
    /// Created on first expand of a given library, then cached so
    /// re-expanding the same card is instant. Cleared on every fetch
    /// because the library code may have been LOAD-replaced and we
    /// want fresh content next time the panel opens.
    code_editors: AHashMap<SharedString, Entity<InputState>>,
    /// `true` when `FUNCTION LIST` reported unknown command — should
    /// not happen here in practice because the menu entry is gated on
    /// version, but keep the safety belt.
    unsupported: bool,
    edit_form: Option<EditForm>,
    error: Option<SharedString>,
    loading: bool,
    submitting: bool,
    deleting: Option<SharedString>,
    _fetch_task: Option<Task<()>>,
    _mutate_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisFunctionEditor {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| match event {
            ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated => {
                this.libraries.clear();
                this.expanded.clear();
                this.code_editors.clear();
                this.error = None;
                this.unsupported = false;
                this.fetch(cx);
            }
            _ => {}
        }));
        let mut this = Self {
            server_state,
            libraries: Vec::new(),
            expanded: AHashSet::new(),
            code_editors: AHashMap::new(),
            unsupported: false,
            edit_form: None,
            error: None,
            loading: false,
            submitting: false,
            deleting: None,
            _fetch_task: None,
            _mutate_task: None,
            _subscriptions: subscriptions,
        };
        this.fetch(cx);
        this
    }

    /// Pull the current library list. Always asks `WITHCODE` so the
    /// inline expand toggle is instant; FUNCTION LIST is cheap because
    /// it's a small in-memory table (libraries are global, not per-key).
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
                function_list(&mut conn, true).await
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(listing) => {
                        this.unsupported = listing.unsupported;
                        this.libraries = listing.libraries;
                        // Drop stale code editors so a freshly loaded
                        // (REPLACEd) library shows the new source the
                        // next time the user expands its card.
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

    /// Open the unified create/edit form. `existing` is the library to
    /// pre-fill from, or `None` for a new library. The same `EditForm`
    /// shape is used either way — the difference is target_name and
    /// the default REPLACE flag.
    fn open_form(&mut self, existing: Option<&FunctionLibrary>, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let default_value: SharedString = existing
            .and_then(|l| l.code.clone().map(SharedString::from))
            .unwrap_or_else(|| {
                SharedString::from("#!lua name=mylib\nredis.register_function('hello', function() return 'hi' end)\n")
            });
        let code = cx.new(|cx| {
            // Pass "lua" literally rather than going through
            // `Language::from_str`, which silently falls back to JSON
            // when gpui-component's `tree-sitter-languages` feature
            // isn't enabled. Lua is registered manually via
            // `register_extra_languages()` at app startup, so the
            // registry resolves the name directly.
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
                .default_value(default_value)
        });
        let target_name = existing.map(|l| SharedString::from(l.name.clone()));
        // REPLACE makes sense by default when editing (you're
        // overwriting on purpose). For a new library REPLACE=false
        // lets Redis error out if the name is already taken.
        let replace = target_name.is_some();
        self.edit_form = Some(EditForm {
            target_name,
            code,
            replace,
        });
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

    fn submit(&mut self, cx: &mut gpui::Context<Self>) {
        // Defense in depth — the form entry points are hidden without
        // FunctionWrite.
        if !self.server_state.read(cx).can(Capability::FunctionWrite) {
            return;
        }
        let Some(form) = self.edit_form.as_ref() else { return };
        let code = form.code.read(cx).value().to_string();
        if code.trim().is_empty() {
            self.error = Some(i18n_functions(cx, "code_required"));
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
            let result: Result<String> = task.await;
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
                        let result: Result<()> = task.await;
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
}

impl gpui::Render for ZedisFunctionEditor {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let header = self.render_header(cx).into_any_element();

        // Edit/New form takeover — replaces the library list while open.
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
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.libraries.len());
            for lib in &self.libraries.clone() {
                rows.push(self.render_library_card(lib.clone(), window, cx).into_any_element());
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

impl ZedisFunctionEditor {
    fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // FUNCTION LOAD / DELETE are server writes (Capability::FunctionWrite).
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
                            .tooltip(i18n_common(cx, "back_to_editor"))
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

    fn render_library_card(
        &mut self,
        lib: FunctionLibrary,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let can_write = self.server_state.read(cx).can(Capability::FunctionWrite);
        let muted = cx.theme().muted_foreground;
        let theme_blue = cx.theme().blue;
        let name = lib.name.clone();
        let name_for_id = name.clone();
        let id_hash: u32 = djb2_hash(name_for_id.as_ref());
        let expanded = self.expanded.contains(name.as_str());
        let deleting = self.deleting.as_deref() == Some(name.as_str());
        let engine_chip = self.chip(lib.engine.clone().into(), theme_blue, cx).into_any_element();
        let funcs_count = lib.functions.len();

        // Function names rendered as plain monospace labels in the
        // foreground color — readable without the visual weight of a
        // full chip. Flags follow inline in muted text as supplemental
        // metadata.
        let foreground = cx.theme().foreground;
        let mut func_chips: Vec<gpui::AnyElement> = Vec::with_capacity(funcs_count);
        for f in &lib.functions {
            let pill_name = f.name.clone();
            let flags_text = if f.flags.is_empty() {
                None
            } else {
                Some(format!(
                    "({})",
                    f.flags.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ))
            };
            func_chips.push(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Label::new(pill_name)
                            .text_sm()
                            .font_family(get_mono_font_family())
                            .text_color(foreground),
                    )
                    .when_some(flags_text, |this, text| {
                        this.child(Label::new(text).text_xs().text_color(muted))
                    })
                    .into_any_element(),
            );
        }

        // Lazily build the read-only Lua editor for this library's
        // source when the card is expanded. Materializing it before
        // composing the layout avoids a `&mut self` + `&mut cx`
        // overlap inside the builder chain.
        if expanded
            && !self.code_editors.contains_key(name.as_str())
            && let Some(code) = lib.code.as_ref()
        {
            let value = code.clone();
            let editor = cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("lua")
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
                            // Read-only Lua viewer. `disabled(true)`
                            // blocks editing while keeping syntax
                            // highlighting active — the dedicated
                            // Edit panel owns the writable path.
                            Input::new(editor)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
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
            .when(!func_chips.is_empty(), |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .px_3()
                        .pb_2()
                        .child(
                            Label::new(i18n_functions(cx, "functions_label"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .children(func_chips),
                )
            })
            .when_some(code_block, |this, block| this.child(block))
    }

    fn render_form_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.edit_form.as_ref() else {
            return div().into_any_element();
        };
        let code_input = form.code.clone();
        let target = form.target_name.clone();
        let replace = form.replace;
        let submitting = self.submitting;

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

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(Label::new(title_text).text_sm().text_color(cx.theme().foreground))
            .child(Label::new(i18n_functions(cx, "code_hint")).text_xs().text_color(muted))
            .child(
                // Big-ish editor area — Lua functions can run several
                // dozen lines, give it room.
                div()
                    .h(px(360.0))
                    .w_full()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_sm()
                    .child(
                        Input::new(&code_input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .h_full()
                            .font_family(get_mono_font_family()),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
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
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
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
/// to `u32` (ElementId only accepts primitive tuple seconds). DJB2
/// is overkill for "make this stable across re-renders" but the
/// collision domain is single-digit (#libraries on one server), so
/// the algorithm choice doesn't matter.
fn djb2_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}
