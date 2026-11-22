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

use gpui::{App, ElementId, SharedString, Window, prelude::*};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, label::Label};

/// Compact key-value statistic chip with a muted label and emphasized figure.
#[derive(IntoElement)]
pub struct ZedisStatChip {
    id: Option<ElementId>,
    label: SharedString,
    value: SharedString,
}

/// Construct a reusable metric/status chip.
pub fn stat_chip(label: impl Into<SharedString>, value: impl Into<SharedString>) -> ZedisStatChip {
    ZedisStatChip {
        id: None,
        label: label.into(),
        value: value.into(),
    }
}

impl ZedisStatChip {
    /// Override the chip's element ID (needed only when two sibling chips share the same label).
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }
}

impl RenderOnce for ZedisStatChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self
            .id
            .unwrap_or_else(|| ElementId::Name(format!("stat-chip-{}", self.label).into()));
        h_flex()
            .id(id)
            .gap_1()
            .items_center()
            .px_2()
            .py_0p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .child(Label::new(self.label).text_xs().text_color(cx.theme().muted_foreground))
            .child(Label::new(self.value).text_xs().font_semibold())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Context, Render, Role, TestAppContext, px, size};
    use gpui_kit::component::{Root, v_flex};
    use gpui_kit::test::{TestQueryExt, TestWindowExt};

    struct ChipRow;

    impl Render for ChipRow {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .child(
                    h_flex()
                        .gap_2()
                        .child(stat_chip("Scanned", "120"))
                        .child(stat_chip("Matched", "4"))
                        .child(stat_chip("Skipped", "1")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(stat_chip("Count", "10").id("count-left"))
                        .child(stat_chip("Count", "20").id("count-right")),
                )
        }
    }

    #[gpui::test]
    fn sibling_stat_chips_expose_distinct_label_nodes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let handle = cx.open_window(size(px(640.), px(200.)), |window, cx| {
            let view = cx.new(|_| ChipRow);
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let labels = window.find_all_by_role(Role::Label);
            let values: Vec<&str> = labels.iter().filter_map(|node| node.value()).collect();
            for expected in ["Scanned", "120", "Matched", "4", "Skipped", "1", "10", "20"] {
                assert!(
                    values.contains(&expected),
                    "missing label {expected:?} in observed Role::Label nodes: {values:?}"
                );
            }
            assert_eq!(
                values.iter().filter(|&&v| v == "Count").count(),
                2,
                "both explicit-id same-label chips must stay distinct: {values:?}"
            );
            assert_eq!(
                labels.len(),
                10,
                "all 5 chips × 2 labels must register distinct global element IDs: {values:?}"
            );
        })
        .expect("the window is open");
    }
}
