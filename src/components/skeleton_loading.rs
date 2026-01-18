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

use crate::states::i18n_common;
use gpui::{App, Window, prelude::*};
use gpui_component::{ActiveTheme, label::Label, skeleton::Skeleton, v_flex};

#[derive(IntoElement)]
pub struct SkeletonLoading {}

impl SkeletonLoading {
    pub fn new() -> Self {
        Self {}
    }
}

impl RenderOnce for SkeletonLoading {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            // Variable-width skeletons create a more natural loading appearance
            .child(Skeleton::new().w_4_5().h_4().rounded_md())
            .child(Skeleton::new().w_2_5().h_4().rounded_md())
            .child(Skeleton::new().w_2_3().h_4().rounded_md())
            .child(Skeleton::new().w_1_5().h_4().rounded_md())
            .child(Skeleton::new().w_full().h_4().rounded_md())
            .child(
                Label::new(i18n_common(cx, "loading"))
                    .w_full()
                    .text_color(cx.theme().muted_foreground)
                    .mt_2()
                    .text_align(gpui::TextAlign::Center),
            )
            .into_any_element()
    }
}
