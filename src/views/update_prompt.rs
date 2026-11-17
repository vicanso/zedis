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

//! The title-bar update chip's popover: what changed in the release, then
//! Skip this version / Update and restart, and the download's progress in
//! their place once it starts.
//!
//! A popover rather than a dialog: the update is something to read and decide
//! on beside the work, not a question that has to stop it. Its open state is
//! the store's (`update_prompt_open`) and not the popover's own, because a
//! click on the chip does not open it — the click re-checks the release, the
//! chip spins, and the answer opens it (`Zedis::open_update_prompt`). Every
//! button dispatches an [`UpdateAction`], whose handlers are desktop-only, so
//! this file compiles for the browser, where no chip is ever lit.

use crate::helpers::{UpdateAction, card_background, get_mono_font_family};
use crate::states::{ZedisGlobalStore, i18n_update};
use gpui::{Anchor, AnyElement, App, Pixels, SharedString, div, prelude::*, rems};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    popover::Popover,
    progress::Progress,
    scroll::ScrollableElement,
    text::{TextView, TextViewStyle},
    v_flex,
};
use humansize::{DECIMAL, format_size};

/// The notes scroll, so this cap only guards layout work against a
/// pathologically long text. It holds the notes of every release since the
/// running one (up to ten of them, a few thousand characters each): at 5000
/// a build three versions behind was cut off inside the second.
const MAX_NOTES: usize = 20_000;

/// Past this many lines (or [`LONG_NOTES_CHARS`]) the notes get a viewport
/// of their own instead of growing the popover.
const LONG_NOTES_LINES: usize = 12;
const LONG_NOTES_CHARS: usize = 800;

/// The title-bar chip the popover hangs from (built by the title bar).
pub(crate) const UPDATE_CHIP_ID: &str = "zedis-titlebar-update";
const UPDATE_PROMPT_ID: &str = "zedis-update-prompt";
const UPDATE_RELEASE_LINK_ID: &str = "zedis-update-release-link";
const UPDATE_SKIP_ID: &str = "zedis-update-skip";
const UPDATE_INSTALL_ID: &str = "zedis-update-install";
const UPDATE_PROGRESS_ID: &str = "zedis-update-progress";

/// `trigger` (the chip) with its popover, open while the store says so.
pub fn update_prompt(trigger: Button, cx: &App) -> Popover {
    let open = cx.global::<ZedisGlobalStore>().read(cx).update_prompt_open();
    Popover::new(UPDATE_PROMPT_ID)
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .open(open)
        .on_open_change(|open, window, cx| {
            if *open {
                // Not opened here: the check this starts opens it.
                window.dispatch_action(Box::new(UpdateAction::OpenPrompt), cx);
            } else {
                cx.global::<ZedisGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_update_prompt_open(false, cx));
            }
        })
        .w(rems(27.5))
        .content(|_, _, cx| prompt_content(cx))
}

fn prompt_content(cx: &App) -> AnyElement {
    let store = cx.global::<ZedisGlobalStore>().read(cx);
    let Some(info) = store.available_update() else {
        return div().into_any_element();
    };
    let progress = store.download_progress();
    let muted = cx.theme().muted_foreground;
    let mono: SharedString = get_mono_font_family().into();
    let page_url = info.page_url.trim().to_string();
    let mut notes = info.notes;
    if notes.chars().count() > MAX_NOTES {
        notes = notes.chars().take(MAX_NOTES).collect::<String>();
        notes.push('…');
    }

    v_flex()
        .gap_3()
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            Label::new(format!("{} {}", i18n_update(cx, "available_title"), info.version))
                                .font_semibold(),
                        )
                        .when(!page_url.is_empty(), |this| {
                            let url = page_url.clone();
                            this.child(
                                Button::new(UPDATE_RELEASE_LINK_ID)
                                    .link()
                                    .xsmall()
                                    .icon(Icon::new(IconName::ExternalLink))
                                    .tooltip(page_url.clone())
                                    .on_click(move |_, _window, cx| cx.open_url(&url)),
                            )
                        }),
                )
                .child(
                    Label::new(format!("{} → {}", info.current, info.version))
                        .text_xs()
                        .font_family(mono)
                        .text_color(muted),
                ),
        )
        .when(!notes.trim().is_empty(), |this| this.child(notes_area(notes, cx)))
        .child(match progress {
            Some((done, total)) => progress_row(done, total, cx).into_any_element(),
            None => actions_row(cx).into_any_element(),
        })
        .into_any_element()
}

