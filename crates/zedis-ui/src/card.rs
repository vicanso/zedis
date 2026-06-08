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
use gpui_component::{ActiveTheme, Icon, StyledExt, button::Button, h_flex, label::Label, list::ListItem, v_flex};

/// Type alias for the click handler closure.
type ZedisCardOnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Visual role of a card.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CardVariant {
    /// A real data entity (a configured server). Solid border,
    /// header-left layout, supports actions / hover-only actions /
    /// footer.
    #[default]
    Entity,
    /// An action entry point (e.g. "Add New", "Import"). Dashed
    /// border + hover background change + center-aligned content so
    /// it reads as a placeholder/affordance rather than data. In this
    /// variant `actions`, `hover_only_actions` and `footer` are not
    /// rendered — the whole card is the single click target.
    Action,
}

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
    /// Optional footer element.
    footer: Option<AnyElement>,
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
            footer: None,
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
        // Construct the header row: Icon + Title + Spacer + Actions
        let header = h_flex()
            .when_some(self.icon, |this, icon| this.child(icon))
            .when_some(self.title, |this, title| {
                let subtitle = self.subtitle.clone();
                let subtitle_font = self.subtitle_font.clone();
                let tag = self.tag.clone();
                this.child(
                    div().flex_1().overflow_hidden().child(
                        v_flex()
                            .ml_2()
                            .child(
                                // Name + tag chip share one row, hugging.
                                // The name is flex_initial + min_w_0 so
                                // it sizes to its content yet truncates
                                // when long; the chip is flex_none so it
                                // always stays right beside the name; a
                                // trailing flex_1 spacer eats the slack
                                // so the pair stays left-aligned and
                                // adjacent even for short names.
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .w_full()
                                    .child(
                                        div().flex_initial().min_w_0().overflow_hidden().child(
                                            Label::new(title)
                                                .text_base()
                                                .font_semibold()
                                                .whitespace_nowrap()
                                                .text_ellipsis(),
                                        ),
                                    )
                                    .when_some(tag, |row, (label, colors)| {
                                        let (bg, fg) = colors.unwrap_or_else(|| {
                                            let muted = cx.theme().muted_foreground;
                                            (Hsla { a: 0.15, ..muted }, muted)
                                        });
                                        row.child(
                                            div()
                                                .flex_none()
                                                .px_1p5()
                                                .rounded_full()
                                                .bg(bg)
                                                .child(Label::new(label).text_xs().text_color(fg)),
                                        )
                                    })
                                    .child(div().flex_1()),
                            )
                            .when_some(subtitle, |col, sub| {
                                let mut label = Label::new(sub)
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(cx.theme().muted_foreground);
                                if let Some(family) = subtitle_font {
                                    label = label.font_family(family);
                                }
                                col.child(label)
                            }),
                    ),
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
        // ListItem packs its children into a single gapless block, so
        // compose header / description / footer into one v_flex with
        // an explicit gap to get even vertical rhythm (otherwise the
        // description ends up cramped against its neighbors).
        let body = v_flex()
            .w_full()
            .gap_2()
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
            // Footer behind a dim hairline divider so metadata reads
            // as a distinct region. No top margin — the v_flex gap
            // already provides separation from the description.
            .when_some(self.footer, |this, footer| {
                // pt_3 matches the card's `.py_3()` bottom inset so the
                // date sits with equal whitespace above (divider→text)
                // and below (text→card edge).
                this.child(div().pt_3().border_t_1().border_color(cx.theme().border).child(footer))
            });

        let card = ListItem::new(self.id)
            .m_2()
            .border(px(1.))
            .border_color(cx.theme().border)
            // Slightly tighter vertical padding than horizontal so the
            // space below the footer doesn't dwarf the internal gap_2
            // rhythm (ListItem adds its own py_1 on top of this).
            .px_4()
            .py_3()
            .rounded(cx.theme().radius)
            .when_some(self.bg, |this, bg| this.bg(bg))
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(body);

        div().group(CARD_GROUP).child(card).into_any_element()
    }
}
