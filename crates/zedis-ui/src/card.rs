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

use gpui::{AnyElement, App, ClickEvent, ElementId, Fill, Hsla, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable, StyledExt, button::Button, h_flex, label::Label, list::ListItem, tooltip::Tooltip,
    v_flex,
};

/// Type alias for the click handler closure.
type ZedisCardOnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Side of the square the card's icon sits in.
const ICON_BLOCK: f32 = 24.;

/// Visual role of a card.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CardVariant {
    /// A real data entity (a configured server). Solid border,
    /// two rows: name, tag and actions over address, time and
    /// description.
    #[default]
    Entity,
    /// An action entry point (e.g. "Add New", "Import"). Dashed
    /// border + hover background change + center-aligned content so
    /// it reads as a placeholder/affordance rather than data. In this
    /// variant `actions`, `hover_only_actions`, `meta` and `trailing` are not
    /// rendered — the whole card is the single click target.
    Action,
}

/// A customizable Card component used to display grouped content.
///
/// It supports an icon, title, description, action buttons, a time stamp,
/// and custom background styling. It wraps a `ListItem` to provide standard
/// interactive behaviors.
#[derive(IntoElement)]
pub struct ZedisCard {
    /// Unique identifier for the element.
    id: ElementId,
    /// Optional leading icon.
    icon: Option<Icon>,
    /// Main title text (rendered bold/primary).
    title: Option<SharedString>,
    /// Secondary line under the title — smaller, muted, optionally
    /// monospace. Used for the host:port address so it visually
    /// separates from the human-readable name.
    subtitle: Option<SharedString>,
    /// Font family for the subtitle (e.g. a monospace family). The
    /// platform-correct family lives in the app crate, so the caller
    /// passes it in rather than this crate hard-coding one.
    subtitle_font: Option<SharedString>,
    /// Secondary description text.
    description: Option<SharedString>,
    /// Optional tag chip rendered in the header row (e.g. "PROD").
    /// The label and its resolved `(background, foreground)` colors are
    /// supplied by the caller — this crate has no access to the app's
    /// tag-color presets. `None` colors fall back to the muted theme
    /// token.
    tag: Option<(SharedString, Option<(Hsla, Hsla)>)>,
    /// List of action buttons to display in the header.
    actions: Option<Vec<Button>>,
    /// Action buttons that are only visible while the card is hovered.
    /// Rendered in the same action row as `actions`, just to the left.
    /// Useful for low-priority/cluttery controls (reorder arrows,
    /// pinning) that shouldn't take visual weight at rest.
    hover_only_actions: Option<Vec<Button>>,
    /// Handler for click events.
    on_click: Option<ZedisCardOnClick>,
    /// An element after the subtitle on the detail row — a time stamp.
    meta: Option<AnyElement>,
    /// An element at the end of the title row that is always there — the
    /// card's own buttons, which a hover must not be needed to find.
    trailing: Option<AnyElement>,
    /// Custom background fill.
    bg: Option<Fill>,
    /// Visual role (entity vs action). See [`CardVariant`].
    variant: CardVariant,
}
impl ZedisCard {
    /// Creates a new `Card` with the given element ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            title: None,
            subtitle: None,
            subtitle_font: None,
            description: None,
            tag: None,
            actions: None,
            hover_only_actions: None,
            on_click: None,
            meta: None,
            trailing: None,
            bg: None,
            variant: CardVariant::default(),
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

    /// Sets the subtitle (second title line — host:port, etc.).
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Sets the subtitle font family (pass a monospace family for
    /// addresses). No-op unless `subtitle` is also set.
    pub fn subtitle_font(mut self, family: impl Into<SharedString>) -> Self {
        self.subtitle_font = Some(family.into());
        self
    }

    /// Sets the description text displayed below the header.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets a colored tag chip shown in the header row. An empty
    /// `label` is treated as "no tag". `colors` is the
    /// `(background, foreground)` pair resolved by the caller
    /// (preset → HSLA for the active theme mode); `None` falls back to
    /// the muted token.
    pub fn tag(mut self, label: impl Into<SharedString>, colors: Option<(Hsla, Hsla)>) -> Self {
        let label = label.into();
        if !label.is_empty() {
            self.tag = Some((label, colors));
        }
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

    /// Sets the element shown after the subtitle on the detail row.
    pub fn meta(mut self, meta: impl IntoElement) -> Self {
        self.meta = Some(meta.into_any_element());
        self
    }

    /// Sets the always-visible element at the end of the title row.
    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_any_element());
        self
    }

    /// Overrides the default background color/fill.
    pub fn bg(mut self, bg: impl Into<Fill>) -> Self {
        self.bg = Some(bg.into());
        self
    }

