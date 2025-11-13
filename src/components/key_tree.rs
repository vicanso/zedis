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

use gpui::AppContext;
use gpui::px;
use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::ActiveTheme;
use gpui_component::IconName;
use gpui_component::h_flex;
use gpui_component::list::ListItem;
use gpui_component::tree::TreeItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;

pub struct ZedisKeyTree {
    keys: Vec<String>,
    tree_state: Entity<TreeState>,
}

impl ZedisKeyTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        // tree_state.update(cx, |state, cx| {
        //     state.set_items(
        //         vec![
        //             TreeItem::new("1", "1"),
        //             TreeItem::new("2", "2"),
        //             TreeItem::new("3", "3"),
        //         ],
        //         cx,
        //     );
        // });
        Self {
            tree_state,
            keys: vec![],
        }
    }
    pub fn extend_key(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        self.keys.extend(keys);
        let items = self
            .keys
            .iter()
            .map(|key| TreeItem::new(key.to_string(), key.to_string()))
            .collect::<Vec<TreeItem>>();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }
}

impl Render for ZedisKeyTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        tree(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                view.update(cx, |_, cx| {
                    let item = entry.item();
                    let icon = if !entry.is_folder() {
                        IconName::File
                    } else if entry.is_expanded() {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    };

                    ListItem::new(ix)
                        .w_full()
                        .rounded(cx.theme().radius)
                        .py_0p5()
                        .px_2()
                        .pl(px(16.) * entry.depth() + px(8.))
                        .child(h_flex().gap_2().child(icon).child(item.label.clone()))
                        .on_click(cx.listener({
                            let item = item.clone();
                            move |_, _, _window, cx| {
                                if item.is_folder() {
                                    return;
                                }

                                // Self::open_file(
                                //     cx.entity(),
                                //     PathBuf::from(item.id.as_str()),
                                //     _window,
                                //     cx,
                                // )
                                // .ok();

                                cx.notify();
                            }
                        }))
                })
            },
        )
        .text_sm()
        .p_1()
        .bg(cx.theme().sidebar)
        .text_color(cx.theme().sidebar_foreground)
        .h_full()
    }
}
