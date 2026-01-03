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

use crate::states::{ZedisGlobalStore, i18n_settings, update_app_state_and_save};
use gpui::{Entity, Subscription, Window, prelude::*, px};
use gpui_component::{
    form::{field, v_form},
    input::{InputEvent, InputState, NumberInput},
    label::Label,
    v_flex,
};

pub struct ZedisSettingEditor {
    max_key_tree_depth_state: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisSettingEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let max_key_tree_depth = store.max_key_tree_depth();
        let max_key_tree_depth_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_settings(cx, "max_key_tree_depth_placeholder"))
                .default_value(max_key_tree_depth.to_string())
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe_in(&max_key_tree_depth_state, window, |_view, state, event, _window, cx| {
                if let InputEvent::Blur = &event {
                    let text = state.read(cx).value();
                    let value = text.parse::<i64>().unwrap_or_default();
                    update_app_state_and_save(cx, "save_max_key_tree_depth", move |state, _cx| {
                        state.set_max_key_tree_depth(value as usize);
                    });
                }
            }),
        );

        Self {
            _subscriptions: subscriptions,
            max_key_tree_depth_state,
        }
    }
}

impl Render for ZedisSettingEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_5()
            .child(Label::new(i18n_settings(cx, "title")).text_3xl().mb_2())
            .child(
                v_form().flex_1().columns(2).label_width(px(120.)).child(
                    field()
                        .label(i18n_settings(cx, "max_key_tree_depth"))
                        .child(NumberInput::new(&self.max_key_tree_depth_state)),
                ),
            )
    }
}
