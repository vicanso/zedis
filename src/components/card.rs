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

use gpui::{AnyElement, App, ClickEvent, ElementId, Fill, SharedString, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, Icon, button::Button, h_flex, label::Label, list::ListItem};

/// Type alias for the click handler closure.
type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A customizable Card component used to display grouped content.
///
/// It supports an icon, title, description, action buttons, a footer,
/// and custom background styling. It wraps a `ListItem` to provide standard
/// interactive behaviors.
#[derive(IntoElement)]
pub struct Card {
    /// Unique identifier for the element.
    id: ElementId,
    /// Optional leading icon.
    icon: Option<Icon>,
    /// Main title text.
    title: Option<SharedString>,
    /// Secondary description text.
    description: Option<SharedString>,
    /// List of action buttons to display in the header.
    actions: Option<Vec<Button>>,
    /// Handler for click events.
    on_click: Option<OnClick>,
    /// Optional footer element.
    footer: Option<AnyElement>,
    /// Custom background fill.
    bg: Option<Fill>,
}
impl Card {
    /// Creates a new `Card` with the given element ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            title: None,
            description: None,
            actions: None,
            on_click: None,
            footer: None,
            bg: None,
        }
    }

    /// Sets the leading icon for the card.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Sets the title text.
    /// Accepts any type that can be converted into a `SharedString`.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the description text displayed below the header.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the action buttons displayed on the right side of the header.
    pub fn actions(mut self, actions: impl Into<Vec<Button>>) -> Self {
        self.actions = Some(actions.into());
        self
    }

    /// Sets the click event handler for the card.
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// Sets a custom footer element at the bottom of the card.
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Overrides the default background color/fill.
    pub fn bg(mut self, bg: impl Into<Fill>) -> Self {
        self.bg = Some(bg.into());
        self
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Construct the header row: Icon + Title + Spacer + Actions
        let header = h_flex()
            .when_some(self.icon, |this, icon| this.child(icon))
            .when_some(self.title, |this, title| {
                this.child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(title).ml_2().text_base().whitespace_nowrap().text_ellipsis()),
                )
            })
            // Use flex_1 to push actions to the right
            .when_some(self.actions, |this, actions| {
                this.child(h_flex().flex_shrink_0().justify_end().children(actions))
            });

        // Construct the main card container using a declarative style
        ListItem::new(self.id)
            .m_2()
            .border(px(1.))
            .border_color(cx.theme().border)
            .p_4()
            .rounded(cx.theme().radius)
            // Apply custom background if provided
            .when_some(self.bg, |this, bg| this.bg(bg))
            // Attach click handler if provided
            .when_some(self.on_click, |this, handler| this.on_click(handler))
            // Add Header
            .child(header)
            // Add Description
            .when_some(self.description, |this, description| {
                this.child(Label::new(description).text_sm().whitespace_normal())
            })
            // Add Footer
            .when_some(self.footer, |this, footer| this.child(footer))
    }
}
