// Copyright 2025 Tree xie.
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

use crate::states::Route;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_content;
use crate::states::save_app_state;
use crate::views::ZedisEditor;
use crate::views::ZedisKeyTree;
use crate::views::ZedisServers;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::label::Label;
use gpui_component::resizable::ResizableState;
use gpui_component::resizable::h_resizable;
use gpui_component::resizable::resizable_panel;
use gpui_component::skeleton::Skeleton;
use gpui_component::v_flex;
use tracing::debug;
use tracing::error;
use tracing::info;

pub struct ZedisContent {
    server_state: Entity<ZedisServerState>,
    servers: Option<Entity<ZedisServers>>,
    value_editor: Option<Entity<ZedisEditor>>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisContent {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.observe(
            &cx.global::<ZedisGlobalStore>().state(),
            |this, model, cx| {
                let route = model.read(cx).route();
                if route != Route::Home && this.servers.is_some() {
                    debug!("remove servers view");
                    let _ = this.servers.take();
                }
                if route != Route::Editor && this.value_editor.is_some() {
                    debug!("remove value editor view");
                    let _ = this.value_editor.take();
                }
                cx.notify();
            },
        ));

        Self {
            server_state,
            servers: None,
            value_editor: None,
            key_tree: None,
            _subscriptions: subscriptions,
        }
    }
    fn render_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let servers = if let Some(servers) = &self.servers {
            servers.clone()
        } else {
            debug!("new servers view");
            let servers = cx.new(|cx| ZedisServers::new(window, cx, self.server_state.clone()));
            self.servers = Some(servers.clone());
            servers
        };
        div().m_4().child(servers)
    }
    fn render_loading(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .gap_2()
                    .w(px(600.0))
                    .child(Skeleton::new().w(px(600.)).h_4().rounded_md())
                    .child(Skeleton::new().w(px(100.)).h_4().rounded_md())
                    .child(Skeleton::new().w(px(220.)).h_4().rounded_md())
                    .child(Skeleton::new().w(px(420.)).h_4().rounded_md())
                    .child(Skeleton::new().w(px(600.)).h_4().rounded_md())
                    .child(
                        Label::new(i18n_content(cx, "loading"))
                            .w_full()
                            .text_color(cx.theme().muted_foreground)
                            .mt_2()
                            .text_align(gpui::TextAlign::Center),
                    ),
            )
    }
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.clone();
        let value_editor = if let Some(value_editor) = &self.value_editor {
            value_editor.clone()
        } else {
            let value_editor = cx.new(|cx| ZedisEditor::new(window, cx, server_state.clone()));
            self.value_editor = Some(value_editor.clone());
            value_editor
        };
        let key_tree = if let Some(key_tree) = &self.key_tree {
            key_tree.clone()
        } else {
            debug!("new key tree view");
            let key_tree = cx.new(|cx| ZedisKeyTree::new(window, cx, server_state));
            self.key_tree = Some(key_tree.clone());
            key_tree
        };
        let mut key_tree_width = cx.global::<ZedisGlobalStore>().read(cx).key_tree_width();
        let min_width = px(275.);
        if key_tree_width < min_width {
            key_tree_width = min_width;
        }
        h_resizable("editor-container")
            .child(
                resizable_panel()
                    .size(key_tree_width)
                    .size_range(min_width..px(400.))
                    .child(key_tree),
            )
            .child(resizable_panel().child(value_editor))
            .on_resize(
                cx.listener(move |_this, event: &Entity<ResizableState>, _window, cx| {
                    let Some(width) = event.read(cx).sizes().first() else {
                        return;
                    };
                    let mut value = cx.global::<ZedisGlobalStore>().value(cx);
                    value.set_key_tree_width(*width);
                    cx.background_spawn(async move {
                        if let Err(e) = save_app_state(&value) {
                            error!(error = %e, "save key tree width fail",);
                        } else {
                            info!("save key tree width success");
                        }
                    })
                    .detach();
                }),
            )
    }
}

impl Render for ZedisContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = cx.global::<ZedisGlobalStore>().read(cx).route();
        if route == Route::Home {
            return self.render_servers(window, cx).into_any_element();
        }
        let server_state = self.server_state.read(cx);
        if server_state.is_busy() {
            return self.render_loading(window, cx).into_any_element();
        }
        self.render_editor(window, cx).into_any_element()
    }
}
