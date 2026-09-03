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

//! Side-by-side value diff view.
//!
//! Pops in when the editor opens a `DiffSession`. Two read-only panes
//! show the chosen reference version (left) and a snapshot of the
//! current editor value (right). For RedisJSON / JSON-shaped String
//! values we also render an RFC 7396 merge-patch block underneath,
//! because that's exactly what the Save path would send as `JSON.MERGE`
//! — giving the user a server-side preview.
//!
//! Both panes are deliberately read-only for v1. The save flow already
//! has a "review history → restore → save" path that's better suited
//! to editing; the diff view's purpose is *understanding the change*,
//! not making one.

use crate::helpers::{
    DiffOp, ValueDiffAction, format_duration, get_mono_font_family, humanize_keystroke, line_diff, unix_ts,
};
use crate::states::{ZedisGlobalStore, i18n_editor, json_merge_diff};
// Sibling-relative path: `editor` is a private child of `views`, so
// the crate-rooted path doesn't resolve. As siblings under the same
// parent module we can reach it via `super::editor`.
use super::editor::DiffSession;
use gpui::{FocusHandle, ScrollHandle, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use serde_json::Value as JsonValue;
use std::sync::Arc;
use std::time::Duration;

/// Boxed close-callback shared between the editor (which constructs it
/// from a `WeakEntity<ZedisEditor>`) and the diff view's Close button.
/// Aliased to silence `clippy::type_complexity` and to let both sites
/// reference the same signature without drifting.
pub type DiffCloseCallback = Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static>;

/// Everything derivable from the (immutable) `DiffSession` bytes, computed
/// once in [`ZedisValueDiff::new`]. The view repaints on scroll / hover /
/// focus, and re-running decode + JSON parse + the O(n·m) LCS per frame on
/// a multi-MB value made every repaint pay for a diff that never changes.
struct DiffComputation {
    identical: bool,
    ops: Vec<DiffOp>,
    left_lines: Vec<SharedString>,
    right_lines: Vec<SharedString>,
    /// Merge-patch block state: `None` — not applicable (non-JSON session
    /// or an unparsable side); `Some(None)` — applicable but the patch is
    /// empty; `Some(Some(body))` — the pretty-printed RFC 7396 patch.
    merge_patch: Option<Option<SharedString>>,
}

impl DiffComputation {
    fn new(session: &DiffSession) -> Self {
        // UTF-8 lossy decode — diff is text-oriented; binary keys that
        // happen to carry invalid UTF-8 sequences get `U+FFFD` substitutes
        // in the affected lines. The Hex view round-trip stays inside the
        // bytes editor (which is the proper place for byte-level
        // inspection), so we trade a touch of fidelity for far simpler
        // diff rendering.
        let left_raw = String::from_utf8_lossy(&session.reference_bytes).into_owned();
        let right_raw = String::from_utf8_lossy(&session.current_bytes).into_owned();

        // Parse both sides as JSON regardless of the session's `is_json`
        // flag — a plain String key that happens to hold JSON still
        // benefits from line-aligned pretty printing on both sides. The
        // parsed values feed both the pretty-print and the merge patch, so
        // each side is parsed exactly once.
        let parsed_left = serde_json::from_str::<JsonValue>(&left_raw).ok();
        let parsed_right = serde_json::from_str::<JsonValue>(&right_raw).ok();

        // Pretty-print JSON to maximise line-level diff signal — a single
        // minified blob would collapse all changes onto one line. Only
        // reformat if both sides parse, so a half-broken JSON value still
        // renders raw bytes rather than silently losing content.
        let (left, right) = match (&parsed_left, &parsed_right) {
            (Some(l), Some(r)) => (
                serde_json::to_string_pretty(l).unwrap_or(left_raw),
                serde_json::to_string_pretty(r).unwrap_or(right_raw),
            ),
            _ => (left_raw, right_raw),
        };

        // The merge-patch block stays guarded by `is_json` so we don't
        // suggest `JSON.MERGE` for a plain SET key; a diff against an
        // unparsable side would mislead, so hide rather than fake one.
        let merge_patch = if session.is_json
            && let (Some(l), Some(r)) = (&parsed_left, &parsed_right)
        {
            Some(
                json_merge_diff(l, r).map(|v| SharedString::from(serde_json::to_string_pretty(&v).unwrap_or_default())),
            )
        } else {
            None
        };

        let identical = left == right;
        let ops = if identical {
            Vec::new()
        } else {
            line_diff(&left, &right)
        };
        // Materialise lines as `SharedString` up front so the per-frame row
        // build clones an Arc handle instead of re-allocating each line.
        let to_lines = |s: &str| s.lines().map(|l| SharedString::from(l.to_string())).collect();

        Self {
            identical,
            ops,
            left_lines: to_lines(&left),
            right_lines: to_lines(&right),
            merge_patch,
        }
    }
}

pub struct ZedisValueDiff {
    session: Arc<DiffSession>,
    computed: DiffComputation,
    on_close: DiffCloseCallback,
    /// Shared by the scroll viewport and the always-on sibling scrollbar
    /// so the bar tracks the diff body's offset.
    scroll_handle: ScrollHandle,
    /// Focus is grabbed once on first render so the `ValueDiff` key
    /// context joins the dispatch path and Esc closes the view.
    focus_handle: FocusHandle,
    focused: bool,
}

impl ZedisValueDiff {
    pub fn new(session: DiffSession, on_close: DiffCloseCallback, cx: &mut Context<Self>) -> Self {
        let computed = DiffComputation::new(&session);
        Self {
            session: Arc::new(session),
            computed,
            on_close,
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            focused: false,
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // Cross-server diffs carry their own label (the other server's
        // "name / dbN"); history diffs render "vN (3 min ago)".
        let title: SharedString = if let Some(label) = self.session.reference_label.clone() {
            label
        } else {
            let secs_ago = (unix_ts() - self.session.reference_at).max(0) as u64;
            let rel = format_duration(Duration::from_secs(secs_ago));
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            rust_i18n::t!(
                "editor.diff_view_title",
                // History is 0-indexed internally; users read "v1" for the
                // newest entry — convert at display time.
                version = (self.session.history_idx + 1).to_string(),
                ago = rel,
                locale = locale
            )
            .to_string()
            .into()
        };

        let on_close = self.on_close.clone();
        h_flex()
            .w_full()
            .h(px(36.))
            .px_4()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(Label::new(title).font_semibold())
            .child(
                Button::new("value-diff-close")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .label(i18n_editor(cx, "diff_view_close"))
                    // Esc closes too (`ValueDiffAction` in the ValueDiff
                    // key context) — surface that on the visible control.
                    .tooltip(humanize_keystroke("escape"))
                    .on_click(move |_, w, cx| on_close(w, cx)),
            )
    }

    /// Render one side of the diff (left or right). `ops` is the full
    /// op list; we walk it and pick which subset belongs on this side.
    fn render_pane(
        &self,
        title: SharedString,
        lines: &[SharedString],
        ops: &[DiffOp],
        is_left: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let added_bg = theme.green.opacity(0.18);
        let removed_bg = theme.red.opacity(0.18);
        let transparent = theme.background.opacity(0.0);

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(ops.len());
        for op in ops {
            let (idx_opt, bg, prefix) = match op {
                DiffOp::Equal(li, ri) => {
                    let idx = if is_left { Some(*li) } else { Some(*ri) };
                    (idx, transparent, ' ')
                }
                DiffOp::Delete(li) => {
                    // Left-side-only line. Right pane shows a padding row
                    // so vertical alignment is preserved.
                    if is_left {
                        (Some(*li), removed_bg, '-')
                    } else {
                        (None, transparent, ' ')
                    }
                }
                DiffOp::Insert(ri) => {
                    if is_left {
                        (None, transparent, ' ')
                    } else {
                        (Some(*ri), added_bg, '+')
                    }
                }
            };

            let line_text: SharedString = match idx_opt {
                Some(idx) => lines.get(idx).cloned().unwrap_or_default(),
                None => SharedString::default(),
            };
            let line_no: SharedString = match idx_opt {
                Some(idx) => format!("{:>4}", idx + 1).into(),
                None => "    ".into(),
            };

            rows.push(
                h_flex()
                    .w_full()
                    .px_2()
                    .bg(bg)
                    .child(Label::new(line_no).text_xs().text_color(theme.muted_foreground))
                    .child(
                        Label::new(SharedString::from(prefix.to_string()))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .mx_1(),
                    )
                    .child(
                        Label::new(line_text)
                            .text_xs()
                            .font_family(get_mono_font_family())
                            .flex_1(),
                    )
                    .into_any_element(),
            );
        }

        // The pane is intentionally natural-height (no inner scroll): the
        // whole diff is wrapped in a single outer scroll region by the
        // caller. `gpui-component`'s `overflow_y_scrollbar` rebuilds the
        // element inside its own `size_full` wrapper and drops flex
        // modifiers, so a per-pane scroller nested inside the side-by-side
        // `size_full` row collapses its height chain and never scrolls.
        // One outer scroller also keeps both panes row-aligned for free,
        // since `line_diff` already pads both sides to equal line counts.
        v_flex()
            .flex_1()
            .min_w(px(200.))
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.3))
                    .child(Label::new(title).text_xs().text_color(theme.muted_foreground)),
            )
            .child(v_flex().w_full().children(rows))
            .into_any_element()
    }

    /// Optional RFC 7396 merge patch block — only rendered when the
    /// session was opened on a key Redis identified as JSON, AND both
    /// snapshots parse cleanly (precomputed in [`DiffComputation`]).
    fn render_merge_patch_block(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let patch = self.computed.merge_patch.as_ref()?;
        let theme = cx.theme();

        let label = i18n_editor(cx, "diff_patch_label");
        let body: SharedString = match patch {
            None => i18n_editor(cx, "diff_patch_empty"),
            Some(body) => body.clone(),
        };
        Some(
            v_flex()
                .w_full()
                .gap_1()
                .px_2()
                .pb_2()
                .child(Label::new(label).text_xs().text_color(theme.muted_foreground))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .bg(theme.muted.opacity(0.15))
                        .child(
                            Label::new(body)
                                .text_xs()
                                .font_family(get_mono_font_family())
                                .whitespace_normal(),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for ZedisValueDiff {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Grab focus on first paint so Esc (scoped to the `ValueDiff` key
        // context below) closes the diff like the Close button does.
        if !self.focused {
            self.focused = true;
            self.focus_handle.focus(window, cx);
        }
        let theme = cx.theme();
        // Decode / JSON parse / LCS all live in `self.computed` — the
        // session bytes never change after construction, so render only
        // lays out the precomputed rows.
        let identical = self.computed.identical;

        let body: gpui::AnyElement = if identical {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(Label::new(i18n_editor(cx, "diff_identical")).text_color(theme.muted_foreground))
                .into_any_element()
        } else {
            let left_title: SharedString = if let Some(label) = self.session.reference_label.clone() {
                label
            } else {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                rust_i18n::t!(
                    "editor.diff_reference_label",
                    version = (self.session.history_idx + 1).to_string(),
                    locale = locale
                )
                .to_string()
                .into()
            };
            let right_title = i18n_editor(cx, "diff_current_label");

            let left_pane = self.render_pane(left_title, &self.computed.left_lines, &self.computed.ops, true, cx);
            let right_pane = self.render_pane(right_title, &self.computed.right_lines, &self.computed.ops, false, cx);

            h_flex()
                .w_full()
                .items_start()
                .gap_2()
                .px_2()
                .pb_2()
                .child(left_pane)
                .child(right_pane)
                .into_any_element()
        };

        let patch_block = if identical {
            None
        } else {
            self.render_merge_patch_block(cx)
        };

        // Single scroll region for the whole diff: header stays pinned at
        // the top, while the side-by-side panes and the optional merge-patch
        // block scroll together below it. `flex_1` + `min_h_0` bound the
        // region to the space left under the header so the scrollbar engages
        // instead of the content overflowing the editor.
        //
        // The scrollbar is an explicit, absolutely-positioned sibling set to
        // `ScrollbarMode::Always` (the theme default is `Scrolling`, which
        // only flashes the bar briefly after the offset changes — so the diff
        // appeared to have no scrollbar). Both the viewport and the bar share
        // `self.scroll_handle`.
        v_flex()
            .key_context("ValueDiff")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ValueDiffAction, window, cx| {
                (this.on_close)(window, cx);
            }))
            .size_full()
            .child(self.render_header(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("value-diff-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(
                                v_flex()
                                    .w_full()
                                    .child(body)
                                    .when_some(patch_block, |this, b| this.child(b)),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .child(Scrollbar::vertical(&self.scroll_handle).mode(ScrollbarMode::Always)),
                    ),
            )
            .into_any_element()
    }
}
