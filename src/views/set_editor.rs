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

use crate::assets::CustomIconName;
use crate::helpers::fast_contains_ignore_case;
use crate::states::ZedisGlobalStore;
use crate::states::i18n_list_editor;
use crate::states::{RedisListValue, ZedisServerState};
use gpui::App;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::radio::RadioGroup;
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::{Disableable, IndexPath};
use gpui_component::{Icon, IconName};
use gpui_component::{WindowExt, h_flex};
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::info;

pub struct ZedisSetEditor {
    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,
}
impl ZedisSetEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, server_state: Entity<ZedisServerState>) -> Self {
        Self { server_state }
    }
}
impl Render for ZedisSetEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w_full()
            .child(h_flex().w_full().px_2().py_1().child(Label::new("Set Editor")))
    }
}
