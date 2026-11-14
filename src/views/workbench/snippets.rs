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

//! Saved snippet persistence and sidebar UI for [`ZedisWorkbench`].

use super::ZedisWorkbench;
use super::tab::WorkbenchTab;
use crate::db::{WorkbenchManager, WorkbenchSnippet};
use crate::helpers::unix_ts;
use crate::states::{dialog_button_props, i18n_common, i18n_workbench};
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::Arc;
use uuid::Uuid;
use zedis_ui::ZedisDialog;

struct SaveSnippetForm {
    name: Entity<InputState>,
    description: Entity<InputState>,
}

impl SaveSnippetForm {
    fn new(
        default_name: SharedString,
        default_desc: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_workbench(cx, "snippet_name"))
                .default_value(default_name)
        });
        let description = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_workbench(cx, "snippet_description"))
                .default_value(default_desc)
        });
        Self { name, description }
    }
}

impl Render for SaveSnippetForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_workbench(cx, "snippet_name"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&self.name).small()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_workbench(cx, "snippet_description"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&self.description).small()),
            )
    }
}

impl ZedisWorkbench {
    pub(super) fn refresh_snippets(&mut self, cx: &mut Context<Self>) {
        self.snippets = WorkbenchManager::list_with_id()
            .into_iter()
            .map(|(id, snippet)| (id, Arc::new(snippet)))
            .collect();
        cx.notify();
    }

    pub(super) fn open_save_snippet_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let target_id = tab.snippet_id.clone();
        let default_name = tab.title.clone();
        let default_desc = tab.description.clone();
        let script = tab.editor.read(cx).value().to_string();
        let default_params = tab.param_values(cx);
        let existing_created_at = target_id
            .as_deref()
            .and_then(|id| WorkbenchManager::get(id).ok())
            .map(|s| s.created_at);

        let form = cx.new(|cx| SaveSnippetForm::new(default_name, default_desc, window, cx));
        let body = form.clone();
        let name_field = form.read(cx).name.clone();
        let ok_label = i18n_common(cx, "save");
        let cancel_label = i18n_common(cx, "cancel");
        let workbench = cx.entity().downgrade();

        ZedisDialog::new(i18n_workbench(cx, "save_snippet"))
            .w(px(420.))
            .ok_text(ok_label.clone())
            .cancel_text(cancel_label.clone())
            .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
            .child(move || body.clone())
            .on_ok(move |_, window, cx| {
                let name = form.read(cx).name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return false;
                }
                let description = form.read(cx).description.read(cx).value().trim().to_string();
                let now = unix_ts();
                let id = target_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
                let snippet = WorkbenchSnippet {
                    name: name.clone(),
                    script: script.clone(),
                    params: default_params.clone(),
                    description: description.clone(),
                    created_at: existing_created_at.unwrap_or(now),
                    updated_at: now,
                };
                if let Err(e) = WorkbenchManager::upsert(&id, snippet) {
                    window.push_notification(Notification::error(e.to_string()), cx);
                    return false;
                }
                if let Some(this) = workbench.upgrade() {
                    this.update(cx, |this, cx| {
                        if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                            tab.snippet_id = Some(id.clone());
                            tab.title = name.into();
                            tab.description = description.into();
                        }
                        this.refresh_snippets(cx);
                    });
                }
                true
            })
            .open(window, cx);
        name_field.update(cx, |state, cx| state.focus(window, cx));
    }

    pub(super) fn open_snippet(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.tabs.iter().position(|t| t.snippet_id.as_deref() == Some(&id)) {
            self.active_tab = pos;
            cx.notify();
            return;
        }
        let Ok(snippet) = WorkbenchManager::get(&id) else {
            return;
        };
        let tab_id = self.alloc_tab_id();
        let mut tab = WorkbenchTab::new(
            tab_id,
            snippet.name.clone().into(),
            Some(&snippet.script),
            &snippet.params,
            window,
            cx,
        );
        tab.snippet_id = Some(id);
        tab.description = snippet.description.into();
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
    }

    pub(super) fn delete_snippet(&mut self, id: String, cx: &mut Context<Self>) {
        if WorkbenchManager::delete(&id).is_ok() {
            for tab in &mut self.tabs {
                if tab.snippet_id.as_deref() == Some(&id) {
                    tab.snippet_id = None;
                }
            }
            self.refresh_snippets(cx);
        }
    }

    pub(super) fn render_snippets_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let hover_bg = theme.list_hover;

        let rows: Vec<AnyElement> = if self.snippets.is_empty() {
            vec![
                div()
                    .p_3()
                    .child(
                        Label::new(i18n_workbench(cx, "no_snippets"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .into_any_element(),
            ]
        } else {
            self.snippets
                .iter()
                .enumerate()
                .map(|(ix, (id, snippet))| {
                    let id_open = id.clone();
                    let id_del = id.clone();
                    let desc = snippet.description.clone();
                    h_flex()
                        .id(("workbench-snippet-row", ix))
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(border)
                        .hover(move |s| s.bg(hover_bg))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_snippet(id_open.clone(), window, cx);
                        }))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(Label::new(snippet.name.clone()).text_xs().text_ellipsis())
                                .when(!desc.is_empty(), |this| {
                                    this.child(Label::new(desc).text_xs().text_color(muted).text_ellipsis())
                                }),
                        )
                        .child(
                            Button::new(("workbench-snippet-delete", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Delete)
                                .tooltip(i18n_workbench(cx, "delete_snippet"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    cx.stop_propagation();
                                    this.delete_snippet(id_del.clone(), cx);
                                })),
                        )
                        .into_any_element()
                })
                .collect()
        };

        v_flex()
            .w(px(230.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .h(px(34.))
                    .px_3()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border)
                    .child(Label::new(i18n_workbench(cx, "snippets")).text_xs().font_semibold()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .children(rows),
            )
    }
}
