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

use super::{TEMPLATES, ZedisLuaScriptLibrary};
use crate::assets::CustomIconName;
use crate::connection::{Capability, KillTarget, floors, max_keys_index, script_sha1};
use crate::db::{LuaScript, LuaScriptManager};
use crate::helpers::{
    debugs_lua_scripts, get_mono_font_family, kills_runaway_scripts, parse_lines, reads_clipboard_on_demand,
};
use crate::states::{
    ServerView, ZedisGlobalStore, account_is_read_only, back_to_editor_tooltip, i18n_common, i18n_lua_scripts,
};
use crate::views::unavailable_chip;
use gpui::{AnyElement, ClipboardItem, Context, Render, SharedString, Window, div, prelude::*, px};
#[cfg(target_family = "wasm")]
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, Input, Textarea},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::Arc;

/// SHA prefix length shown in the card header.
const SHA_PREVIEW_CHARS: usize = 12;

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

impl Render for ZedisLuaScriptLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let muted = cx.theme().muted_foreground;
        let header = self.render_header(cx).into_any_element();

        if let Some(debugger) = self.debugger.clone() {
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(debugger)
                .into_any_element();
        }

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
        let body: AnyElement = if self.scripts.is_empty() {
            v_flex()
                .items_center()
                .justify_center()
                .size_full()
                .gap_3()
                .child(Label::new(i18n_lua_scripts(cx, "empty")).text_color(muted))
                .when(!account_is_read_only(), |this| {
                    this.child(
                        Button::new("lua-empty-new")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label(i18n_lua_scripts(cx, "new_script"))
                            .on_click(cx.listener(|this, _, w, cx| this.open_form(None, w, cx))),
                    )
                })
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
            let mut rows: Vec<AnyElement> = Vec::with_capacity(filtered.len());
            for (id, script) in filtered {
                rows.push(self.render_card(id, script, window, cx).into_any_element());
            }
            v_flex().gap_2().p_3().w_full().children(rows).into_any_element()
        };

        let error_banner: Option<AnyElement> = self.error.as_ref().map(|e| {
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
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        // A read-only bridge account uses the library and changes none of it.
        let may_edit = !account_is_read_only();
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
                    // Import reads the clipboard at the press, which a page
                    // may not (`reads_clipboard_on_demand`).
                    .when(may_edit && reads_clipboard_on_demand(), |this| {
                        this.child(
                            Button::new("lua-import")
                                .ghost()
                                .small()
                                .label(i18n_lua_scripts(cx, "import"))
                                .tooltip(i18n_lua_scripts(cx, "import_tooltip"))
                                .on_click(cx.listener(|this, _, w, cx| this.import_from_clipboard(w, cx))),
                        )
                    })
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
                    .when(kills_runaway_scripts(), |this| {
                        this.child(
                            Button::new("lua-kill")
                                .ghost()
                                .small()
                                .label(i18n_common(cx, "kill_script_button"))
                                .tooltip(i18n_common(cx, "kill_script_tooltip"))
                                .on_click(cx.listener(|this, _, _w, cx| {
                                    this.server_state
                                        .update(cx, |state, cx| state.kill_running_script(KillTarget::Script, cx));
                                })),
                        )
                    })
                    .when(may_edit, |this| {
                        this.child(
                            Button::new("lua-new")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label(i18n_lua_scripts(cx, "new_script"))
                                .on_click(cx.listener(|this, _, w, cx| this.open_form(None, w, cx))),
                        )
                    })
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

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
        script: Arc<LuaScript>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        // A read-only bridge account runs scripts and changes none of them.
        let may_edit = !account_is_read_only();
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
        let id_debug = id.clone();
        let script_for_edit = script.clone();
        let id_for_edit = id.clone();
        let name_for_delete = SharedString::from(script.name.clone());
        let script_for_copy = script.clone();

        let code_block: Option<AnyElement> = if code_expanded {
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
                            .readonly(true)
                            .h_full()
                            .w_full()
                            .font_family(get_mono_font_family()),
                    )
                    .into_any_element()
            })
        } else {
            None
        };

        let run_block: Option<AnyElement> = if run_expanded {
            self.run_forms.get(&id).map(|form| {
                let result_block: Option<AnyElement> = form.last.clone().map(|res| {
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
                let id_ro = id.clone();
                let readonly = form.readonly;
                let supports_ro = self.server_state.read(cx).supports(floors::EVAL_RO);
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
                                    .when(supports_ro, |this| {
                                        this.child(
                                            Button::new(("lua-ro", card_hash))
                                                .small()
                                                .outline()
                                                .selected(readonly)
                                                .label(if readonly { "EVALSHA_RO" } else { "EVALSHA" })
                                                .tooltip(i18n_lua_scripts(cx, "evalsha_ro_tooltip"))
                                                .on_click(cx.listener(move |this, _, _w, cx| {
                                                    this.toggle_run_readonly(id_ro.clone(), cx)
                                                })),
                                        )
                                    })
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
                                    .when(may_edit, |this| {
                                        this.child(
                                            Button::new(("lua-save-defaults", card_hash))
                                                .outline()
                                                .small()
                                                .label(i18n_lua_scripts(cx, "save_defaults"))
                                                .tooltip(i18n_lua_scripts(cx, "save_defaults_tooltip"))
                                                .on_click(cx.listener(move |this, _, _w, cx| {
                                                    this.save_run_as_defaults(id_save_defaults.clone(), cx)
                                                })),
                                        )
                                    }),
                            ),
                    )
                    .when_some(result_block, |this, block| this.child(block))
                    .into_any_element()
            })
        } else {
            None
        };

        let cache_chip: Option<AnyElement> = cached.map(|ok| {
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
                    // Only ever set in the browser, for a script every
                    // account sees (ADR 17).
                    .when(script.shared, |this| {
                        this.child(
                            Label::new(i18n_lua_scripts(cx, "shared_badge"))
                                .text_xs()
                                .text_color(muted),
                        )
                    })
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
                                cx.write_to_clipboard(ClipboardItem::new_string(script_for_copy.code.clone()));
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
                        .when(debugs_lua_scripts(), |this| {
                            this.child(
                                Button::new(("lua-debug", card_hash))
                                    .outline()
                                    .small()
                                    .icon(IconName::Inspector)
                                    .label(i18n_lua_scripts(cx, "debug"))
                                    .tooltip(i18n_lua_scripts(cx, "debug_tooltip"))
                                    .on_click(cx.listener(move |this, _, w, cx| this.open_debugger(&id_debug, w, cx))),
                            )
                        })
                    })
                    .when(may_edit, |this| {
                        this.child(
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
                                    let pair = (id_for_edit.clone(), LuaScript::clone(&script_for_edit));
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
                        )
                    }),
            )
            .when_some(run_block, |this, block| this.child(block))
            .when_some(code_block, |this, block| this.child(block))
    }

    /// The page's *Shared* box: on the bridge a script is the saver's unless
    /// it is shared with every account (ADR 17). The desktop has no accounts.
    #[cfg(not(target_family = "wasm"))]
    fn shared_box(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    #[cfg(target_family = "wasm")]
    fn shared_box(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = self.edit_form.as_ref()?;
        Some(
            Checkbox::new("lua-shared")
                .checked(form.shared)
                .label(i18n_lua_scripts(cx, "shared"))
                .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    if let Some(form) = this.edit_form.as_mut() {
                        form.shared = *checked;
                        cx.notify();
                    }
                }))
                .into_any_element(),
        )
    }

    fn render_form_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            SharedString::from(script_sha1(&code_text))
        };
        let needed = max_keys_index(&code_text);
        let name_dup = LuaScriptManager::name_taken(name_text.trim(), form.target_id.as_deref());

        let mut hints: Vec<AnyElement> = Vec::new();
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

        let error_banner: Option<AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        let mut template_btns: Vec<AnyElement> = Vec::with_capacity(TEMPLATES.len());
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
                    .children(self.shared_box(cx))
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