/// The changelog as Markdown (it is the GitHub release body), inline when it
/// is short and in a viewport of its own when it is not. The viewport has a
/// definite `h`, never `max_h`: `Scrollable` copies `max_h` onto its content
/// as well, which then never has anything to scroll (CLAUDE.md).
fn notes_area(notes: String, cx: &App) -> AnyElement {
    let long = notes.lines().count() > LONG_NOTES_LINES || notes.chars().count() > LONG_NOTES_CHARS;
    let text = TextView::markdown("zedis-update-notes", notes).style(release_notes_style());
    let container = div()
        .w_full()
        .rounded(cx.theme().radius)
        .bg(card_background(cx))
        .border_1()
        .border_color(cx.theme().border)
        .overflow_hidden();
    if long {
        container
            .child(div().w_full().h(rems(17.5)).p_2p5().child(text).overflow_y_scrollbar())
            .into_any_element()
    } else {
        container.p_2p5().child(text).into_any_element()
    }
}

fn release_notes_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, base: Pixels| match level {
            1 => base * 1.25,
            2 => base * 1.15,
            3 => base * 1.05,
            _ => base,
        })
}

fn actions_row(cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .child(
            Button::new(UPDATE_SKIP_ID)
                .outline()
                .small()
                .label(i18n_update(cx, "skip_version"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(UpdateAction::Skip), cx)),
        )
        .child(
            Button::new(UPDATE_INSTALL_ID)
                .primary()
                .small()
                .label(i18n_update(cx, "update_and_restart"))
                // The row turns into the progress bar on the click itself
                // (`start_download` publishes 0% before it awaits anything),
                // so there is nothing left to click twice.
                .on_click(|_, window, cx| window.dispatch_action(Box::new(UpdateAction::Install), cx)),
        )
}

