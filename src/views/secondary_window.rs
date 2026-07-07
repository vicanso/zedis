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

use gpui::{
    AnyWindowHandle, App, AppContext, DisplayId, Entity, FocusHandle, Focusable, Global, KeyDownEvent, Window,
    WindowOptions, div, prelude::*,
};
use gpui_component::Root;
use std::{any::TypeId, collections::HashMap};

/// The `DisplayId` of the monitor the main (active) window is currently on, or
/// `None` if there's no active window. Pass it to `Bounds::centered` so a
/// secondary window (About / Settings) opens on the same monitor as the app
/// instead of always centering on the primary display.
pub fn active_window_display(cx: &mut App) -> Option<DisplayId> {
    let handle = cx.active_window()?;
    handle
        .update(cx, |_, window, cx| window.display(cx).map(|d| d.id()))
        .ok()
        .flatten()
}

/// Global registry that tracks open secondary windows by their content type.
/// Allows [`open_secondary_window`] to reuse an existing window instead of
/// opening a duplicate.
struct SecondaryWindowRegistry(HashMap<TypeId, AnyWindowHandle>);

impl Global for SecondaryWindowRegistry {}

impl SecondaryWindowRegistry {
    fn get(cx: &mut App) -> &mut Self {
        if cx.try_global::<Self>().is_none() {
            cx.set_global(Self(HashMap::new()));
        }
        cx.global_mut::<Self>()
    }
}

/// Wrapper view that takes focus on creation and closes the window on ESC.
///
/// Used as the content layer inside [`Root`] for all secondary windows
/// (settings, about, etc.) so that ESC-to-close behaviour is centralised
/// in one place rather than repeated per-window.
struct SecondaryWindow<V: Render + 'static> {
    focus_handle: FocusHandle,
    content: Entity<V>,
}

impl<V: Render + 'static> SecondaryWindow<V> {
    fn new(content: Entity<V>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self { focus_handle, content }
    }
}

impl<V: Render + 'static> Focusable for SecondaryWindow<V> {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<V: Render + 'static> Render for SecondaryWindow<V> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|_this, event: &KeyDownEvent, window, _cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(self.content.clone())
    }
}

/// Opens a secondary (non-main) window with ESC-to-close support.
///
/// If a window for the same content type `V` is already open it will be
/// activated instead of creating a duplicate.  The `build` closure receives
/// `(window, cx)` and should return the content entity.  The window is
/// automatically wrapped with [`Root`] (required by gpui_component widgets)
/// and [`SecondaryWindow`] (focus + ESC handling).
pub fn open_secondary_window<V, F>(options: WindowOptions, cx: &mut App, build: F)
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
{
    let type_id = TypeId::of::<V>();

    // Check whether a window for this type already exists and is still open.
    if let Some(handle) = SecondaryWindowRegistry::get(cx).0.get(&type_id).copied() {
        let still_open = handle.update(cx, |_, window, _| window.activate_window()).is_ok();
        if still_open {
            return;
        }
        // Window was closed — fall through to create a new one.
        SecondaryWindowRegistry::get(cx).0.remove(&type_id);
    }

    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        let content = build(window, cx);
        let wrapper = cx.new(|cx| SecondaryWindow::new(content, window, cx));
        cx.new(|cx| Root::new(wrapper, window, cx))
    }) {
        SecondaryWindowRegistry::get(cx).0.insert(type_id, handle.into());
    }
}
