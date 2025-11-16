// Copyright 2025 Tree xie.
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

use crate::states::ZedisServerState;
use gpui::App;
use gpui::Context;
use gpui::Entity;
use gpui::InteractiveElement;
use gpui::IntoElement;
use gpui::ParentElement;
use gpui::Render;
use gpui::RenderOnce;
use gpui::Styled;
use gpui::Window;
use gpui::div;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Side;
use gpui_component::Sizable;
use gpui_component::Size;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_component::v_flex;

pub struct ZedisSidebar {
    server_state: Entity<ZedisServerState>,
}
impl ZedisSidebar {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self { server_state }
    }
}
impl Render for ZedisSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Sidebar::new(Side::Left)
            .width(px(52.))
            .header(SidebarHeader::new().child("R"))
            .child(
                SidebarGroup::new("NA").child(
                    SidebarMenu::new()
                        .child(
                            SidebarMenuItem::new("")
                                .icon(Icon::new(IconName::LayoutDashboard).size(px(18.)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.server_state.update(cx, |state, cx| {
                                        state.select_server("", cx);
                                    });
                                })),
                        )
                        .child(
                            SidebarMenuItem::new("")
                                .icon(Icon::new(IconName::Settings).size(px(18.)))
                                .on_click(|_, _, _| println!("Settings clicked")),
                        ),
                ),
            )
            .footer(SidebarFooter::new().child("User Profile"))
        // div()
        //     .id("sidebar-container")
        //     .border_color(cx.theme().border)
        //     .size(px(48.))
        //     .border_r_1()
        //     .h_full()
        //     .child(
        //         v_flex()
        //             .h_full()
        //             .justify_end()
        //             .child(Button::new("line-column").ghost().xsmall().label("input")),
        //     )
    }
}
