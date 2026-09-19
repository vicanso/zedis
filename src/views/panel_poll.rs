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

//! The loop behind a panel that samples the server for as long as it is open
//! — Server Load, Hot Keys — plus the chip both of them head their page with.
//!
//! Sample, apply, sleep one interval, again. The sleep ends early when the
//! panel asks ([`PanelPoll::refresh_now`]: its Refresh button, or the end of
//! an action whose effect the user is waiting to see). Both panels used to
//! carry this loop themselves, and both got that early wake-up by sleeping in
//! 200ms slices and checking a flag after each one — five timer wake-ups a
//! second for as long as the panel stayed open. Here the sleep races the
//! interval against a channel instead, so an idle panel wakes once per
//! interval and a click is answered at once rather than within a slice.
//!
//! A round is skipped — the loop keeps turning — while the panel's server
//! state says nobody is looking (`is_background()`: an inactive workspace
//! tab, an unattended window, a hidden browser page) or has no server yet.

use crate::states::ZedisServerState;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::future::{Either, select};
use gpui::{Context, Entity, Hsla, SharedString, Task, prelude::*};
use gpui_kit::component::{StyledExt, label::Label, v_flex};
use std::future::Future;
use std::time::Duration;

/// A running poll. Dropping it stops the loop.
pub(crate) struct PanelPoll {
    _task: Task<()>,
    refresh: UnboundedSender<()>,
}

impl PanelPoll {
    /// Poll `fetch(server_id, db)` every `interval` and hand each result to
    /// `apply` on the view. The first round runs at once.
    pub(crate) fn start<V, R, Fut>(
        cx: &mut Context<V>,
        server_state: Entity<ZedisServerState>,
        interval: Duration,
        fetch: impl Fn(String, usize) -> Fut + 'static,
        apply: impl Fn(&mut V, R, &mut Context<V>) + 'static,
    ) -> Self
    where
        V: 'static,
        R: 'static,
        Fut: Future<Output = R> + 'static,
    {
        let (refresh, mut refreshed) = unbounded::<()>();
        let task = cx.spawn(async move |this, cx| {
            loop {
                // The view being gone is what ends the loop, whichever call
                // notices first.
                let Ok(target) = this.update(cx, |_, cx| {
                    let state = server_state.read(cx);
                    (!state.is_background() && !state.server_id().is_empty())
                        .then(|| (state.server_id().to_string(), state.db()))
                }) else {
                    break;
                };
                if let Some((server_id, db)) = target {
                    let result = fetch(server_id, db).await;
                    if this.update(cx, |view, cx| apply(view, result, cx)).is_err() {
                        break;
                    }
                }
                let sleep = cx.background_executor().timer(interval);
                if let Either::Right((None, _)) = select(sleep, refreshed.next()).await {
                    // Every sender is gone: the `PanelPoll` was dropped.
                    break;
                }
            }
        });
        Self { _task: task, refresh }
    }

    /// Sample now instead of at the end of the interval.
    pub(crate) fn refresh_now(&self) {
        // A send to a loop that has ended is nothing to report.
        let _ = self.refresh.unbounded_send(());
    }
}

/// A label over a value, for the summary row at the top of a sampling panel.
pub(crate) fn summary_chip(
    label: SharedString,
    value: impl Into<SharedString>,
    value_color: Hsla,
    muted: Hsla,
) -> impl IntoElement {
    v_flex()
        .gap_0p5()
        .child(Label::new(label).text_xs().text_color(muted))
        .child(
            Label::new(value.into())
                .text_sm()
                .font_semibold()
                .text_color(value_color),
        )
}
