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

//! In-panel help affordance: a small "?" info button that opens a popover
//! explaining a specialized panel (memory analysis, keyspace
//! notifications, …). The i18n-resolved Markdown body is passed in by the
//! caller, so this widget stays free of the app's i18n helpers — matching
//! the crate rule that platform/app values arrive from the caller.

use gpui::{Anchor, ScrollHandle, SharedString, div, prelude::*, px, rems};
use gpui_kit::component::{
    Icon, Sizable,
    button::{Button, ButtonVariants},
    popover::Popover,
    scroll::{Scrollbar, ScrollbarMode},
    text::{TextView, TextViewStyle},
};

/// The `?` glyph icon. Referenced by its embedded-asset path rather than the
/// app's `CustomIconName` enum (which this crate can't see); the SVG is
/// resolved through the asset source the app registers globally with gpui.
const QUESTION_ICON: &str = "icons/circle-question-mark.svg";

/// Compact Markdown styling for help popovers: the library sizes headings
/// up to ~28px, which overwhelms a small popover; shrink them to a gentle
/// hierarchy and tighten the paragraph gap.
fn help_text_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.4))
        .heading_font_size(|level, _base| match level {
            1 => px(16.),
            2 => px(15.),
            _ => px(14.),
        })
}

/// A "?" help button for a panel header. `id` must be unique within the
/// view; `body` is Markdown (the caller resolves i18n). Opens on click,
/// dismisses on click-outside. The content is capped and scrolls so a long
/// explanation can't grow the popover past the viewport.
pub fn help_popover(id: impl Into<SharedString>, body: impl Into<SharedString>) -> Popover {
    let id: SharedString = id.into();
    let body = body.into();
    // Derive the child ids up front so the (owned) `id` can move into
    // `Popover::new` while the content closure keeps only what it needs.
    let trigger_id = SharedString::from(format!("{id}-trigger"));
    let scroll_key = SharedString::from(format!("{id}-scroll"));
    let body_id = SharedString::from(format!("{id}-body"));
    Popover::new(id)
        // Left-anchored (open rightward/down): these buttons sit next to a
        // panel's title at the left of the header, so this keeps the popover
        // on screen.
        .anchor(Anchor::TopLeft)
        .trigger(
            Button::new(trigger_id)
                .ghost()
                .xsmall()
                .icon(Icon::empty().path(QUESTION_ICON)),
        )
        .max_w(px(400.))
        .content(move |_state, window, cx| {
            // A persistent scroll handle (keyed to this popover) drives both
            // the native scroll area and its visible scrollbar. `max_h` +
            // `overflow_y_scroll` makes the box adaptive: it sizes to the
            // content below the cap and scrolls above it. gpui-component's
            // `overflow_y_scrollbar` wrapper can't do this (it only inherits
            // `size`, not `max_h`), so we hand-roll native scroll + a sibling
            // `Scrollbar`, mirroring the command palette.
            let scroll_handle = window
                .use_keyed_state(scroll_key.clone(), cx, |_, _| ScrollHandle::default())
                .read(cx)
                .clone();
            div()
                .relative()
                .w(px(360.))
                .child(
                    div()
                        .id("help-scroll")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .track_scroll(&scroll_handle)
                        .child(TextView::markdown(body_id.clone(), body.clone()).style(help_text_style())),
                )
                .child(
                    // Overlay bar reading the same handle; `Always` keeps it
                    // visible whenever the content overflows and auto-hides
                    // when it fits.
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .child(Scrollbar::vertical(&scroll_handle).mode(ScrollbarMode::Always)),
                )
        })
}
