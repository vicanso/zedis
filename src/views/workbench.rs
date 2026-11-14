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

//! Multi-tab parameterized Redis Query Workbench (`ServerView::Workbench`).

mod results;
mod snippets;
mod tab;

use crate::connection::{
    DangerKind, ServerDb, TerminalSession, classify_dangerous_line, get_server, is_write_command,
    requires_write_confirm,
};
use crate::db::WorkbenchSnippet;
use crate::helpers::get_mono_font_family;
use crate::states::{
    ServerEvent, ServerView, WriteGate, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, i18n_common,
    i18n_workbench, record_command,
};
use crate::views::{bridge_danger, confirm_dangerous_command, first_write_line, refusal};
use gpui::{AnyElement, App, Context, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, Input},
    label::Label,
    notification::Notification,
    v_flex,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tab::{WorkbenchStatementResult, WorkbenchTab};
use web_time::Instant;

pub struct ZedisWorkbench {
    server_state: Entity<ZedisServerState>,
    tabs: Vec<WorkbenchTab>,
    active_tab: usize,
    next_tab_id: u64,
    snippets: Vec<(String, Arc<WorkbenchSnippet>)>,
    show_snippets: bool,
    _subscriptions: Vec<Subscription>,
}

impl ZedisWorkbench {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                for tab in &mut this.tabs {
                    tab.reset_connection();
                }
                cx.notify();
            }
        }));

        let mut this = Self {
            server_state,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            snippets: Vec::new(),
            show_snippets: true,
            _subscriptions: subscriptions,
        };
        this.add_new_tab(window, cx);
        this.refresh_snippets(cx);
        this
    }

    pub(super) fn alloc_tab_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    pub(super) fn add_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.alloc_tab_id();
        let title: SharedString = format!("Query {id}").into();
        let tab = WorkbenchTab::new(id, title, None, &BTreeMap::new(), window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
    }

    pub(super) fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(ix);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let editor = tab.editor.clone();
            editor.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn sync_tab_params(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = BTreeMap::new();
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.sync_params(&defaults, true, window, cx);
        }
    }

    pub(super) fn stop_active_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.task = None;
            tab.running = false;
            cx.notify();
        }
    }

    pub(super) fn run_active_tab(&mut self, selection_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let lines = tab.executable_lines(selection_only, cx);
        if lines.is_empty() {
            return;
        }
        let joined = lines.join("\n");
        let server_id = self.server_state.read(cx).server_id().to_string();
        let tab_db = tab.tab_db.unwrap_or_else(|| self.server_state.read(cx).db());

        let gate = self.server_state.read(cx).write_gate();
        if gate != WriteGate::Open
            && let Some(write_line) = first_write_line(&joined)
        {
            match gate {
                WriteGate::Locked => {
                    let Ok(server) = get_server(&server_id) else {
                        return;
                    };
                    let server_state = self.server_state.clone();
                    let entity = cx.entity().downgrade();
                    confirm_dangerous_command(
                        &server,
                        tab_db,
                        &DangerKind::WriteLocked,
                        Some(&write_line),
                        window,
                        cx,
                        move |window, cx| {
                            server_state.update(cx, |state, cx| state.unlock_writes(cx));
                            let entity = entity.clone();
                            window.defer(cx, move |window, cx| {
                                if let Some(this) = entity.upgrade() {
                                    this.update(cx, |this, cx| this.run_active_tab(selection_only, window, cx));
                                }
                            });
                        },
                    );
                }
                _ => {
                    window.push_notification(Notification::warning(i18n_common(cx, "disable_in_readonly")), cx);
                }
            }
            return;
        }

        if let Ok(server) = get_server(&server_id) {
            let confirm_writes = requires_write_confirm(&server);
            let mut blocking: Option<(String, DangerKind)> = None;
            for line in &lines {
                if let Some(kind) = classify_dangerous_line(line) {
                    blocking = Some((line.clone(), kind));
                    break;
                }
                if let Some(kind) =
                    shlex::split(line).and_then(|parts| parts.first().and_then(|cmd| bridge_danger(&server, cmd)))
                {
                    blocking = Some((line.clone(), kind));
                    break;
                }
                if confirm_writes
                    && let Some(parts) = shlex::split(line)
                    && let Some(cmd_name) = parts.first()
                    && is_write_command(cmd_name, &parts[1..])
                    && refusal(cmd_name, &parts[1..]).is_none()
                {
                    blocking = Some((line.clone(), DangerKind::GenericWrite));
                    break;
                }
            }
            if let Some((line, kind)) = blocking {
                let entity = cx.entity().downgrade();
                let tab_id = tab.id;
                confirm_dangerous_command(&server, tab_db, &kind, Some(&line), window, cx, move |_, cx| {
                    if let Some(this) = entity.upgrade() {
                        this.update(cx, |this, cx| this.spawn_statements(tab_id, lines.clone(), true, cx));
                    }
                });
                return;
            }
        }

        let tab_id = tab.id;
        self.spawn_statements(tab_id, lines, false, cx);
    }

    fn spawn_statements(&mut self, tab_id: u64, lines: Vec<String>, confirmed: bool, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let default_db = server_state.db();
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        let active_db = tab.tab_db.unwrap_or(default_db);
        let mut at = ServerDb::new(server_id.as_str(), active_db);
        if confirmed {
            let token = get_server(&server_id).map(|s| s.name).unwrap_or_default();
            at = at.confirmed(token);
        }
        let session = tab.session.clone();
        tab.results.clear();
        tab.running = true;
        cx.notify();

        let task = cx.spawn(async move |handle, cx| {
            for line in lines {
                let started = Instant::now();
                let line_for_run = line.clone();
                let at_run = at.clone();
                let session_run = session.clone();
                let server_id_rec = server_id.clone();

                let outcome = cx
                    .background_spawn(async move {
                        let Some(parts) = shlex::split(&line_for_run) else {
                            return Err(("unmatched quote in command".to_string(), false));
                        };
                        let Some((cmd_name, args)) = parts.split_first() else {
                            return Err(("empty command".to_string(), false));
                        };
                        if let Some(reason) = refusal(cmd_name, args) {
                            return Err((reason, false));
                        }
                        match session_run.run(&at_run, cmd_name, args).await {
                            Ok(reply) => {
                                record_command(server_id_rec.as_str(), line_for_run.as_str());
                                Ok(reply)
                            }
                            Err(e) => Err((e.to_string(), TerminalSession::drops_link(e.connection_kind()))),
                        }
                    })
                    .await;

                let elapsed_ms = started.elapsed().as_millis();
                let (result, selected_db, link_dropped) = match outcome {
                    Ok(reply) => {
                        let db = reply.selected_db();
                        (Ok(reply), db, false)
                    }
                    Err((msg, dropped)) => (Err(SharedString::from(msg)), None, dropped),
                };

                let alive = handle.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(db) = selected_db {
                            tab.tab_db = Some(db);
                        }
                        if link_dropped {
                            tab.tab_db = None;
                        }
                        tab.results.push(WorkbenchStatementResult {
                            command: line.into(),
                            elapsed_ms,
                            outcome: result,
                        });
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }

            let _ = handle.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.running = false;
                    tab.task = None;
                    cx.notify();
                }
            });
        });

        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.task = Some(task);
        }
    }

    fn render_header(&self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .h(px(40.))
            .px_4()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("workbench-back")
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
                    .child(Label::new(i18n_workbench(cx, "title")).font_semibold()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("workbench-toggle-snippets")
                            .outline()
                            .small()
                            .icon(IconName::BookOpen)
                            .label(i18n_workbench(cx, "snippets"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.show_snippets = !this.show_snippets;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("workbench-save-snippet")
                            .outline()
                            .small()
                            .icon(IconName::Check)
                            .label(i18n_workbench(cx, "save_snippet"))
                            .on_click(cx.listener(|this, _, w, cx| this.open_save_snippet_dialog(w, cx))),
                    )
                    .child(
                        Button::new("workbench-run-selection")
                            .outline()
                            .small()
                            .icon(IconName::Play)
                            .disabled(running)
                            .label(i18n_workbench(cx, "run_selection"))
                            .on_click(cx.listener(|this, _, w, cx| this.run_active_tab(true, w, cx))),
                    )
                    .child(if running {
                        Button::new("workbench-stop")
                            .danger()
                            .small()
                            .icon(IconName::Close)
                            .label(i18n_workbench(cx, "stop"))
                            .on_click(cx.listener(|this, _, _w, cx| this.stop_active_tab(cx)))
                    } else {
                        Button::new("workbench-run-all")
                            .primary()
                            .small()
                            .icon(IconName::Play)
                            .label(i18n_workbench(cx, "run_all"))
                            .on_click(cx.listener(|this, _, w, cx| this.run_active_tab(false, w, cx)))
                    }),
            )
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let can_close = self.tabs.len() > 1;
        let active_bg = theme.muted;

        let chips: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let is_active = ix == self.active_tab;
                h_flex()
                    .id(("workbench-tab", ix))
                    .h_full()
                    .px_3()
                    .gap_1()
                    .items_center()
                    .border_r_1()
                    .border_color(border)
                    .when(is_active, |s| s.bg(active_bg))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active_tab = ix;
                        if let Some(t) = this.tabs.get(ix) {
                            let editor = t.editor.clone();
                            editor.update(cx, |s, cx| s.focus(window, cx));
                        }
                        cx.notify();
                    }))
                    .child(Label::new(tab.title.clone()).text_xs())
                    .when(can_close, |this| {
                        this.child(
                            Button::new(("workbench-tab-close", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(i18n_workbench(cx, "close_tab"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(ix, window, cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        h_flex()
            .w_full()
            .h(px(34.))
            .items_center()
            .border_b_1()
            .border_color(border)
            .children(chips)
            .child(
                div().px_2().child(
                    Button::new("workbench-new-tab")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip(i18n_workbench(cx, "new_tab"))
                        .on_click(cx.listener(|this, _, window, cx| this.add_new_tab(window, cx))),
                ),
            )
    }

    fn render_params_bar(&self, tab: &WorkbenchTab, cx: &App) -> Option<AnyElement> {
        if tab.param_names.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let fields: Vec<AnyElement> = tab
            .param_names
            .iter()
            .filter_map(|name| {
                let input = tab.param_inputs.get(name)?;
                Some(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Label::new(format!("{name}:"))
                                .text_xs()
                                .font_family(get_mono_font_family())
                                .text_color(muted),
                        )
                        .child(Input::new(input).xsmall().w(px(140.)))
                        .into_any_element(),
                )
            })
            .collect();

        Some(
            h_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .gap_3()
                .flex_wrap()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Label::new(i18n_workbench(cx, "params_label"))
                        .text_xs()
                        .text_color(muted),
                )
                .children(fields)
                .into_any_element(),
        )
    }
}

impl Render for ZedisWorkbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sync_params(&BTreeMap::new(), false, window, cx);
        }
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return div().into_any_element();
        };
        let running = tab.running;
        let editor = tab.editor.clone();
        let params_bar = self.render_params_bar(tab, cx);
        let results_pane = self.render_results_pane(tab, cx);

        v_flex()
            .size_full()
            .child(self.render_header(running, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.show_snippets, |this| this.child(self.render_snippets_sidebar(cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .h_full()
                            .child(self.render_tab_bar(cx))
                            .when_some(params_bar, |this, bar| this.child(bar))
                            .child(
                                div().h(px(220.)).w_full().flex_none().child(
                                    Editor::new(&editor)
                                        .appearance(false)
                                        .bordered(false)
                                        .size_full()
                                        .font_family(get_mono_font_family()),
                                ),
                            )
                            .child(results_pane),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    #[gpui::test]
    fn workbench_extracts_parameters_substitutes_values_and_manages_tabs(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::new());
        let mut view_slot = None;
        let handle = cx.open_window(size(px(1024.), px(768.)), |window, cx| {
            let wb = cx.new(|cx| ZedisWorkbench::new(server_state, window, cx));
            view_slot = Some(wb.clone());
            Root::new(wb, window, cx)
        });
        let view = view_slot.expect("workbench view created");
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let editor = view.read_with(cx, |wb, _| {
                assert_eq!(wb.tabs.len(), 1);
                assert_eq!(wb.tabs[0].param_names, vec!["key".to_string()]);
                wb.tabs[0].editor.clone()
            });
            editor.update(cx, |ed, cx| {
                ed.set_value("HGET user:{{uid}} {{field}}\nTTL user:{{uid}}", window, cx);
            });
        })
        .expect("update workbench script");
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let (uid_input, field_input) = view.read_with(cx, |wb, _| {
                assert_eq!(wb.tabs[0].param_names, vec!["uid".to_string(), "field".to_string()]);
                (
                    wb.tabs[0].param_inputs.get("uid").expect("uid input").clone(),
                    wb.tabs[0].param_inputs.get("field").expect("field input").clone(),
                )
            });
            uid_input.update(cx, |inp, cx| inp.set_value("42", window, cx));
            field_input.update(cx, |inp, cx| inp.set_value("email", window, cx));
            view.update(cx, |wb, cx| {
                let lines = wb.tabs[0].executable_lines(false, cx);
                assert_eq!(lines, vec!["HGET user:42 email", "TTL user:42"]);

                wb.add_new_tab(window, cx);
                assert_eq!(wb.tabs.len(), 2);
                assert_eq!(wb.active_tab, 1);
                wb.close_tab(1, window, cx);
                assert_eq!(wb.tabs.len(), 1);
            });
        })
        .expect("verify params and tabs");
    }
}
