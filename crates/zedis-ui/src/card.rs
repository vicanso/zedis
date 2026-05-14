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
type ZedisCardOnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A customizable Card component used to display grouped content.
///
/// It supports an icon, title, description, action buttons, a footer,
/// and custom background styling. It wraps a `ListItem` to provide standard
/// interactive behaviors.
#[derive(IntoElement)]
pub struct ZedisCard {
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
    /// Action buttons that are only visible while the card is hovered.
    /// Rendered in the same action row as `actions`, just to the left.
    /// Useful for low-priority/cluttery controls (reorder arrows,
    /// pinning) that shouldn't take visual weight at rest.
    hover_only_actions: Option<Vec<Button>>,
    /// Handler for click events.
    on_click: Option<ZedisCardOnClick>,
    /// Optional footer element.
    footer: Option<AnyElement>,
    /// Custom background fill.
    bg: Option<Fill>,
}
impl ZedisCard {
    /// Creates a new `Card` with the given element ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            title: None,
            description: None,
            actions: None,
            hover_only_actions: None,
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

    /// Sets action buttons that only appear while the card is hovered.
    /// Rendered to the left of the always-visible `actions` in the
    /// header row.
    pub fn hover_only_actions(mut self, actions: impl Into<Vec<Button>>) -> Self {
        self.hover_only_actions = Some(actions.into());
        self
    }

    /// Sets the click event handler for the card.
    pub fn on_click(mut self, handler: ZedisCardOnClick) -> Self {
        self.on_click = Some(handler);
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

impl RenderOnce for ZedisCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Shared name across every card. Hover detection finds the
        // nearest matching ancestor, which is always the containing
        // card's outer ListItem (cards never nest), so cards do not
        // bleed into each other's hover state.
        const CARD_GROUP: &str = "zedis-card";

        let hover_only_actions = self.hover_only_actions;
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
            // Hover-only actions render in their own wrapper so the
            // invisibility toggle does not collapse layout — the
            // wrapper keeps its width.
            .when_some(hover_only_actions, |this, actions| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .justify_end()
                        .invisible()
                        .group_hover(CARD_GROUP, |s| s.visible())
                        .children(actions),
                )
            })
            // Use flex_1 to push actions to the right
            .when_some(self.actions, |this, actions| {
                this.child(h_flex().flex_shrink_0().justify_end().children(actions))
            });

        // Wrap the ListItem in a thin div that owns the hover group.
        // ListItem itself does not impl InteractiveElement, so we
        // attach `.group(...)` to an outer wrapper. The hover-only
        // actions above resolve their nearest ancestor with that
        // group name — which is always this card's wrapper, never a
        // sibling card.
        let card = ListItem::new(self.id)
            .m_2()
            .border(px(1.))
            .border_color(cx.theme().border)
            .p_4()
            .rounded(cx.theme().radius)
            // Apply custom background if provided
            .when_some(self.bg, |this, bg| this.bg(bg))
            // Attach click handler if provided
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            // Add Header
            .child(header)
            // Always render the description slot — fall back to a
            // non-breaking space so cards without a description still
            // reserve one line of height. Keeps grid rows visually
            // aligned across mixed has-description / no-description
            // entries.
            .child(
                Label::new(self.description.unwrap_or_else(|| SharedString::from("\u{00A0}")))
                    .text_sm()
                    .whitespace_normal(),
            )
            // Add Footer
            .when_some(self.footer, |this, footer| this.child(footer));

        div().group(CARD_GROUP).child(card)
    }
}
