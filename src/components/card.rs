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

use gpui::App;
use gpui::ClickEvent;
use gpui::Context;
use gpui::ElementId;
use gpui::InteractiveElement;
use gpui::ParentElement;
use gpui::Render;
use gpui::RenderOnce;
use gpui::Styled;
use gpui::Window;
use gpui::div;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::v_flex;
use gpui_macros::IntoElement;

#[derive(IntoElement)]
pub struct Card {
    id: ElementId,
    icon: Option<Icon>,
    title: Option<String>,
    description: Option<String>,
    actions: Option<Vec<Button>>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl Card {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id: ElementId = id.into();
        Self {
            id,
            icon: None,
            title: None,
            description: None,
            actions: None,
            on_click: None,
        }
    }
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
    pub fn actions(mut self, actions: impl Into<Vec<Button>>) -> Self {
        self.actions = Some(actions.into());
        self
    }
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Card {
    fn render(self, window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let mut header = h_flex();
        if let Some(icon) = self.icon {
            header = header.child(icon);
        }
        if let Some(title) = self.title {
            header = header.child(Label::new(title).ml_2().text_sm().whitespace_normal());
        }
        if let Some(actions) = self.actions {
            header = header.child(
                h_flex()
                    .flex_1()
                    .justify_end()
                    .children(actions.into_iter()),
            );
        }

        let mut item = ListItem::new(self.id)
            .m_2()
            .border(px(1.))
            .border_color(cx.theme().border)
            .p_2()
            .rounded(cx.theme().radius)
            .child(header);

        if let Some(on_click) = self.on_click {
            item = item.on_click(on_click);
        }

        item
    }
}
