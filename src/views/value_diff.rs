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
//!
//! Built for values of a few MiB — a 1 MiB JSON document pretty-prints to
//! about 120,000 lines. The diff is computed once, off the UI thread
//! ([`DiffData`]); unchanged stretches fold behind one row each, a line too
//! wide for the pane continues on the rows below it, and the rows are a
//! `uniform_list`, so a frame builds the few dozen that are on screen. The
//! previous version built every row of both panes on every repaint.

use crate::helpers::{
    LayoutRow, SideBySideRow, ValueDiffAction, fold_runs, format_duration, get_mono_font_family, group_thousands,
    humanize_keystroke, layout_rows, line_diff, side_by_side, unix_ts, wrap_line,
};
use crate::states::{ZedisGlobalStore, i18n_editor, json_merge_diff};
// Sibling-relative path: `editor` is a private child of `views`, so
// the crate-rooted path doesn't resolve. As siblings under the same
// parent module we can reach it via `super::editor`.
use super::editor::DiffSession;
use gpui::{
    App, Bounds, FocusHandle, Hsla, Pixels, Rems, ScrollStrategy, SharedString, Task, UniformListScrollHandle,
    WeakEntity, Window, canvas, div, font, prelude::*, px, rems, uniform_list,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
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

/// Boxed close-callback shared between the editor (which constructs it
/// from a `WeakEntity<ZedisEditor>`) and the diff view's Close button.
/// Aliased to silence `clippy::type_complexity` and to let both sites
/// reference the same signature without drifting.
pub type DiffCloseCallback = Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static>;

/// Every row's height: `uniform_list` sizes all rows from the first, so a
/// fold, a caption and a line are all this tall.
const ROW_HEIGHT: Rems = Rems(1.375);
/// The text size of every row (`text_xs`), which the column count is
/// measured in.
const TEXT_SIZE: Rems = Rems(0.75);
/// Unchanged rows kept beside a change when the rest of a stretch folds.
const FOLD_CONTEXT: usize = 3;
/// A fold hides at least this many rows; fewer stay open.
const FOLD_MIN_HIDDEN: usize = 4;
/// Horizontal padding inside a row, either side.
const ROW_PADDING: Pixels = px(8.);
/// Room kept free on the right for the scrollbar that overlays the list.
const SCROLLBAR_ROOM: Pixels = px(12.);
/// Columns before any measurement arrived, and the fewest a pane gets.
const FALLBACK_COLUMNS: usize = 80;
const MIN_COLUMNS: usize = 16;

/// Everything derivable from the (immutable) `DiffSession` bytes, computed
/// once, off the UI thread: decode, JSON parse and pretty-print, the line
/// diff, the side-by-side pairing and the fold search. None of it depends on
/// the view's width or on which folds are open.
struct DiffData {
    identical: bool,
    left_lines: Vec<SharedString>,
    right_lines: Vec<SharedString>,
    rows: Vec<SideBySideRow>,
    folds: Vec<Range<usize>>,
    /// Lines on the left that are not on the right, and the other way round.
    removed: usize,
    added: usize,
    /// Digits of the longest line number, for the gutter.
    number_digits: usize,
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

        let identical = left == right;
        let rows = if identical {
            Vec::new()
        } else {
            side_by_side(&line_diff(&left, &right))
        };
        let folds = fold_runs(&rows, FOLD_CONTEXT, FOLD_MIN_HIDDEN);
        let removed = rows.iter().filter(|row| row.changed && row.left.is_some()).count();
        let added = rows.iter().filter(|row| row.changed && row.right.is_some()).count();
        let left_lines = to_lines(&left);
        let right_lines = to_lines(&right);
        let number_digits = left_lines.len().max(right_lines.len()).max(1).to_string().len();
        Self {
            identical,
            left_lines,
            right_lines,
            rows,
            folds,
            removed,
            added,
            number_digits,
            merge_patch,
        }
    }
}

/// Lines as `SharedString`, so a visible row clones an Arc handle instead
/// of allocating its text on every frame.
fn to_lines(text: &str) -> Vec<SharedString> {
    text.lines().map(|line| SharedString::from(line.to_string())).collect()
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ViewRow {
    Diff(LayoutRow),
    /// The merge-patch block's caption.
    PatchCaption,
    /// A piece of a merge-patch line: `line` of the patch, `range` its bytes.
    Patch {
        line: u32,
        range: Range<u32>,
    },
    /// The merge patch is empty: one row that says so.
    PatchEmpty,
}

/// How many characters fit on a row: per pane for the diff, across the whole
/// width for the merge patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    side: usize,
    full: usize,
}