    /// Sets the card's visual role. See [`CardVariant`].
    pub fn variant(mut self, variant: CardVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Shorthand for `.variant(CardVariant::Action)` — dashed border,
    /// hover background, centered content.
    pub fn action(mut self) -> Self {
        self.variant = CardVariant::Action;
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

        // Action cards read as an affordance, not data: dashed
        // border, hover background, center-aligned content. Built as
        // a plain stateful div (not ListItem) because ListItem does
        // not impl InteractiveElement, so it can't take `.hover(..)`.
        if self.variant == CardVariant::Action {
            return div()
                .id(self.id)
                .m_2()
                .p_4()
                .border(px(1.))
                .border_dashed()
                // muted_foreground (secondary-text tone) instead of
                // the near-invisible hairline `border` color so the
                // dashes actually read as a placeholder outline.
                .border_color(cx.theme().muted_foreground)
                .rounded(cx.theme().radius)
                .when_some(self.bg, |this, bg| this.bg(bg))
                .hover(|s| s.bg(cx.theme().list_active))
                .cursor_pointer()
                .when_some(self.on_click, |this, handler| {
                    this.on_click(move |event, window, cx| handler(event, window, cx))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .w_full()
                        .when_some(self.icon, |this, icon| this.child(icon))
                        .when_some(self.title, |this, title| {
                            this.child(Label::new(title).text_base().text_center())
                        })
                        .when_some(self.description, |this, description| {
                            this.child(
                                Label::new(description)
                                    .text_sm()
                                    .text_center()
                                    .whitespace_normal()
                                    .text_color(cx.theme().muted_foreground),
                            )
                        }),
                )
                .into_any_element();
        }

        let hover_only_actions = self.hover_only_actions;
        let muted = cx.theme().muted_foreground;
        // Two rows. The first is what a card is found by — the name and its
        // environment — with the actions at its end; the second is the
        // detail, all on one line: address, when it was last used, and the
        // description as far as it fits (whole in its tooltip). The card
        // used to be four rows tall with a blank one where there was no
        // description, and a screen showed four of them.
        let title_row = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            // Leading icon in a bordered, subtly-filled rounded square so it
            // reads as an "avatar" rather than a loose glyph.
            .when_some(self.icon, |this, icon| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(ICON_BLOCK))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .child(icon.with_size(px(14.))),
                )
            })
            .when_some(self.title, |this, title| {
                // The name sizes to its content yet truncates when long; the
                // chip is `flex_none` so it always stays beside the name.
                this.child(
                    div().flex_initial().min_w_0().overflow_hidden().child(
                        Label::new(title)
                            .text_sm()
                            .font_semibold()
                            .whitespace_nowrap()
                            .text_ellipsis(),
                    ),
                )
            })
            .when_some(self.tag, |row, (label, colors)| {
                let (bg, fg) = colors.unwrap_or((Hsla { a: 0.15, ..muted }, muted));
                row.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded_full()
                        .bg(bg)
                        .child(Label::new(label).text_xs().font_semibold().text_color(fg)),
                )
            })
            .child(div().flex_1())
            // Hover-only actions keep their box while invisible, so showing
            // them does not move what is beside them.
            .when_some(hover_only_actions, |this, actions| {
                this.child(
                    h_flex()
                        .flex_none()
                        .invisible()
                        .group_hover(CARD_GROUP, |s| s.visible())
                        .children(actions),
                )
            })
            .when_some(self.actions, |this, actions| {
                this.child(h_flex().flex_none().children(actions))
            })
            .when_some(self.trailing, |this, trailing| {
                this.child(div().flex_none().child(trailing))
            });

        let separator = || Label::new("·").text_xs().text_color(muted).flex_none();
        let subtitle_font = self.subtitle_font;
        let has_subtitle = self.subtitle.is_some();
        let has_meta = self.meta.is_some();
        let detail_row = h_flex()
            .w_full()
            .items_center()
            .gap_1p5()
            // Under the name, past the icon block.
            .pl(px(ICON_BLOCK + 8.))
            .when_some(self.subtitle, |row, subtitle| {
                let mut label = Label::new(subtitle)
                    .text_xs()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(muted);
                if let Some(family) = subtitle_font {
                    label = label.font_family(family);
                }
                row.child(div().flex_initial().min_w_0().overflow_hidden().child(label))
            })
            .when_some(self.meta, |row, meta| {
                row.when(has_subtitle, |row| row.child(separator()))
                    .child(div().flex_none().child(meta))
            })
            .when_some(self.description, |row, description| {
                let full = description.clone();
                row.when(has_subtitle || has_meta, |row| row.child(separator())).child(
                    div()
                        .id("zedis-card-description")
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            Label::new(description)
                                .text_xs()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(muted),
                        )
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx)),
                )
            });

        let card = ListItem::new(self.id)
            .m_1()
            .cursor_pointer()
            .border(px(1.))
            .border_color(cx.theme().border)
            .p(px(10.))
            .rounded(cx.theme().radius)
            .when_some(self.bg, |this, bg| this.bg(bg))
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(v_flex().w_full().gap_1().child(title_row).child(detail_row));

        div().group(CARD_GROUP).child(card).into_any_element()
    }
}
