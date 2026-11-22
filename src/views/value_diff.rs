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

//! Value diff view.
//!
//! Pops in when the editor opens a `DiffSession`: the chosen reference
//! version (the left pane, the `-` lines) against a snapshot of the current
//! editor value (the right pane, the `+` lines). For RedisJSON / JSON-shaped
//! String values an RFC 7396 merge-patch block sits underneath, because
//! that's exactly what the Save path would send as `JSON.MERGE` — giving the
//! user a server-side preview.
//!
//! Read-only on purpose. The save flow already has a "review history →
//! restore → save" path that's better suited to editing; the diff view's
//! purpose is *understanding the change*, not making one.
//!
//! The comparing is ours and the drawing is gpui-kit's. `zedis_core::diff`
//! finds the changed lines — a diff built for one edit in 120,000 lines,
//! which is what a 1 MiB JSON document pretty-prints to — and `whole_patch`
//! writes both texts out as one unified patch with every line in it. The
//! `Diff` component draws that as a virtualized list: side by side or
//! unified, the unchanged stretches folded, a long line wrapped, the words
//! that changed within a line marked, JSON coloured. Everything before the
//! first frame (decode, JSON parse and pretty-print, the diff, the patch and
//! its parse) is computed once, off the UI thread ([`DiffData`]).
//!
//! The view drew its own rows before the component existed. What it still
//! lists by hand is the merge-patch block, which is plain text and — a merge
//! patch replacing an array whole — can be as long as the value.

use super::editor::DiffSession;
use crate::helpers::{
    ValueDiffAction, format_duration, get_mono_font_family, group_thousands, humanize_keystroke, unix_ts, whole_patch,
    wrap_line,
};
use crate::states::{ZedisGlobalStore, i18n_editor, json_merge_diff, update_app_state_and_save_quiet};
use gpui::{
    App, Bounds, Entity, FocusHandle, Focusable, Hsla, Pixels, Rems, SharedString, Task, UniformListScrollHandle,
    Window, canvas, div, font, prelude::*, px, rems, uniform_list,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, StyledExt,
    button::{Button, ButtonGroup, ButtonVariants},
    diff::{Diff, DiffFile, DiffHunkSeparator, DiffInlineUnit, DiffMode, DiffState},
    h_flex,
    label::Label,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use serde_json::Value as JsonValue;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

/// Boxed close-callback shared between the editor (which constructs it
/// from a `WeakEntity<ZedisEditor>`) and the diff view's Close button.
/// Aliased to silence `clippy::type_complexity` and to let both sites
/// reference the same signature without drifting.
pub type DiffCloseCallback = Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static>;

/// What the patch calls the value. It has no extension, so the component
/// guesses no language from it: a JSON value is told by name instead.
const PATCH_NAME: &str = "value";
/// Unchanged lines kept beside a change when the rest of a stretch folds.
const FOLD_CONTEXT: usize = 3;
/// A fold hides at least this many lines; fewer stay open.
const FOLD_MIN_HIDDEN: usize = 4;

/// Every row of the merge-patch block: `uniform_list` sizes all rows from
/// the first.
const ROW_HEIGHT: Rems = Rems(1.375);
/// The text size of those rows (`text_xs`), which their columns are
/// measured in.
const TEXT_SIZE: Rems = Rems(0.75);
/// Horizontal padding inside a row, either side.
const ROW_PADDING: Pixels = px(8.);
/// Room kept free on the right for the scrollbar that overlays the list.
const SCROLLBAR_ROOM: Pixels = px(12.);
/// Columns before any measurement arrived, and the fewest the block gets.
const FALLBACK_COLUMNS: usize = 160;
const MIN_COLUMNS: usize = 16;
/// Rows of the merge patch on screen at once; the rest scroll.
const MERGE_PATCH_ROWS: usize = 8;

/// Everything derivable from the (immutable) `DiffSession` bytes, computed
/// once, off the UI thread: decode, JSON parse and pretty-print, the line
/// diff, the patch it is written out as, and that patch parsed into what the
/// component draws.
struct DiffData {
    identical: bool,
    /// The two texts as the component's one file. Empty when they are
    /// identical, or when the patch did not parse (`failure` says why).
    files: Vec<DiffFile>,
    failure: Option<SharedString>,
    /// Lines on the left that are not on the right, and the other way round.
    removed: usize,
    added: usize,
    /// Runs of changed lines: what previous / next change step through.
    changes: usize,
    /// Merge-patch block state: `None` — not applicable (non-JSON session
    /// or an unparsable side); `Some(None)` — applicable but the patch is
    /// empty; `Some(Some(lines))` — the pretty-printed RFC 7396 patch.
    merge_patch: Option<Option<Vec<SharedString>>>,
}

impl DiffData {
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
        let both_json = parsed_left.is_some() && parsed_right.is_some();
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
            Some(json_merge_diff(l, r).map(|patch| to_lines(&serde_json::to_string_pretty(&patch).unwrap_or_default())))
        } else {
            None
        };

        let mut data = Self {
            identical: left == right,
            files: Vec::new(),
            failure: None,
            removed: 0,
            added: 0,
            changes: 0,
            merge_patch,
        };
        if data.identical {
            return data;
        }
        let patch = whole_patch(PATCH_NAME, &left, &right);
        (data.removed, data.added, data.changes) = (patch.removed, patch.added, patch.changes);
        match DiffFile::parse(&patch.text) {
            // Pretty-printed JSON on both sides is coloured as JSON; anything
            // else is plain text, whatever the key's type says.
            Ok(files) if both_json => data.files = files.into_iter().map(|file| file.with_language("json")).collect(),
            Ok(files) => data.files = files,
            // `whole_patch` writes what this parser reads, so this is a bug
            // in one of them: say so in place of a blank pane.
            Err(e) => {
                error!(error = %e, "value diff: the patch did not parse");
                data.failure = Some(e.to_string().into());
            }
        }
        data
    }
}