impl Default for Columns {
    fn default() -> Self {
        Self {
            side: FALLBACK_COLUMNS,
            full: FALLBACK_COLUMNS * 2,
        }
    }
}

/// The rows as laid out for one width and one set of open folds.
struct Laid {
    rows: Arc<Vec<ViewRow>>,
    /// The row each change starts on, for previous / next change.
    changes: Vec<usize>,
    columns: Columns,
}

impl Laid {
    fn new(data: &DiffData, expanded: &[bool], columns: Columns) -> Self {
        let layout = layout_rows(
            &data.rows,
            &data.left_lines,
            &data.right_lines,
            &data.folds,
            expanded,
            columns.side,
        );
        let mut rows: Vec<ViewRow> = layout.rows.into_iter().map(ViewRow::Diff).collect();
        match &data.merge_patch {
            None => {}
            Some(None) => rows.extend([ViewRow::PatchCaption, ViewRow::PatchEmpty]),
            Some(Some(lines)) => {
                rows.push(ViewRow::PatchCaption);
                for (line, text) in lines.iter().enumerate() {
                    rows.extend(wrap_line(text, columns.full).into_iter().map(|range| ViewRow::Patch {
                        line: line as u32,
                        range: range.start as u32..range.end as u32,
                    }));
                }
            }
        }
        Self {
            rows: Arc::new(rows),
            changes: layout.changes,
            columns,
        }
    }
}

/// The width of one character of the rows' monospace text.
fn char_width(window: &Window) -> Pixels {
    let size = TEXT_SIZE.to_pixels(window.rem_size());
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&font(get_mono_font_family()));
    text_system.ch_advance(font_id, size).unwrap_or(size * 0.6)
}

