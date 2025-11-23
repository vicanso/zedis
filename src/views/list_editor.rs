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

use crate::states::ZedisServerState;
use gpui::Entity;
use gpui::TextAlign;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui::uniform_list;
use gpui_component::ActiveTheme;
use gpui_component::Colorize;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::list::ListItem;

pub struct ZedisListEditor {
    server_state: Entity<ZedisServerState>,
}

impl ZedisListEditor {
    pub fn new(
        _window: &mut Window,
        _cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self { server_state }
    }
}

impl Render for ZedisListEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(data) = server_state.value().and_then(|v| v.list_value()) else {
            return div().into_any_element();
        };
        let data = data.clone();
        let size = data.1.len();
        let bg_color = cx.theme().background;

        uniform_list(
            "zedis-editor-list",
            size,
            move |visible_range, _window, _cx| {
                let mut children = vec![];
                let index_word_count = visible_range.clone().last().unwrap_or(2) + 1;
                let index_width = index_word_count.to_string().len().max(3) as f32 * 16.;

                for ix in visible_range {
                    let bg = if ix % 2 == 0 {
                        bg_color
                    } else {
                        bg_color.lighten(1.0)
                    };
                    children.push(
                        ListItem::new(("zedis-editor-list-item", ix))
                            .h(px(40.))
                            .w_full()
                            .bg(bg)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Label::new((ix + 1).to_string())
                                            .w(px(index_width))
                                            .text_align(TextAlign::Right)
                                            .text_sm()
                                            .px_2(),
                                    )
                                    .child(Label::new(data.1[ix].clone()).text_sm()),
                            ),
                    );
                }
                children
            },
        )
        .h_full()
        .w_full()
        .into_any_element()
    }
}
