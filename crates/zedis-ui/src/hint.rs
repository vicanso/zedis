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

//! One-time onboarding hint: an info strip shown at the top of a
//! specialized panel on the user's first visit, dismissed forever via its
//! close button. The text and the dismissal side effect belong to the
//! caller — this widget only draws the strip.

use gpui::{App, ClickEvent, SharedString, Window, prelude::*};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
};

type OnClose = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Builder for the strip. Construct via [`hint_banner`]; wire the
/// persistence side of dismissal through [`ZedisHintBanner::on_close`].
#[derive(IntoElement)]
pub struct ZedisHintBanner {
    id: SharedString,
    text: SharedString,
    on_close: Option<OnClose>,
}

/// A dismissible first-visit hint strip. `id` must be unique within the
/// window (it names the close button); `text` is the i18n-resolved hint.
pub fn hint_banner(id: impl Into<SharedString>, text: impl Into<SharedString>) -> ZedisHintBanner {
    ZedisHintBanner {
        id: id.into(),
        text: text.into(),
        on_close: None,
    }
}

impl ZedisHintBanner {
    /// Called when the close button is clicked. The caller persists the
    /// dismissal (and re-renders itself) here.
    pub fn on_close(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ZedisHintBanner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.4))
            .child(Icon::new(IconName::Info).small().text_color(cx.theme().primary))
            .child(
                Label::new(self.text)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .flex_1(),
            )
            .when_some(self.on_close, |this, on_close| {
                this.child(
                    Button::new(self.id.clone())
                        .icon(Icon::new(IconName::Close))
                        .ghost()
                        .xsmall()
                        .on_click(move |event, window, cx| on_close(event, window, cx)),
                )
            })
    }
}
