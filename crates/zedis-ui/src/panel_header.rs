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

use gpui::{AnyElement, App, ElementId, StyleRefinement, Window, prelude::*};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex};

/// Shared top toolbar header for secondary inspector and telemetry panels.
///
/// Aligns every panel header to the standard `px_3` content spine, wraps onto a
/// second line rather than clipping narrow windows, and draws a single bottom
/// hairline separator.
#[derive(IntoElement, Default)]
pub struct ZedisPanelHeader {
    id: Option<ElementId>,
    scroll_x: bool,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

/// Construct a new shared panel header bar.
pub fn panel_header() -> ZedisPanelHeader {
    ZedisPanelHeader::default()
}

impl ZedisPanelHeader {
    /// Assign a stable element ID (required when enabling [`Self::scroll_x`]).
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Allow horizontal scrolling as a narrow-viewport fallback when wrapped
    /// control groups still exceed the panel width.
    pub fn scroll_x(mut self, scroll: bool) -> Self {
        self.scroll_x = scroll;
        self
    }
}

impl Styled for ZedisPanelHeader {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ZedisPanelHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ZedisPanelHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let base = h_flex()
            .w_full()
            .flex_none()
            .min_h_10()
            .px_3()
            .py_1p5()
            .gap_x_4()
            .gap_y_2()
            .items_center()
            .flex_wrap()
            .border_b_1()
            .border_color(cx.theme().border)
            .refine_style(&self.style)
            .children(self.children);

        match (self.id, self.scroll_x) {
            (Some(id), true) => base.id(id).overflow_x_scroll().into_any_element(),
            (Some(id), false) => base.id(id).into_any_element(),
            (None, _) => base.into_any_element(),
        }
    }
}
