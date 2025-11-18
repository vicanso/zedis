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

use gpui::prelude::*;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Route {
    #[default]
    Home,
    Editor,
}

#[derive(Debug, Clone, Default)]
pub struct ZedisAppState {
    route: Route,
}

impl ZedisAppState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            ..Default::default()
        }
    }
    pub fn route(&self) -> Route {
        self.route
    }
    pub fn go_to(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }
}
