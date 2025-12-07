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

use gpui::AnyElement;
use gpui::App;
use gpui::ClickEvent;
use gpui::ElementId;
use gpui::Fill;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::button::Button;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::list::ListItem;

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Card {
    id: ElementId,
    icon: Option<Icon>,
    title: Option<String>,
    description: Option<String>,
    actions: Option<Vec<Button>>,
    on_click: Option<OnClick>,
    footer: Option<AnyElement>,
    bg: Option<Fill>,
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
            footer: None,
            bg: None,
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
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }
    pub fn bg(mut self, bg: impl Into<Fill>) -> Self {
        self.bg = Some(bg.into());
        self
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut header = h_flex();
        if let Some(icon) = self.icon {
            header = header.child(icon);
        }
        if let Some(title) = self.title {
            header = header.child(Label::new(title).ml_2().text_base().whitespace_normal());
        }
        if let Some(actions) = self.actions {
            header = header.child(h_flex().flex_1().justify_end().children(actions));
        }

        let mut item = ListItem::new(self.id)
            .m_2()
            .border(px(1.))
            .border_color(cx.theme().border)
            .p_4()
            .rounded(cx.theme().radius)
            .child(header);

        if let Some(bg) = self.bg {
            item = item.bg(bg);
        }

        if let Some(on_click) = self.on_click {
            item = item.on_click(on_click);
        }

        if let Some(description) = self.description {
            item = item.child(Label::new(description).text_sm().whitespace_normal());
        }

        if let Some(footer) = self.footer {
            item = item.child(footer);
        }

        item
    }
}
