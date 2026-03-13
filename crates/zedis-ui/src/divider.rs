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

use gpui::{AnyElement, App, StyleRefinement, Window, div, prelude::*};
use gpui_component::{ActiveTheme, StyledExt, h_flex};

/// A container that automatically inserts vertical divider lines between its children.
#[derive(IntoElement, Default)]
pub struct ZedisDivider {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl ZedisDivider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }

    pub fn when(self, condition: bool, then: impl FnOnce(Self) -> Self) -> Self {
        if condition { then(self) } else { self }
    }
}
impl Styled for ZedisDivider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ZedisDivider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = cx.theme().muted_foreground;
        let last = self.children.len().saturating_sub(1);
        let mut container = h_flex().items_center().refine_style(&self.style);
        for (i, child) in self.children.into_iter().enumerate() {
            container = container.child(child);
            if i < last {
                container = container.child(div().h_4().w_px().flex_none().bg(color).mx_4());
            }
        }
        container
    }
}
