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

use gpui::{AnyElement, App, ClickEvent, IntoElement, ParentElement, SharedString, Styled, Window};
use gpui_component::{Icon, IconName, WindowExt, dialog::DialogButtonProps, h_flex};
use std::rc::Rc;

type ZedisDialogOnOk = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static>;
type ZedisDialogOnClose = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A builder for creating confirmation dialogs with less boilerplate.
///
/// Supports both regular `Dialog` and `AlertDialog` via the `.alert()` method.
///
/// # Examples
///
/// ```ignore
/// ZedisDialog::new()
///     .title("Delete Key")
///     .message("Are you sure?")
///     .button_props(dialog_button_props(cx))
///     .on_ok(move |_, window, cx| {
///         // handle confirmation
///         true
///     })
///     .open(window, cx);
/// ```
#[derive(Default)]
pub struct ZedisDialog {
    title: SharedString,
    icon: Option<Icon>,
    message: Option<SharedString>,
    child: Option<Rc<dyn Fn() -> AnyElement>>,
    on_ok: Option<ZedisDialogOnOk>,
    on_close: Option<ZedisDialogOnClose>,
    button_props: Option<DialogButtonProps>,
    overlay_closable: Option<bool>,
    alert: bool,
}

impl gpui::prelude::FluentBuilder for ZedisDialog {}

impl ZedisDialog {
    /// Creates a new `ZedisDialog` builder with default settings.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            ..Default::default()
        }
    }
    pub fn new_alert(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self::new(title).alert().message(message).icon(IconName::Info)
    }

    /// Sets the dialog icon, displayed alongside the title.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Sets a simple text message as the dialog content.
    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Sets a custom child element builder for the dialog content.
    ///
    /// The builder is called each time the dialog renders, so it must be
    /// repeatable. Use this for rich content like scrollable containers.
    pub fn child<F, E>(mut self, f: F) -> Self
    where
        F: Fn() -> E + 'static,
        E: IntoElement,
    {
        self.child = Some(Rc::new(move || f().into_any_element()));
        self
    }

    /// Sets the OK button callback.
    ///
    /// Return `true` to close the dialog, `false` to keep it open.
    pub fn on_ok(mut self, on_ok: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static) -> Self {
        self.on_ok = Some(Rc::new(on_ok));
        self
    }

    /// Sets the callback for when the dialog is closed (after `on_ok` or `on_cancel`).
    pub fn on_close(mut self, on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(on_close));
        self
    }

    /// Sets the dialog button properties (labels, variants).
    pub fn button_props(mut self, button_props: DialogButtonProps) -> Self {
        self.button_props = Some(button_props);
        self
    }

    /// Sets whether clicking the overlay closes the dialog.
    pub fn overlay_closable(mut self, overlay_closable: bool) -> Self {
        self.overlay_closable = Some(overlay_closable);
        self
    }

    /// Switches to `AlertDialog` mode (centered footer, no close button).
    pub fn alert(mut self) -> Self {
        self.alert = true;
        self
    }

    /// Opens the dialog on the given window.
    pub fn open(self, window: &mut Window, cx: &mut App) {
        let title = self.title;
        let icon = self.icon;
        let message = self.message;
        let child = self.child;
        let on_ok = self.on_ok;
        let on_close = self.on_close;
        let button_props = self.button_props;
        let overlay_closable = self.overlay_closable;

        /// Applies common configuration to a dialog.
        /// Works with both `Dialog` and `AlertDialog` since they share the same builder API.
        macro_rules! apply_config {
            ($d:expr) => {{
                let mut d = $d;

                if let Some(i) = &icon {
                    d = d.title(h_flex().gap_1().child(i.clone()).child(title.clone()));
                } else {
                    d = d.title(title.clone());
                }

                if let Some(oc) = overlay_closable {
                    d = d.overlay_closable(oc);
                }
                if let Some(ref bp) = button_props {
                    d = d.button_props(bp.clone());
                }
                if let Some(ref cf) = child {
                    d = d.child(cf());
                } else if let Some(ref msg) = message {
                    d = d.child(msg.to_string());
                }
                if let Some(ref ok) = on_ok {
                    let ok = ok.clone();
                    d = d.on_ok(move |e, w, cx| ok(e, w, cx));
                }
                if let Some(ref cb) = on_close {
                    let cb = cb.clone();
                    d = d.on_close(move |e, w, cx| cb(e, w, cx));
                }
                d
            }};
        }

        if self.alert {
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let d = apply_config!(dialog);
                d.overlay_closable(overlay_closable.unwrap_or(true)).close_button(true)
            });
        } else {
            window.open_dialog(cx, move |dialog, _, _| apply_config!(dialog));
        }
    }
}