/// Lines as `SharedString`, so a visible row clones an Arc handle instead
/// of allocating its text on every frame.
fn to_lines(text: &str) -> Vec<SharedString> {
    text.lines().map(|line| SharedString::from(line.to_string())).collect()
}

/// The merge-patch block: its lines, cut into the rows of one width.
struct MergePatch {
    lines: Arc<Vec<SharedString>>,
    /// A piece of a line per row: `(line, bytes of it)`. A line too wide
    /// for the block continues on the rows below it.
    rows: Arc<Vec<(u32, Range<u32>)>>,
    columns: usize,
    /// The columns the last frame measured (written from the list's
    /// prepaint, where its width is known).
    measured: Rc<Cell<Option<usize>>>,
    scroll_handle: UniformListScrollHandle,
}

impl MergePatch {
    /// `lines` of the patch; none at all is the empty patch, `{}`.
    fn new(lines: Vec<SharedString>) -> Self {
        let mut patch = Self {
            lines: Arc::new(lines),
            rows: Arc::default(),
            columns: 0,
            measured: Rc::new(Cell::new(None)),
            scroll_handle: UniformListScrollHandle::new(),
        };
        patch.lay(FALLBACK_COLUMNS);
        patch
    }

    /// Cut the lines into rows `columns` wide.
    fn lay(&mut self, columns: usize) {
        let mut rows = Vec::with_capacity(self.lines.len());
        for (line, text) in self.lines.iter().enumerate() {
            rows.extend(
                wrap_line(text, columns)
                    .into_iter()
                    .map(|range| (line as u32, range.start as u32..range.end as u32)),
            );
        }
        self.rows = Arc::new(rows);
        self.columns = columns;
    }
}

/// The width of one character of the merge patch's monospace text.
fn char_width(window: &Window) -> Pixels {
    let size = TEXT_SIZE.to_pixels(window.rem_size());
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&font(get_mono_font_family()));
    text_system.ch_advance(font_id, size).unwrap_or(size * 0.6)
}

