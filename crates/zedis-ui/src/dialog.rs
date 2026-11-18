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

use gpui::{AnyElement, App, ClickEvent, IntoElement, ParentElement, Pixels, SharedString, Styled, Window};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::{Icon, IconName, WindowExt, h_flex};
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
    footer_child: Option<Rc<dyn Fn() -> AnyElement>>,
    on_ok: Option<ZedisDialogOnOk>,
    on_close: Option<ZedisDialogOnClose>,
    button_props: Option<DialogButtonProps>,
    overlay_closable: Option<bool>,
    width: Option<Pixels>,
    alert: bool,
    danger: bool,
    ok_text: Option<SharedString>,
    cancel_text: Option<SharedString>,
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

    /// Sets a custom **footer** element builder, replacing the default
    /// ok/cancel footer built from [`Self::ok_text`] / [`Self::cancel_text`].
    ///
    /// The footer is a sibling of the dialog body, which is the scroll
    /// container — so anything here stays visible however long the body grows.
    /// Put the action row (and anything that must never scroll out of reach,
    /// e.g. a live progress bar) here rather than at the end of `child`.
    ///
    /// Like [`Self::child`], the builder runs on every render, so returning a
    /// view entity gives the footer live state.
    pub fn footer_child<F, E>(mut self, f: F) -> Self
    where
        F: Fn() -> E + 'static,
        E: IntoElement,
    {
        self.footer_child = Some(Rc::new(move || f().into_any_element()));
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

    /// Sets the OK button label. Required for the default footer to render
    /// in non-alert mode (alert mode reads the label from `button_props`
    /// itself, which is render-only and not visible from here).
    pub fn ok_text(mut self, text: impl Into<SharedString>) -> Self {
        self.ok_text = Some(text.into());
        self
    }

    /// Sets the Cancel button label. When unset, no cancel button renders
    /// in the default non-alert footer.
    pub fn cancel_text(mut self, text: impl Into<SharedString>) -> Self {
        self.cancel_text = Some(text.into());
        self
    }

    /// Sets whether clicking the overlay closes the dialog.
    ///
    /// Ignored by an alert dialog: gpui-component deprecated backdrop
    /// dismissal there by design, so a confirm can only be answered by its
    /// buttons.
    pub fn overlay_closable(mut self, overlay_closable: bool) -> Self {
        self.overlay_closable = Some(overlay_closable);
        self
    }

    /// Override the dialog width. The underlying gpui_component dialog
    /// defaults to 448px, which is too narrow for chip-row layouts.
    pub fn w(mut self, width: impl Into<Pixels>) -> Self {
        self.width = Some(width.into());
        self
    }

    /// Switches to `AlertDialog` mode: a confirmation, answered by its
    /// buttons — OK and, always, Cancel. (gpui-component's alert shows OK
    /// alone unless asked, which left every confirm in the app with one
    /// button, the one that goes ahead.)
    pub fn alert(mut self) -> Self {
        self.alert = true;
        self
    }

    /// Marks an alert as the confirmation of a destructive action: OK is
    /// drawn in the danger colour, `Cancel` receives initial focus so an
    /// immediate `Return` cancels safely without running the destructive
    /// action, and tabbing to the OK button lets `Return` or `Space`
    /// confirm it (alongside clicking it directly).
    ///
    /// Needs [`Self::ok_text`] and [`Self::cancel_text`]: the buttons are
    /// this dialog's own, because an alert's stock buttons send Return and
    /// a click down one path and cannot tell them apart.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Opens the dialog on the given window.
    pub fn open(self, window: &mut Window, cx: &mut App) {
        // gpui_component's `Dialog` renders a footer only when one is set —
        // `button_props` alone produces no buttons. So a non-alert dialog with
        // an `on_ok` but neither `ok_text` nor `footer_child` registers a
        // callback that nothing can ever reach.
        debug_assert!(
            self.alert || self.on_ok.is_none() || self.ok_text.is_some() || self.footer_child.is_some(),
            "ZedisDialog \"{}\" has on_ok but no footer — set .ok_text(…) or .footer_child(…)",
            self.title
        );
        debug_assert!(
            !self.danger || (self.alert && self.ok_text.is_some() && self.cancel_text.is_some()),
            "ZedisDialog \"{}\" is .danger() — it must be an alert with .ok_text(…) and .cancel_text(…)",
            self.title
        );
        let danger = self.danger && self.alert;
        let title = self.title;
        let icon = self.icon;
        let message = self.message;
        let child = self.child;
        let footer_child = self.footer_child;
        let on_ok = self.on_ok;
        let on_close = self.on_close;
        // An alert asks a question, and a question can be declined.
        let button_props = if self.alert {
            Some(self.button_props.unwrap_or_default().show_cancel(true))
        } else {
            self.button_props
        };
        let overlay_closable = self.overlay_closable;
        let width = self.width;
        let ok_text = self.ok_text;
        let cancel_text = self.cancel_text;
        // The footer this dialog draws itself: a plain dialog's, and a
        // destructive alert's (see `danger`). Any other alert keeps the
        // stock buttons its `button_props` describe.
        let own_footer = if ok_text.is_some() && (!self.alert || danger) {
            Some((ok_text.clone(), cancel_text.clone()))
        } else {
            None
        };
        let danger_handles = danger.then(|| (cx.focus_handle(), cx.focus_handle()));

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

                if let Some(w) = width {
                    d = d.w(w);
                }
                if let Some(ref bp) = button_props {
                    d = d.button_props(bp.clone());
                }
                if let Some(ref cf) = child {
                    d = d.child(cf());
                } else if let Some(ref msg) = message {
                    d = d.child(msg.to_string());
                }
                if let Some((_, ref ok_focus)) = danger_handles {
                    // Cancel holds initial focus so an immediate Return
                    // cancels safely; once the user tabs to the destructive
                    // button, Return confirms it.
                    let ok_focus = ok_focus.clone();
                    let ok_cb = on_ok.clone();
                    d = d.on_ok(move |e, window, cx| {
                        if ok_focus.is_focused(window) {
                            ok_cb.as_ref().map(|f| f(e, window, cx)).unwrap_or(true)
                        } else {
                            true
                        }
                    });
                } else if let Some(ref ok) = on_ok {
                    let ok = ok.clone();
                    d = d.on_ok(move |e, w, cx| ok(e, w, cx));
                }
                if let Some(ref cb) = on_close {
                    let cb = cb.clone();
                    d = d.on_close(move |e, w, cx| cb(e, w, cx));
                }
                // A caller-supplied footer wins over the ok/cancel one — it owns
                // the whole action area (see `footer_child`).
                if let Some(ref ff) = footer_child {
                    d = d.footer(ff());
                } else if let Some((ok_label, cancel_label)) = own_footer.clone() {
                    // Wire the buttons' `on_click` directly instead of using the
                    // stock `DialogAction`/`DialogClose` wrappers: those fire by
                    // dispatching an action along the window's focus path, and a
                    // dialog whose body holds no focusable element (labels only)
                    // never receives it — the focus is still behind the overlay,
                    // leaving the buttons dead (upstream FIXME in Dialog's
                    // keyboard handling notes the same gap for Escape).
                    let mut footer = DialogFooter::new();
                    if let Some(cancel) = cancel_label {
                        let close_cb = on_close.clone();
                        let mut cancel_btn = Button::new("zedis-dialog-cancel").label(cancel).outline();
                        if let Some((ref cancel_focus, _)) = danger_handles {
                            cancel_btn = cancel_btn.track_focus(cancel_focus);
                        }
                        footer = footer.child(cancel_btn.on_click(move |e, window, cx| {
                            window.close_dialog(cx);
                            if let Some(cb) = &close_cb {
                                cb(e, window, cx);
                            }
                        }));
                    }
                    if let Some(ok) = ok_label {
                        let ok_cb = on_ok.clone();
                        let close_cb = on_close.clone();
                        let button = Button::new("zedis-dialog-ok").label(ok);
                        let mut button = if danger { button.danger() } else { button.primary() };
                        if let Some((_, ref ok_focus)) = danger_handles {
                            button = button.track_focus(ok_focus);
                        }
                        footer = footer.child(button.on_click(move |e, window, cx| {
                            // Mirror the ConfirmDialog contract: on_ok
                            // returning false keeps the dialog open.
                            let close = ok_cb.as_ref().map(|f| f(e, window, cx)).unwrap_or(true);
                            if close {
                                window.close_dialog(cx);
                                if let Some(cb) = &close_cb {
                                    cb(e, window, cx);
                                }
                            }
                        }));
                    }
                    d = d.footer(footer);
                }
                d
            }};
        }

        if self.alert {
            // No `overlay_closable` here: gpui-component disabled backdrop
            // dismissal for alert dialogs, so the setter is deprecated and
            // does nothing.
            let initial_focus = std::cell::Cell::new(danger);
            let cancel_focus = danger_handles.as_ref().map(|(cancel_focus, _)| cancel_focus.clone());
            window.open_alert_dialog(cx, move |dialog, window, cx| {
                if initial_focus.replace(false)
                    && let Some(ref cancel_focus) = cancel_focus
                {
                    cancel_focus.focus(window, cx);
                }
                apply_config!(dialog).close_button(true)
            });
        } else {
            window.open_dialog(cx, move |dialog, _, _| {
                let mut d = apply_config!(dialog);
                if let Some(oc) = overlay_closable {
                    d = d.overlay_closable(oc);
                }
                d
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AnyWindowHandle, AppContext, Context, Render, TestAppContext, div, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use std::cell::Cell;

    struct Page;
    impl Render for Page {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn window(cx: &mut TestAppContext) -> AnyWindowHandle {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.set_reduce_motion(true);
        });
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let page = cx.new(|_| Page);
            Root::new(page, window, cx)
        });
        handle.into()
    }

    /// Opens an alert whose OK records that it ran; `danger` picks the shape.
    fn ask(cx: &mut TestAppContext, handle: AnyWindowHandle, danger: bool) -> Rc<Cell<bool>> {
        let ran = Rc::new(Cell::new(false));
        let seen = ran.clone();
        cx.update_window(handle, |_, window, cx| {
            let dialog = ZedisDialog::new_alert("Delete \"k\"?", "It cannot be undone.")
                .ok_text("Delete")
                .cancel_text("Cancel")
                .on_ok(move |_, _, _| {
                    seen.set(true);
                    true
                });
            let dialog = if danger { dialog.danger() } else { dialog };
            dialog.open(window, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        ran
    }

    fn press_return(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .expect("the window is open")
    }

    /// A destructive confirm answers Return with Cancel: it closes and runs
    /// nothing, and only its own Delete button goes ahead. A plain alert's
    /// Return is its OK — which is why every confirm that destroys something
    /// is this shape.
    #[gpui::test]
    fn a_destructive_confirm_answers_return_with_cancel(cx: &mut TestAppContext) {
        let handle = window(cx);

        let ran = ask(cx, handle, true);
        assert!(!press_return(cx, handle), "Return closes it");
        assert!(!ran.get(), "and deletes nothing");

        let ran = ask(cx, handle, true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("zedis-dialog-ok", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert!(ran.get(), "its Delete button goes ahead");

        let ran = ask(cx, handle, false);
        press_return(cx, handle);
        assert!(ran.get(), "a plain alert's Return is its OK");
    }

    #[gpui::test]
    fn tabbing_to_destructive_ok_confirms_on_return(cx: &mut TestAppContext) {
        let handle = window(cx);

        let ran = ask(cx, handle, true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("tab", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert!(!press_return(cx, handle), "Return closes the dialog once OK is focused");
        assert!(ran.get(), "and runs the destructive action");

        // Space on the focused destructive button also confirms once.
        let ran = ask(cx, handle, true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("tab", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("space", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        let still_open = cx
            .update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .expect("the window is open");
        assert!(!still_open, "Space closes the dialog");
        assert!(ran.get(), "and runs the destructive action");
    }
}
