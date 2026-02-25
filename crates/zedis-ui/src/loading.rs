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

use gpui::{App, SharedString, StyleRefinement, Styled, Window, prelude::*, relative};
use gpui_component::{ActiveTheme, StyledExt, skeleton::Skeleton, v_flex};

#[derive(IntoElement, Default)]
pub struct ZedisSkeletonLoading {
    style: StyleRefinement,
    text: Option<SharedString>,
    count: usize,
}

impl ZedisSkeletonLoading {
    pub fn new() -> Self {
        Self {
            count: 5,
            ..Default::default()
        }
    }

    pub fn text(mut self, text: impl Into<SharedString>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn count(mut self, count: usize) -> Self {
        self.count = count;
        self
    }
}

impl Styled for ZedisSkeletonLoading {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ZedisSkeletonLoading {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut container = v_flex().gap_2().refine_style(&self.style).w_full();

        let width_percentages = [0.8, 0.4, 0.66, 0.2, 1.0];

        for i in 0..self.count {
            let width = width_percentages[i % width_percentages.len()];
            container = container.child(Skeleton::new().w(relative(width)).h_4().rounded_md());
        }

        if let Some(text) = self.text {
            container = container.child(
                gpui::div()
                    .w_full()
                    .mt_2()
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .text_align(gpui::TextAlign::Center)
                    .child(text),
            );
        }

        container
    }
}