/// Columns for a block `width` wide, less its padding and the scrollbar.
fn columns_for(width: Pixels, char_width: Pixels) -> usize {
    let usable = (width - SCROLLBAR_ROOM - ROW_PADDING * 2.).max(px(0.));
    ((usable / char_width).floor().max(0.) as usize).max(MIN_COLUMNS)
}

/// The part of `line` a row shows: the whole line (an Arc clone) when that
/// is all of it, else a copy of the piece.
fn piece(line: &SharedString, range: &Range<u32>) -> SharedString {
    let range = range.start as usize..range.end as usize;
    if range.start == 0 && range.end == line.len() {
        line.clone()
    } else {
        line.get(range)
            .map(|text| SharedString::from(text.to_string()))
            .unwrap_or_default()
    }
}

pub struct ZedisValueDiff {
    session: Arc<DiffSession>,
    /// `None` while the diff is being computed.
    data: Option<DiffData>,
    _compute: Task<()>,
    /// The component's state, once there is a patch to show: its files, the
    /// folds the user opened, where it is scrolled to.
    state: Option<Entity<DiffState>>,
    mode: DiffMode,
    merge_patch: Option<MergePatch>,
    /// Focus is grabbed once on first render so the `ValueDiff` key
    /// context joins the dispatch path and Esc closes the view.
    focus_handle: FocusHandle,
    focused: bool,
    /// The patch has just arrived: hand the focus on to it, so the arrow
    /// and page keys scroll without a click first.
    focus_patch: bool,
    on_close: DiffCloseCallback,
}

impl ZedisValueDiff {
    pub fn new(session: DiffSession, on_close: DiffCloseCallback, cx: &mut Context<Self>) -> Self {
        let session = Arc::new(session);
        let for_task = session.clone();
        let compute = cx.spawn(async move |this, cx| {
            let data = cx.background_spawn(async move { DiffData::new(&for_task) }).await;
            let _ = this.update(cx, |this, cx| this.computed(data, cx));
        });
        Self {
            session,
            data: None,
            _compute: compute,
            state: None,
            // The layout it was last left in, side by side the first time.
            mode: if cx.global::<ZedisGlobalStore>().read(cx).diff_unified() {
                DiffMode::Unified
            } else {
                DiffMode::Split
            },
            merge_patch: None,
            focus_handle: cx.focus_handle(),
            focused: false,
            focus_patch: false,
            on_close,
        }
    }

    /// The diff is ready: give the component its files and keep the rest.
    fn computed(&mut self, mut data: DiffData, cx: &mut Context<Self>) {
        let files = std::mem::take(&mut data.files);
        if !files.is_empty() {
            let mode = self.mode;
            self.state = Some(cx.new(|cx| {
                DiffState::new(files, cx)
                    .with_mode(mode)
                    .with_context_lines(Some(FOLD_CONTEXT))
                    .with_min_collapsed_lines(FOLD_MIN_HIDDEN)
                    .with_inline_unit(Some(DiffInlineUnit::Word))
            }));
            self.focus_patch = true;
        }
        // Identical values show one line saying so, and no patch under it.
        self.merge_patch = data
            .merge_patch
            .take()
            .filter(|_| !data.identical)
            .map(|lines| MergePatch::new(lines.unwrap_or_default()));
        self.data = Some(data);
        cx.notify();
    }

    fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if let Some(state) = &self.state {
            state.update(cx, |state, cx| state.set_mode(mode, cx));
        }
        // Remembered: the next diff opens in it, as the terminal keeps its
        // reply format.
        let unified = mode == DiffMode::Unified;
        update_app_state_and_save_quiet(cx, "save_diff_layout", move |state, _| {
            state.set_diff_unified(unified);
        });
        cx.notify();
    }

    /// Scroll to the previous or next change, from what is on screen.
    fn go_to_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        state.update(cx, |state, cx| {
            if forward {
                state.next_change(cx);
            } else {
                state.previous_change(cx);
            }
        });
    }

    /// The merge patch's width fits a different number of characters.
    fn remeasured(&mut self, cx: &mut Context<Self>) {
        let Some(patch) = self.merge_patch.as_mut() else {
            return;
        };
        if let Some(columns) = patch.measured.get()
            && columns != patch.columns
        {
            patch.lay(columns);
            cx.notify();
        }
    }

    /// The reference's name: the other server's "name / dbN" for a
    /// cross-server diff, "vN (reference)" for a version from the history.
    fn reference_label(&self, cx: &App) -> SharedString {
        if let Some(label) = self.session.reference_label.clone() {
            return label;
        }
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        rust_i18n::t!(
            "editor.diff_reference_label",
            // History is 0-indexed internally; users read "v1" for the
            // newest entry — convert at display time.
            version = (self.session.history_idx + 1).to_string(),
            locale = locale
        )
        .to_string()
        .into()
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
                version = (self.session.history_idx + 1).to_string(),
                ago = rel,
                locale = locale
            )
            .to_string()
            .into()
        };

        // What changed, and how many places there are to step through.
        let summary = self
            .data
            .as_ref()
            .filter(|_| self.state.is_some())
            .map(|data| (data.removed, data.added, data.changes));
        let prev_tooltip = format!(
            "{} ({})",
            i18n_editor(cx, "diff_prev_change"),
            humanize_keystroke("shift-f7")
        );
        let next_tooltip = format!("{} ({})", i18n_editor(cx, "diff_next_change"), humanize_keystroke("f7"));
        let unified = self.mode == DiffMode::Unified;

        let on_close = self.on_close.clone();
        h_flex()
            .w_full()
            .h(px(36.))
            .px_4()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(Label::new(title).font_semibold().truncate().flex_1().min_w_0())
            .when_some(summary, |this, (removed, added, changes)| {
                let can_move = changes > 0;
                this.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .font_family(get_mono_font_family())
                        .child(
                            div()
                                .text_color(theme.red)
                                .child(format!("−{}", group_thousands(removed as u64))),
                        )
                        .child(
                            div()
                                .text_color(theme.green)
                                .child(format!("+{}", group_thousands(added as u64))),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Button::new("value-diff-prev")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronUp)
                                .disabled(!can_move)
                                .tooltip(prev_tooltip)
                                .on_click(cx.listener(|this, _, _, cx| this.go_to_change(false, cx))),
                        )
                        // How many changes there are to step through.
                        .child(
                            h_flex().min_w(rems(2.)).justify_center().child(
                                Label::new(group_thousands(changes as u64))
                                    .text_xs()
                                    .font_family(get_mono_font_family())
                                    .text_color(theme.muted_foreground),
                            ),
                        )
                        .child(
                            Button::new("value-diff-next")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronDown)
                                .disabled(!can_move)
                                .tooltip(next_tooltip)
                                .on_click(cx.listener(|this, _, _, cx| this.go_to_change(true, cx))),
                        ),
                )
                // Side by side or one column: the segmented control the JSON
                // view switch uses, the chosen one painted pressed.
                .child(
                    ButtonGroup::new("value-diff-layout")
                        .compact()
                        .small()
                        .outline()
                        .child(
                            Button::new("value-diff-split")
                                .label(i18n_editor(cx, "diff_layout_split"))
                                .selected(!unified),
                        )
                        .child(
                            Button::new("value-diff-unified")
                                .label(i18n_editor(cx, "diff_layout_unified"))
                                .selected(unified),
                        )
                        .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                            if let Some(ix) = clicks.first() {
                                let mode = if *ix == 1 { DiffMode::Unified } else { DiffMode::Split };
                                this.set_mode(mode, cx);
                            }
                        })),
                )
            })
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

    /// Which side is which: the reference behind the `−` lines and the
    /// editor's snapshot behind the `+` ones. Side by side they sit over
    /// their panes; in one column they are the legend for the two marks.
    fn render_side_titles(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let title = |mark: &'static str, color: Hsla, text: SharedString| {
            h_flex()
                .id(SharedString::from(format!("diff-side-title-{mark}")))
                .flex_1()
                .min_w_0()
                .px_3()
                .py_1()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(get_mono_font_family())
                        .text_color(color)
                        .child(mark),
                )
                .child(Label::new(text).text_xs().text_color(muted).truncate())
        };
        h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .child(title("−", theme.red, self.reference_label(cx)))
            .child(div().w(px(1.)).h_full().bg(theme.border))
            .child(title("+", theme.green, i18n_editor(cx, "diff_current_label")))
    }

    /// The merge-patch block: a caption, then the patch's lines in a list of
    /// their own, a few rows tall, under the diff.
    fn render_merge_patch(&self, patch: &MergePatch, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let row = || {
            h_flex()
                .w_full()
                .h(ROW_HEIGHT)
                .flex_none()
                .items_center()
                .px(ROW_PADDING)
                .text_xs()
                .font_family(get_mono_font_family())
                .whitespace_nowrap()
        };
        let caption = row()
            .border_t_1()
            .border_color(theme.border)
            .text_color(theme.muted_foreground)
            .child(i18n_editor(cx, "diff_patch_label"));
        let patch_bg = theme.muted.opacity(0.15);
        if patch.rows.is_empty() {
            return v_flex()
                .w_full()
                .flex_none()
                .child(caption)
                .child(row().bg(patch_bg).child(i18n_editor(cx, "diff_patch_empty")));
        }

        let (lines, rows) = (patch.lines.clone(), patch.rows.clone());
        let list = uniform_list("value-diff-merge-patch", rows.len(), move |range, _window, _cx| {
            range
                .map(|ix| {
                    let (line, bytes) = &rows[ix];
                    let text = lines.get(*line as usize).map(|text| piece(text, bytes));
                    row().pr(SCROLLBAR_ROOM).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(text.unwrap_or_default()),
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&patch.scroll_handle)
        .size_full();

        // The block's width, measured where it is known: after layout. A
        // width that fits a different number of characters cuts the lines
        // again, on the next frame.
        let measured = patch.measured.clone();
        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
                let columns = columns_for(bounds.size.width, char_width(window));
                if measured.get() != Some(columns) {
                    measured.set(Some(columns));
                    window.on_next_frame(move |_, cx| {
                        let _ = view.update(cx, |this, cx| this.remeasured(cx));
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let visible = patch.rows.len().min(MERGE_PATCH_ROWS);
        v_flex().w_full().flex_none().child(caption).child(
            div()
                .relative()
                .w_full()
                .h(rems(ROW_HEIGHT.0 * visible as f32))
                .bg(patch_bg)
                .child(measure)
                .child(list)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(Scrollbar::vertical(&patch.scroll_handle).mode(ScrollbarMode::Always)),
                ),
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
        // Then on to the patch once there is one — only if the focus is
        // still where the line above put it.
        if std::mem::take(&mut self.focus_patch)
            && self.focus_handle.is_focused(window)
            && let Some(state) = &self.state
        {
            state.focus_handle(cx).focus(window, cx);
        }
        let theme = cx.theme();
        let centered = |text: SharedString, color: Hsla| {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(Label::new(text).text_color(color))
                .into_any_element()
        };
        let body = match (&self.data, &self.state) {
            (None, _) => centered(i18n_editor(cx, "diff_computing"), theme.muted_foreground),
            (Some(data), _) if data.identical => centered(i18n_editor(cx, "diff_identical"), theme.muted_foreground),
            (Some(data), None) => centered(data.failure.clone().unwrap_or_default(), theme.danger),
            (Some(_), Some(state)) => {
                let state = state.clone();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_side_titles(cx))
                    .child(
                        v_flex().flex_1().min_h_0().w_full().child(
                            Diff::new(&state)
                                // One value, not a list of files: no file
                                // header, and no `@@ -1,9 +1,9 @@` over it.
                                .header_visible(false)
                                .hunk_separator(DiffHunkSeparator::Simple)
                                .soft_wrap(true),
                        ),
                    )
                    .when_some(self.merge_patch.as_ref(), |this, patch| {
                        this.child(self.render_merge_patch(patch, cx))
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .key_context("ValueDiff")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, action: &ValueDiffAction, window, cx| match action {
                ValueDiffAction::Close => (this.on_close)(window, cx),
                ValueDiffAction::NextChange => this.go_to_change(true, cx),
                ValueDiffAction::PreviousChange => this.go_to_change(false, cx),
            }))
            .size_full()
            .child(self.render_header(cx))
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    fn session(reference: &str, current: &str, is_json: bool) -> DiffSession {
        DiffSession {
            history_idx: 0,
            reference_bytes: bytes::Bytes::from(reference.to_string()),
            reference_at: 0,
            current_bytes: bytes::Bytes::from(current.to_string()),
            is_json,
            reference_label: None,
        }
    }

    /// A value the size of the view's worst case: ~160,000 lines once
    /// pretty-printed. One edit is one line changed on each side, in one
    /// place, and the patch the component is handed holds the rest as
    /// context for it to fold.
    #[test]
    fn a_large_json_value_with_one_edit_is_one_change() {
        let users = |renamed: usize| {
            let users: Vec<JsonValue> = (0..20_000)
                .map(|i| {
                    let name = if i == renamed {
                        format!("renamed-{i}")
                    } else {
                        format!("user-{i}")
                    };
                    serde_json::json!({ "id": i, "name": name, "tags": ["a", "b"] })
                })
                .collect();
            serde_json::to_string(&users).unwrap_or_default()
        };
        let data = DiffData::new(&session(&users(usize::MAX), &users(9_999), true));
        assert!(!data.identical && data.failure.is_none());
        assert_eq!((data.removed, data.added, data.changes), (1, 1, 1));
        assert_eq!(data.files.len(), 1);
        assert_eq!((data.files[0].deletions(), data.files[0].additions()), (1, 1));
        // A merge patch replaces an array whole (RFC 7396), so this one holds
        // every record: rows of a list like the rest, not one block of text.
        let lines = data.merge_patch.flatten().expect("a merge patch");
        assert!(lines.len() > 100_000, "{} lines", lines.len());
        let patch = MergePatch::new(lines);
        assert_eq!(patch.rows.len(), patch.lines.len(), "none of them needs a second row");
    }

    #[test]
    fn equal_values_are_identical_and_a_plain_string_has_no_patch() {
        let data = DiffData::new(&session("a\nb", "a\nb", false));
        assert!(data.identical && data.files.is_empty());
        assert!(data.merge_patch.is_none());
        let data = DiffData::new(&session("{\"a\":1}", "{\"a\":1}", true));
        assert!(data.identical);
        assert_eq!(data.merge_patch, Some(None));
    }

    /// The patch is ours and the parser is the component's: whatever a value
    /// holds, what one writes the other has to read back as one file with
    /// the same counts. A value may be patch syntax line for line, end in
    /// CRLF on one side only, or lack its last newline.
    #[test]
    fn whatever_the_value_holds_its_patch_parses() {
        let hostile = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n\\ No newline at end of file\ndiff --git a/x b/x\n\n-\n+\n \n";
        for (left, right, removed, added) in [
            (
                hostile,
                "--- a/x\n+++ b/y\n@@ -1 +1 @@\n\\ No newline at end of file\ndiff --git a/x b/x\n\n-\n+\n \n",
                1,
                1,
            ),
            ("a\r\nb\r\n", "a\nb\r\n", 1, 1),
            ("a\nb", "a\nb\n", 1, 1),
            ("a\nb\n", "a\nb", 1, 1),
            ("", "only\nnow\n", 0, 2),
            ("gone\n", "", 1, 0),
            ("tab\there", "tab\tthere \u{4e2d}\u{6587}", 1, 1),
        ] {
            let data = DiffData::new(&session(left, right, false));
            assert!(data.failure.is_none(), "{left:?} -> {right:?}: {:?}", data.failure);
            assert_eq!(data.files.len(), 1, "{left:?} -> {right:?}");
            assert_eq!(
                (data.files[0].deletions(), data.files[0].additions()),
                (removed, added),
                "{left:?} -> {right:?}"
            );
            assert_eq!((data.removed, data.added), (removed, added));
        }
    }

    #[test]
    fn a_long_line_of_the_merge_patch_continues_below() {
        let long = "x".repeat(50);
        let data = DiffData::new(&session("{\"k\":1}", &format!("{{\"k\":\"{long}\"}}"), true));
        let mut patch = MergePatch::new(data.merge_patch.flatten().expect("a merge patch"));
        assert_eq!(patch.rows.len(), 3, "three short lines at the fallback width");
        // `  "k": "xxx…"` is 59 characters: three rows of twenty.
        patch.lay(20);
        assert_eq!(patch.rows.len(), 5);
        assert_eq!(patch.rows[1], (1, 0..20));
        assert_eq!(patch.rows[3], (1, 40..59));
    }

    #[test]
    fn a_piece_of_a_line_is_its_byte_range() {
        let line = SharedString::from("abcdef");
        assert_eq!(piece(&line, &(0..6)), "abcdef");
        assert_eq!(piece(&line, &(2..4)), "cd");
        assert_eq!(piece(&line, &(4..99)), "");
    }

    #[test]
    fn the_columns_follow_the_width_and_never_vanish() {
        assert!(columns_for(px(1212.), px(7.)) > columns_for(px(400.), px(7.)));
        assert_eq!(columns_for(px(10.), px(7.)), MIN_COLUMNS);
    }

    /// The view in a window: the patch arrives from the background, the
    /// component gets its one file side by side, and the header's switch and
    /// the change keys reach it.
    #[gpui::test]
    fn the_patch_is_shown_side_by_side_and_the_layout_switches(cx: &mut TestAppContext) {
        init_ui(cx);
        let mut view = None;
        let handle = cx.open_window(size(px(1000.), px(700.)), |window, cx| {
            let on_close: DiffCloseCallback = Arc::new(|_, _| {});
            let diff = cx.new(|cx| {
                let session = session("{\"a\":1,\"b\":[1,2]}", "{\"a\":2,\"b\":[1,2]}", true);
                ZedisValueDiff::new(session, on_close, cx)
            });
            view = Some(diff.clone());
            Root::new(diff, window, cx)
        });
        let view = view.expect("the window built the view");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(view.read(cx).state.is_none(), "still comparing");
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let state = view.read(cx).state.clone().expect("a patch to show");
            assert_eq!(state.read(cx).mode(), DiffMode::Split);
            assert_eq!(state.read(cx).files().len(), 1);
            assert!(
                view.read(cx).merge_patch.is_some(),
                "a JSON value shows its merge patch"
            );

            view.update(cx, |view, cx| {
                view.set_mode(DiffMode::Unified, cx);
                view.go_to_change(true, cx);
                view.go_to_change(false, cx);
            });
            assert_eq!(state.read(cx).mode(), DiffMode::Unified);
            window.render_frame(cx);
        })
        .expect("the window is open");

        // The layout is remembered: the next diff opens in it.
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.global::<ZedisGlobalStore>().read(cx).diff_unified());
            let session = session("{\"a\":1}", "{\"a\":2}", true);
            let on_close: DiffCloseCallback = Arc::new(|_, _| {});
            let next = cx.new(|cx| ZedisValueDiff::new(session, on_close, cx));
            assert_eq!(next.read(cx).mode, DiffMode::Unified);
        });
    }
}
