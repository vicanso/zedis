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

use gpui::{Entity, EventEmitter, SharedString, Subscription, Window, prelude::*};
use gpui_component::{
    IndexPath,
    select::{Select, SelectEvent, SelectItem, SelectState},
};

#[derive(Clone)]
struct ZedisSelectOption {
    label: SharedString,
    index: usize,
}

impl SelectItem for ZedisSelectOption {
    type Value = usize;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.index
    }
}

pub enum ZedisSelectEvent {
    Change(usize),
}

pub struct ZedisSelect {
    state: Entity<SelectState<Vec<ZedisSelectOption>>>,
    _subscription: Subscription,
}

impl EventEmitter<ZedisSelectEvent> for ZedisSelect {}

impl ZedisSelect {
    pub fn new(items: Vec<String>, selected_index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options = items
            .into_iter()
            .enumerate()
            .map(|(i, s)| ZedisSelectOption {
                label: s.into(),
                index: i,
            })
            .collect::<Vec<_>>();
        let initial = selected_index.map(IndexPath::new);
        let state = cx.new(|cx| SelectState::new(options, initial, window, cx));
        let subscription = cx.subscribe_in(
            &state,
            window,
            |_this, _state, event: &SelectEvent<Vec<ZedisSelectOption>>, _window, cx| {
                let SelectEvent::Confirm(value) = event;
                if let Some(index) = *value {
                    cx.emit(ZedisSelectEvent::Change(index));
                }
            },
        );
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for ZedisSelect {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Select::new(&self.state)
    }
}