/// Columns for a list `width` wide: each pane is half of it, less its line
/// number gutter, its `-` / `+` marker and its padding.
fn columns_for(width: Pixels, char_width: Pixels, number_digits: usize) -> Columns {
    let usable = (width - SCROLLBAR_ROOM).max(px(0.));
    let fit = |text: Pixels| ((text / char_width).floor().max(0.) as usize).max(MIN_COLUMNS);
    let chrome = char_width * (number_digits + 2) as f32 + ROW_PADDING * 3.;
    Columns {
        side: fit(usable / 2. - chrome),
        full: fit(usable - ROW_PADDING * 2.),
    }
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

/// What every row of one frame is drawn with, read from the theme before the
/// list's `'static` closure takes it.
#[derive(Clone)]
struct RowStyle {
    added_bg: Hsla,
    removed_bg: Hsla,
    /// The empty side of a change: a line that exists on the other side only.
    absent_bg: Hsla,
    fold_bg: Hsla,
    fold_hover_bg: Hsla,
    patch_bg: Hsla,
    muted: Hsla,
    border: Hsla,
    gutter: Pixels,
    marker: Pixels,
    fold_label: SharedString,
    patch_caption: SharedString,
    patch_empty: SharedString,
}

pub struct ZedisValueDiff {
    session: Arc<DiffSession>,
    /// `None` while the diff is being computed.
    data: Option<Arc<DiffData>>,
    _compute: Task<()>,
    /// Per fold of `data.folds`, whether the user opened it.
    expanded: Vec<bool>,
    laid: Option<Laid>,
    /// The columns the last frame measured (written from the list's
    /// prepaint, where its width is known).
    measured: Rc<Cell<Option<Columns>>>,
    scroll_handle: UniformListScrollHandle,
    /// The change previous / next last moved to; cleared when the user
    /// scrolls, so the next move starts from what is on screen.
    change_cursor: Option<usize>,
    /// Focus is grabbed once on first render so the `ValueDiff` key
    /// context joins the dispatch path and Esc closes the view.
    focus_handle: FocusHandle,
    focused: bool,
    on_close: DiffCloseCallback,
}

impl ZedisValueDiff {
    pub fn new(session: DiffSession, on_close: DiffCloseCallback, cx: &mut Context<Self>) -> Self {
        let session = Arc::new(session);
        let for_task = session.clone();
        let compute = cx.spawn(async move |this, cx| {
            let data = cx.background_spawn(async move { DiffData::new(&for_task) }).await;
            let _ = this.update(cx, |this, cx| {
                this.expanded = vec![false; data.folds.len()];
                this.data = Some(Arc::new(data));
                this.relayout();
                cx.notify();
            });
        });
        Self {
            session,
            data: None,
            _compute: compute,
            expanded: Vec::new(),
            laid: None,
            measured: Rc::new(Cell::new(None)),
            scroll_handle: UniformListScrollHandle::new(),
            change_cursor: None,
            focus_handle: cx.focus_handle(),
            focused: false,
            on_close,
        }
    }

    /// Lay the rows out again for the measured width and the open folds.
    fn relayout(&mut self) {
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let columns = self.measured.get().unwrap_or_default();
        self.laid = Some(Laid::new(data, &self.expanded, columns));
    }

    /// The width changed enough to fit a different number of characters.
    fn remeasured(&mut self, cx: &mut Context<Self>) {
        if self.laid.as_ref().map(|laid| laid.columns) != self.measured.get() {
            self.relayout();
            cx.notify();
        }
    }

    fn expand_fold(&mut self, fold: usize, cx: &mut Context<Self>) {
        if let Some(open) = self.expanded.get_mut(fold) {
            *open = true;
            self.relayout();
            cx.notify();
        }
    }

    /// The first row on screen, from the scroll offset.
    fn top_row(&self, window: &Window) -> usize {
        let row_height = ROW_HEIGHT.to_pixels(window.rem_size());
        let offset = -self.scroll_handle.0.borrow().base_handle.offset().y;
        (offset / row_height).max(0.) as usize
    }

    /// Scroll to the previous or next change: after the one moved to last,
    /// or — when the user has scrolled since — after the top of the screen.
    fn go_to_change(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(changes) = self.laid.as_ref().map(|laid| &laid.changes) else {
            return;
        };
        let Some(last) = changes.len().checked_sub(1) else {
            return;
        };
        let target = match (self.change_cursor, forward) {
            (Some(at), true) => (at + 1).min(last),
            (Some(at), false) => at.saturating_sub(1),
            (None, true) => {
                let top = self.top_row(window);
                changes.iter().position(|&row| row > top).unwrap_or(last)
            }
            (None, false) => {
                let top = self.top_row(window);
                changes.iter().rposition(|&row| row < top).unwrap_or(0)
            }
        };
        self.change_cursor = Some(target);
        self.scroll_handle
            .scroll_to_item(changes[target], ScrollStrategy::Center);
        cx.notify();
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

        // What changed, and where the user is among the changes.
        let summary = self
            .data
            .as_ref()
            .filter(|data| !data.identical)
            .map(|data| (data.removed, data.added));
        let changes = self.laid.as_ref().map_or(0, |laid| laid.changes.len());
        let position: SharedString = match self.change_cursor {
            Some(at) => format!("{} / {changes}", at + 1),
            None => format!("{changes}"),
        }
        .into();
        let can_move = changes > 0;
        let prev_tooltip = format!(
            "{} ({})",
            i18n_editor(cx, "diff_prev_change"),
            humanize_keystroke("shift-f7")
        );
        let next_tooltip = format!("{} ({})", i18n_editor(cx, "diff_next_change"), humanize_keystroke("f7"));

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
            .when_some(summary, |this, (removed, added)| {
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
                                .on_click(cx.listener(|this, _, window, cx| this.go_to_change(false, window, cx))),
                        )
                        // Wide enough for "12 / 34", so moving between changes
                        // does not shift the buttons under the pointer.
                        .child(
                            h_flex().min_w(rems(4.)).justify_center().child(
                                Label::new(position)
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
                                .on_click(cx.listener(|this, _, window, cx| this.go_to_change(true, window, cx))),
                        ),
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

    /// The two panes' titles, over their halves of the list.
    fn render_pane_titles(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
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
        let title = |text: SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_1()
                .child(Label::new(text).text_xs().text_color(theme.muted_foreground).truncate())
        };
        h_flex()
            .w_full()
            .pr(SCROLLBAR_ROOM)
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .child(title(left_title))
            .child(div().w(px(1.)).h_full().bg(theme.border))
            .child(title(right_title))
            .into_any_element()
    }

    fn row_style(&self, window: &Window, cx: &App) -> RowStyle {
        let theme = cx.theme();
        let ch = char_width(window);
        let digits = self.data.as_ref().map_or(1, |data| data.number_digits);
        RowStyle {
            added_bg: theme.green.opacity(0.18),
            removed_bg: theme.red.opacity(0.18),
            absent_bg: theme.muted.opacity(0.35),
            fold_bg: theme.muted.opacity(0.35),
            fold_hover_bg: theme.muted.opacity(0.7),
            patch_bg: theme.muted.opacity(0.15),
            muted: theme.muted_foreground,
            border: theme.border,
            gutter: ch * digits as f32 + ROW_PADDING,
            marker: ch * 2.,
            fold_label: i18n_editor(cx, "diff_folded"),
            patch_caption: i18n_editor(cx, "diff_patch_label"),
            patch_empty: i18n_editor(cx, "diff_patch_empty"),
        }
    }

    fn render_rows(
        &self,
        laid: &Laid,
        data: &Arc<DiffData>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = laid.rows.clone();
        let data = data.clone();
        let style = self.row_style(window, cx);
        let view = cx.entity().downgrade();
        let measured = self.measured.clone();
        let number_digits = data.number_digits;
        let list = uniform_list("value-diff-rows", rows.len(), move |range, _window, _cx| {
            range
                .map(|ix| render_row(&rows[ix], &data, &style, &view))
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll_handle)
        .size_full();

        // The list's width, measured where it is known: after layout. A
        // width that fits a different number of characters lays the rows
        // out again, on the next frame.
        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
                let columns = columns_for(bounds.size.width, char_width(window), number_digits);
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

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            // A scroll by the user: the next previous / next change starts
            // from the screen, not from the change moved to last.
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.change_cursor = None))
            .child(measure)
            .child(list)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(Scrollbar::vertical(&self.scroll_handle).mode(ScrollbarMode::Always)),
            )
    }
}

/// One row of the list. Every kind is [`ROW_HEIGHT`] tall.
fn render_row(row: &ViewRow, data: &DiffData, style: &RowStyle, view: &WeakEntity<ZedisValueDiff>) -> gpui::AnyElement {
    let base = || {
        h_flex()
            .w_full()
            .h(ROW_HEIGHT)
            .flex_none()
            .items_center()
            .text_xs()
            .font_family(get_mono_font_family())
            .whitespace_nowrap()
            .pr(SCROLLBAR_ROOM)
    };
    match row {
        ViewRow::Diff(LayoutRow::Line {
            row,
            chunk,
            left,
            right,
        }) => {
            let Some(line) = data.rows.get(*row as usize) else {
                return base().into_any_element();
            };
            let first = *chunk == 0;
            let side = |index: Option<u32>,
                        range: &Option<Range<u32>>,
                        lines: &[SharedString],
                        changed_bg: Hsla,
                        mark: &str| {
                let text = index
                    .and_then(|index| lines.get(index as usize))
                    .zip(range.as_ref())
                    .map(|(text, range)| piece(text, range))
                    .unwrap_or_default();
                let number: SharedString = match index {
                    Some(index) if first => (index + 1).to_string().into(),
                    _ => SharedString::default(),
                };
                let bg = match (line.changed, index) {
                    (false, _) => None,
                    (true, Some(_)) => Some(changed_bg),
                    (true, None) => Some(style.absent_bg),
                };
                let marker = if line.changed && first && index.is_some() {
                    mark
                } else {
                    ""
                };
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .when_some(bg, |this, bg| this.bg(bg))
                    .child(
                        div()
                            .flex_none()
                            .w(style.gutter)
                            .pr(ROW_PADDING)
                            .text_right()
                            .text_color(style.muted)
                            .child(number),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(style.marker)
                            .text_color(style.muted)
                            .child(SharedString::from(marker)),
                    )
                    .child(div().flex_1().min_w_0().overflow_hidden().pr(ROW_PADDING).child(text))
            };
            base()
                .child(side(line.left, left, &data.left_lines, style.removed_bg, "-"))
                .child(div().w(px(1.)).h_full().flex_none().bg(style.border))
                .child(side(line.right, right, &data.right_lines, style.added_bg, "+"))
                .into_any_element()
        }
        ViewRow::Diff(LayoutRow::Fold { fold, hidden }) => {
            let fold = *fold as usize;
            let view = view.clone();
            let label = style
                .fold_label
                .replace("%{count}", &group_thousands(u64::from(*hidden)));
            base()
                .id(("value-diff-fold", fold))
                .justify_center()
                .gap_1()
                .bg(style.fold_bg)
                .text_color(style.muted)
                .cursor_pointer()
                .hover({
                    let hover = style.fold_hover_bg;
                    move |this| this.bg(hover)
                })
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.expand_fold(fold, cx));
                })
                .child(Icon::new(IconName::ChevronDown).xsmall())
                .child(label)
                .into_any_element()
        }
        ViewRow::PatchCaption => base()
            .px(ROW_PADDING)
            .border_t_1()
            .border_color(style.border)
            .text_color(style.muted)
            .child(style.patch_caption.clone())
            .into_any_element(),
        ViewRow::PatchEmpty => base()
            .px(ROW_PADDING)
            .bg(style.patch_bg)
            .child(style.patch_empty.clone())
            .into_any_element(),
        ViewRow::Patch { line, range } => {
            let text = match &data.merge_patch {
                Some(Some(lines)) => lines.get(*line as usize).map(|text| piece(text, range)),
                _ => None,
            };
            base()
                .px(ROW_PADDING)
                .bg(style.patch_bg)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(text.unwrap_or_default()),
                )
                .into_any_element()
        }
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
        let body = match (&self.data, &self.laid) {
            (None, _) | (Some(_), None) => centered(i18n_editor(cx, "diff_computing"), theme.muted_foreground),
            (Some(data), Some(_)) if data.identical => {
                centered(i18n_editor(cx, "diff_identical"), theme.muted_foreground)
            }
            (Some(data), Some(laid)) => {
                let data = data.clone();
                let titles = self.render_pane_titles(cx);
                let rows = self.render_rows(laid, &data, window, cx);
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(titles)
                    .child(rows)
                    .into_any_element()
            }
        };

        v_flex()
            .key_context("ValueDiff")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, action: &ValueDiffAction, window, cx| match action {
                ValueDiffAction::Close => (this.on_close)(window, cx),
                ValueDiffAction::NextChange => this.go_to_change(true, window, cx),
                ValueDiffAction::PreviousChange => this.go_to_change(false, window, cx),
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

    /// A value the size of the view's worst case: ~120,000 lines once
    /// pretty-printed. One edit is one changed row, and everything else
    /// folds away around it.
    #[test]
    fn a_large_json_value_with_one_edit_is_one_change_amid_folds() {
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
        assert!(!data.identical);
        assert_eq!((data.removed, data.added), (1, 1));
        assert!(data.left_lines.len() > 100_000, "{} lines", data.left_lines.len());

        let laid = Laid::new(&data, &vec![false; data.folds.len()], Columns::default());
        assert_eq!(laid.changes.len(), 1);
        let folds = laid
            .rows
            .iter()
            .filter(|row| matches!(row, ViewRow::Diff(LayoutRow::Fold { .. })))
            .count();
        // The stretch above the change and the one below it, and the change
        // and its context in between.
        assert_eq!(folds, 2);
        let caption = laid.rows.iter().position(|row| *row == ViewRow::PatchCaption);
        assert!(caption.is_some_and(|at| at < 40), "the diff took {caption:?} rows");
        assert!(matches!(
            laid.rows[laid.changes[0]],
            ViewRow::Diff(LayoutRow::Line { .. })
        ));
        // A merge patch replaces an array whole (RFC 7396), so this one holds
        // every record: rows of the list like the rest, not one block of text.
        assert!(laid.rows.len() > 100_000);
    }

    #[test]
    fn equal_values_are_identical_and_a_plain_string_has_no_patch() {
        let data = DiffData::new(&session("a\nb", "a\nb", false));
        assert!(data.identical);
        assert!(data.merge_patch.is_none());
        let data = DiffData::new(&session("{\"a\":1}", "{\"a\":1}", true));
        assert!(data.identical);
        assert_eq!(data.merge_patch, Some(None));
    }

    #[test]
    fn a_long_line_continues_below_and_the_patch_spans_the_width() {
        let long = "x".repeat(50);
        let data = DiffData::new(&session(&format!("{{\"k\":\"{long}\"}}"), "{\"k\":1}", true));
        let columns = Columns { side: 20, full: 30 };
        let laid = Laid::new(&data, &vec![false; data.folds.len()], columns);
        let pieces_of = |row: u32| {
            laid.rows
                .iter()
                .filter(|laid_row| matches!(laid_row, ViewRow::Diff(LayoutRow::Line { row: r, .. }) if *r == row))
                .count()
        };
        // `  "k": "xxx…"` is 59 characters: three rows of twenty.
        assert_eq!(pieces_of(1), 3);
        let patch_rows = laid
            .rows
            .iter()
            .filter(|row| matches!(row, ViewRow::Patch { .. }))
            .count();
        assert_eq!(patch_rows, 3, "the patch is three short lines");
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
        let wide = columns_for(px(1212.), px(7.), 5);
        let narrow = columns_for(px(400.), px(7.), 5);
        assert!(wide.side > narrow.side && wide.full > wide.side);
        assert_eq!(
            columns_for(px(10.), px(7.), 5),
            Columns {
                side: MIN_COLUMNS,
                full: MIN_COLUMNS
            }
        );
    }
}
