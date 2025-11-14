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

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};

pub struct ZedisServerState {
    pub current_server: String,
}

impl ZedisServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            current_server: "".to_string(),
        }
    }
    pub fn select_server(&mut self, server: String, cx: &mut Context<Self>) {
        if self.current_server != server {
            self.current_server = server;
            cx.notify();
        }
    }
}