/// During the download: bar, percent and bytes, where the buttons were.
fn progress_row(done: u64, total: u64, cx: &App) -> impl IntoElement {
    let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
    let muted = cx.theme().muted_foreground;
    let mono: SharedString = get_mono_font_family().into();
    v_flex()
        .w_full()
        .gap_1p5()
        .child(Progress::new(UPDATE_PROGRESS_ID).value(pct as f32))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(Label::new(i18n_update(cx, "downloading")).text_xs())
                        .child(
                            Label::new(format!("{pct}%"))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted),
                        ),
                )
                .child(
                    Label::new(format!(
                        "{} / {}",
                        format_size(done, DECIMAL),
                        format_size(total, DECIMAL)
                    ))
                    .text_xs()
                    .font_family(mono)
                    .text_color(muted),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::super::title_bar::TITLE_SERVER_IDENTITY_ID;
    use super::{UPDATE_CHIP_ID, UPDATE_INSTALL_ID, UPDATE_PROGRESS_ID, UPDATE_RELEASE_LINK_ID, UPDATE_SKIP_ID};
    use crate::connection::{RedisServer, get_servers, set_servers_cache};
    use crate::helpers::{UpdateAction, UpdateInfo};
    use crate::states::{Route, ZedisGlobalStore};
    use crate::test_support::{hold_the_server_list, init_ui};
    use crate::views::ZedisTitleBar;
    use gpui::{AnyWindowHandle, AppContext, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use std::{cell::Cell, rc::Rc};

    fn release() -> UpdateInfo {
        UpdateInfo {
            version: "9.9.9".to_string(),
            current: "0.15.1".to_string(),
            page_url: "https://example.invalid/release".to_string(),
            notes: "## 9.9.9\n\n- One thing changed".to_string(),
            asset: None,
        }
    }

    /// A title bar with `release()` behind its chip.
    fn title_bar_with_an_update(cx: &mut TestAppContext) -> AnyWindowHandle {
        init_ui(cx);
        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_available_update(Some(release()), cx));
        });
        let handle = cx.open_window(size(px(1100.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisTitleBar::new(window, cx));
            Root::new(view, window, cx)
        });
        handle.into()
    }

    fn set_prompt_open(open: bool, cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_update_prompt_open(open, cx));
        });
        cx.run_until_parked();
    }

    /// A click on the chip asks for the release and opens nothing itself:
    /// the popover appears when the answer says so, with both buttons and the
    /// release link, while the trigger chip keeps its selected state.
    #[gpui::test]
    fn the_chip_asks_first_and_the_answer_opens_the_popover(cx: &mut TestAppContext) {
        let window = title_bar_with_an_update(cx);
        let asked = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let asked = asked.clone();
            cx.on_action(move |action: &UpdateAction, _| {
                if *action == UpdateAction::OpenPrompt {
                    asked.set(asked.get() + 1);
                }
            });
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(UPDATE_CHIP_ID).description().is_some());
            window.click(UPDATE_CHIP_ID, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(UPDATE_SKIP_ID).is_none(),
                "nothing opens before the answer"
            );
        })
        .expect("the window is open");
        assert_eq!(asked.get(), 1);

        // The check's answer: what `check_for_updates` does with `then_prompt`.
        set_prompt_open(true, cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(UPDATE_CHIP_ID).description().is_none());
            assert!(window.find(UPDATE_RELEASE_LINK_ID).visible());
            assert!(window.find(UPDATE_SKIP_ID).visible());
            assert!(window.find(UPDATE_INSTALL_ID).visible());
            assert!(window.try_find(UPDATE_PROGRESS_ID).is_none());
        })
        .expect("the window is open");
    }

    /// When `page_url` is blank, the external release link is omitted cleanly.
    #[gpui::test]
    fn empty_release_url_hides_release_link(cx: &mut TestAppContext) {
        init_ui(cx);
        cx.update(|cx| {
            let mut info = release();
            info.page_url.clear();
            let store = cx.global::<ZedisGlobalStore>().clone();
            store.update(cx, |state, cx| {
                state.set_available_update(Some(info), cx);
                state.set_update_prompt_open(true, cx);
            });
        });
        let handle = cx.open_window(size(px(1100.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisTitleBar::new(window, cx));
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(UPDATE_SKIP_ID).visible());
            assert!(window.try_find(UPDATE_RELEASE_LINK_ID).is_none());
        })
        .expect("the window is open");
    }

    /// The centered server identity renders only on non-Home routes with a known server.
    #[gpui::test]
    fn server_identity_follows_route_and_selection(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let before = get_servers().unwrap_or_default();
        set_servers_cache(vec![RedisServer {
            id: "prod-1".to_string(),
            name: "prod-cache".to_string(),
            host: "10.0.0.1".to_string(),
            port: 6379,
            tag: Some("PROD".to_string()),
            tag_color: Some("magenta".to_string()),
            ..Default::default()
        }]);
        let handle = cx.open_window(size(px(1100.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisTitleBar::new(window, cx));
            Root::new(view, window, cx)
        });
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(TITLE_SERVER_IDENTITY_ID).is_none(),
                "Home route stays server-agnostic"
            );
        })
        .expect("the window is open");

        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.connect_server("prod-1".into(), 2, cx));
        });
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(TITLE_SERVER_IDENTITY_ID).visible());
        })
        .expect("the window is open");

        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.go_to(Route::Home, cx));
        });
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(TITLE_SERVER_IDENTITY_ID).is_none());
        })
        .expect("the window is open");

        set_servers_cache(before);
    }

    /// Escape closes it, and the store hears so — the chip's next click is
    /// a fresh question, not a popover that thinks it is still open.
    #[gpui::test]
    fn escape_closes_the_popover_in_the_store(cx: &mut TestAppContext) {
        let window = title_bar_with_an_update(cx);
        set_prompt_open(true, cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(UPDATE_INSTALL_ID).visible());
            window.press("escape", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(UPDATE_INSTALL_ID).is_none());
        })
        .expect("the window is open");
        cx.update(|cx| assert!(!cx.global::<ZedisGlobalStore>().read(cx).update_prompt_open()));
    }

    /// Once the download runs, its progress takes the buttons' place.
    #[gpui::test]
    fn the_download_takes_the_buttons_place(cx: &mut TestAppContext) {
        let window = title_bar_with_an_update(cx);
        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_download_progress(Some((5, 10)), cx));
        });
        set_prompt_open(true, cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(UPDATE_PROGRESS_ID).visible());
            assert!(window.try_find(UPDATE_INSTALL_ID).is_none());
            assert!(window.try_find(UPDATE_SKIP_ID).is_none());
        })
        .expect("the window is open");
    }
}
