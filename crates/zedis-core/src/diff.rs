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

//! Line diff for the value diff view, and the side-by-side layout it is
//! drawn from.
//!
//! Hand-rolled instead of pulling `similar` or `diff-rs`, matching the
//! project's policy of preferring in-crate implementations for
//! self-contained needs. The value diff opens on values of a few MiB — a
//! 1 MiB JSON document pretty-prints to about 120,000 lines — so the diff
//! trims what the two sides share at either end, splits what is left at the
//! lines that occur exactly once on each side (patience diff), and runs a
//! plain LCS only on the pieces between them, which are small.
//!
//! [`line_diff`] answers ops in order: each either matches a left/right line
//! pair (`Equal`), shows a deleted-only left line (`Delete`), or an
//! inserted-only right line (`Insert`). [`side_by_side`] pairs the deletions
//! and insertions of a change up into rows, [`fold_runs`] finds the unchanged
//! stretches worth folding, and [`layout_rows`] cuts it all into the fixed-
//! height rows a virtualized list draws.

use std::collections::HashMap;
use std::ops::Range;

/// One row of a side-by-side line diff. Indices reference the original
/// input slices, kept around so the view can show "L23" / "R24" line
/// numbers without re-counting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// Both sides have the same line. Stored as `(left_idx, right_idx)`.
    Equal(usize, usize),
    /// Left side has a line the right side does not. `usize` is the
    /// left index.
    Delete(usize),
    /// Right side has a line the left side does not. `usize` is the
    /// right index.
    Insert(usize),
}

/// Cells an LCS table may have: `n·m` for the two sides of one piece, `u32`
/// each, so 4 MiB at most. A larger piece is split at unique lines first,
/// and one with none to split at is shown as replaced.
const MAX_LCS_CELLS: usize = 1 << 20;

/// Compute a side-by-side line diff between `left` and `right`.
///
/// Lines keep their endings: `lines()` reads `\r\n` and `\n` alike, so two
/// values that differed only there compared line-equal while the texts were
/// not — neither "identical" nor any line marked. Same line count as
/// `lines()`, so the indices still name the lines a view draws.
///
/// Common lines at both ends are matched first; the middle is split at lines
/// found exactly once on each side, in the order both agree on (the longest
/// increasing run of them — patience diff), and each piece between two such
/// anchors is diffed the same way, down to pieces small enough for an exact
/// LCS. A piece too large for one and without an anchor is shown as all
/// deleted, then all inserted. Runs without recursion, so a pathological
/// input cannot exhaust the stack.
pub fn line_diff(left: &str, right: &str) -> Vec<DiffOp> {
    let left_lines: Vec<&str> = left.split_inclusive('\n').collect();
    let right_lines: Vec<&str> = right.split_inclusive('\n').collect();
    let n = left_lines.len();
    let m = right_lines.len();

    // Fast path: identical content.
    if left == right {
        return (0..n).map(|i| DiffOp::Equal(i, i)).collect();
    }

    let mut out = Vec::with_capacity(n.max(m));
    let mut stack = vec![Work::Diff(0..n, 0..m)];
    while let Some(work) = stack.pop() {
        match work {
            Work::Equal(a, b) => out.push(DiffOp::Equal(a, b)),
            Work::EqualRun(a, b, count) => out.extend((0..count).map(|k| DiffOp::Equal(a + k, b + k))),
            Work::Diff(a, b) => diff_piece(&left_lines, &right_lines, a, b, &mut out, &mut stack),
        }
    }
    out
}

/// What is left to emit, last first: the ops of a piece come out in order
/// because each piece pushes what follows it before what it emits itself.
enum Work {
    Diff(Range<usize>, Range<usize>),
    Equal(usize, usize),
    EqualRun(usize, usize, usize),
}

/// Diff `left[a]` against `right[b]`: emit what can be emitted now (the
/// common prefix, then the middle when it is simple), and push the rest.
fn diff_piece(
    left: &[&str],
    right: &[&str],
    mut a: Range<usize>,
    mut b: Range<usize>,
    out: &mut Vec<DiffOp>,
    stack: &mut Vec<Work>,
) {
    while !a.is_empty() && !b.is_empty() && left[a.start] == right[b.start] {
        out.push(DiffOp::Equal(a.start, b.start));
        a.start += 1;
        b.start += 1;
    }
    let mut tail = 0;
    while !a.is_empty() && !b.is_empty() && left[a.end - 1] == right[b.end - 1] {
        a.end -= 1;
        b.end -= 1;
        tail += 1;
    }
    if tail > 0 {
        stack.push(Work::EqualRun(a.end, b.end, tail));
    }
    if a.is_empty() || b.is_empty() {
        out.extend(a.map(DiffOp::Delete));
        out.extend(b.map(DiffOp::Insert));
        return;
    }
    if a.len().saturating_mul(b.len()) <= MAX_LCS_CELLS {
        lcs_diff(left, right, a, b, out);
        return;
    }
    let anchors = unique_anchors(left, right, a.clone(), b.clone());
    if anchors.is_empty() {
        out.extend(a.map(DiffOp::Delete));
        out.extend(b.map(DiffOp::Insert));
        return;
    }
    // Pushed last-first: the piece after the last anchor, then each anchor
    // and the piece before it.
    let (last_a, last_b) = anchors[anchors.len() - 1];
    stack.push(Work::Diff(last_a + 1..a.end, last_b + 1..b.end));
    for window in (0..anchors.len()).rev() {
        let (anchor_a, anchor_b) = anchors[window];
        stack.push(Work::Equal(anchor_a, anchor_b));
        let (from_a, from_b) = match window {
            0 => (a.start, b.start),
            _ => (anchors[window - 1].0 + 1, anchors[window - 1].1 + 1),
        };
        stack.push(Work::Diff(from_a..anchor_a, from_b..anchor_b));
    }
}

/// Lines found exactly once in `left[a]` and exactly once in `right[b]`,
/// as `(left, right)` index pairs, reduced to the longest run whose right
/// indices increase with the left ones — the matches both sides agree on
/// the order of.
fn unique_anchors(left: &[&str], right: &[&str], a: Range<usize>, b: Range<usize>) -> Vec<(usize, usize)> {
    // Per line: (count on the left, count on the right, left index, right index).
    let mut seen: HashMap<&str, (u32, u32, usize, usize)> = HashMap::with_capacity(a.len());
    for i in a.clone() {
        let entry = seen.entry(left[i]).or_insert((0, 0, i, 0));
        entry.0 += 1;
    }
    for j in b {
        if let Some(entry) = seen.get_mut(right[j]) {
            entry.1 += 1;
            entry.3 = j;
        }
    }
    let pairs: Vec<(usize, usize)> = a
        .filter_map(|i| match seen.get(left[i]) {
            Some(&(1, 1, li, rj)) if li == i => Some((li, rj)),
            _ => None,
        })
        .collect();
    longest_increasing_run(&pairs)
}

/// The longest subsequence of `pairs` (sorted by left index) whose right
/// indices increase — patience sorting, `O(k log k)`.
fn longest_increasing_run(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // `tails[len]`: index into `pairs` of the smallest right index that ends
    // an increasing run of `len + 1`; `back[i]`: the pair before `i` in the
    // run it ends.
    let mut tails: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; pairs.len()];
    for (i, &(_, right)) in pairs.iter().enumerate() {
        let len = tails.partition_point(|&t| pairs[t].1 < right);
        back[i] = len.checked_sub(1).map(|prev| tails[prev]);
        if len == tails.len() {
            tails.push(i);
        } else {
            tails[len] = i;
        }
    }
    let mut run = Vec::with_capacity(tails.len());
    let mut at = tails.last().copied();
    while let Some(i) = at {
        run.push(pairs[i]);
        at = back[i];
    }
    run.reverse();
    run
}

/// Exact LCS diff of `left[a]` against `right[b]`: a table of LCS lengths,
/// then a walk back from the end. When deletion and insertion score the
/// same the walk takes the insertion first, so in the output a change's
/// deletions come before its insertions, as `diff(1)` prints them.
fn lcs_diff(left: &[&str], right: &[&str], a: Range<usize>, b: Range<usize>, out: &mut Vec<DiffOp>) {
    let (n, m) = (a.len(), b.len());
    let width = m + 1;
    // `lens[i * width + j]`: LCS length of the first `i` left and `j` right lines.
    let mut lens = vec![0_u32; (n + 1) * width];
    for i in 0..n {
        for j in 0..m {
            lens[(i + 1) * width + j + 1] = if left[a.start + i] == right[b.start + j] {
                lens[i * width + j] + 1
            } else {
                lens[(i + 1) * width + j].max(lens[i * width + j + 1])
            };
        }
    }
    let mut ops: Vec<DiffOp> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && left[a.start + i - 1] == right[b.start + j - 1] {
            ops.push(DiffOp::Equal(a.start + i - 1, b.start + j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || lens[i * width + j - 1] >= lens[(i - 1) * width + j]) {
            ops.push(DiffOp::Insert(b.start + j - 1));
            j -= 1;
        } else {
            ops.push(DiffOp::Delete(a.start + i - 1));
            i -= 1;
        }
    }
    out.extend(ops.into_iter().rev());
}

/// One row of the side-by-side view: the line shown on each side, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideBySideRow {
    pub left: Option<u32>,
    pub right: Option<u32>,
    /// Part of a change (a deleted line, an inserted one, or a pair of them)
    /// rather than an unchanged line.
    pub changed: bool,
}

/// The rows a side-by-side view draws for `ops`. The deletions and
/// insertions of one change are paired up in order — the first removed
/// line beside the first added one — so a line that was edited shows its
/// old and new text on one row, where drawing each op as its own row put
/// every edit on two rows, each half empty.
pub fn side_by_side(ops: &[DiffOp]) -> Vec<SideBySideRow> {
    let mut rows = Vec::with_capacity(ops.len());
    let mut deleted: Vec<u32> = Vec::new();
    let mut inserted: Vec<u32> = Vec::new();
    let flush = |rows: &mut Vec<SideBySideRow>, deleted: &mut Vec<u32>, inserted: &mut Vec<u32>| {
        for k in 0..deleted.len().max(inserted.len()) {
            rows.push(SideBySideRow {
                left: deleted.get(k).copied(),
                right: inserted.get(k).copied(),
                changed: true,
            });
        }
        deleted.clear();
        inserted.clear();
    };
    for op in ops {
        match *op {
            DiffOp::Equal(l, r) => {
                flush(&mut rows, &mut deleted, &mut inserted);
                rows.push(SideBySideRow {
                    left: Some(l as u32),
                    right: Some(r as u32),
                    changed: false,
                });
            }
            DiffOp::Delete(l) => deleted.push(l as u32),
            DiffOp::Insert(r) => inserted.push(r as u32),
        }
    }
    flush(&mut rows, &mut deleted, &mut inserted);
    rows
}

/// The stretches of unchanged rows a view may fold away, as row ranges.
/// Each keeps `context` rows beside every change (none at the very top or
/// bottom, where there is nothing to give context to) and hides at least
/// `min_hidden` rows — folding two lines behind a one-line marker saves
/// nothing.
pub fn fold_runs(rows: &[SideBySideRow], context: usize, min_hidden: usize) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].changed {
            i += 1;
            continue;
        }
        let start = i;
        while i < rows.len() && !rows[i].changed {
            i += 1;
        }
        let from = if start == 0 { 0 } else { start + context };
        let to = if i == rows.len() { i } else { i.saturating_sub(context) };
        if to > from && to - from >= min_hidden {
            runs.push(from..to);
        }
    }
    runs
}

/// A row of the laid-out diff: what one line of a virtualized list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutRow {
    /// Part of `rows[row]`: `chunk` 0 is where its line numbers go, a
    /// later one continues a line too long for the width. `left` / `right`
    /// are the byte ranges of that side's line shown here, `None` where
    /// the side has no line or nothing left of it.
    Line {
        row: u32,
        chunk: u32,
        left: Option<Range<u32>>,
        right: Option<Range<u32>>,
    },
    /// `folds[fold]`, folded: `hidden` unchanged rows behind one marker.
    Fold { fold: u32, hidden: u32 },
}

/// A diff cut into rows: the rows, and where each change starts among them
/// (the first row of every run of changed rows), for "next change".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffLayout {
    pub rows: Vec<LayoutRow>,
    pub changes: Vec<usize>,
}

/// Lay `rows` out for a view `columns` wide per side: the folds in `folds`
/// that `expanded` does not open become one row each, and a line wider than
/// the view continues on the rows below it — every row the same height, as
/// a virtualized list needs. Wrapping happens here, once per width, so the
/// view only slices the ranges it is handed.
pub fn layout_rows<L: AsRef<str>, R: AsRef<str>>(
    rows: &[SideBySideRow],
    left_lines: &[L],
    right_lines: &[R],
    folds: &[Range<usize>],
    expanded: &[bool],
    columns: usize,
) -> DiffLayout {
    let mut layout = DiffLayout {
        rows: Vec::with_capacity(rows.len()),
        changes: Vec::new(),
    };
    let mut folds = folds.iter().enumerate().peekable();
    let mut previous_changed = false;
    let mut i = 0;
    while i < rows.len() {
        if let Some(&(fold, range)) = folds.peek()
            && range.start == i
        {
            folds.next();
            if !expanded.get(fold).copied().unwrap_or(false) {
                layout.rows.push(LayoutRow::Fold {
                    fold: fold as u32,
                    hidden: range.len() as u32,
                });
                i = range.end;
                previous_changed = false;
                continue;
            }
        }
        let row = rows[i];
        if row.changed && !previous_changed {
            layout.changes.push(layout.rows.len());
        }
        previous_changed = row.changed;
        let left = row
            .left
            .and_then(|index| left_lines.get(index as usize))
            .map(|line| wrap_line(line.as_ref(), columns));
        let right = row
            .right
            .and_then(|index| right_lines.get(index as usize))
            .map(|line| wrap_line(line.as_ref(), columns));
        let chunks = left
            .as_ref()
            .map_or(0, Vec::len)
            .max(right.as_ref().map_or(0, Vec::len))
            .max(1);
        let span = |chunks: &Option<Vec<Range<usize>>>, chunk: usize| {
            chunks
                .as_ref()
                .and_then(|chunks| chunks.get(chunk))
                .map(|range| range.start as u32..range.end as u32)
        };
        for chunk in 0..chunks {
            layout.rows.push(LayoutRow::Line {
                row: i as u32,
                chunk: chunk as u32,
                left: span(&left, chunk),
                right: span(&right, chunk),
            });
        }
        i += 1;
    }
    layout
}

/// `line` cut into pieces of at most `columns` columns, as byte ranges: at
/// least one (an empty line is one empty piece). A wide character — CJK,
/// Hangul, full-width forms — takes two columns, as a monospace font draws
/// it; ASCII is sliced by bytes without looking at characters.
pub fn wrap_line(line: &str, columns: usize) -> Vec<Range<usize>> {
    let columns = columns.max(1);
    if line.is_ascii() {
        let mut pieces: Vec<Range<usize>> = (0..line.len())
            .step_by(columns)
            .map(|start| start..(start + columns).min(line.len()))
            .collect();
        if pieces.is_empty() {
            pieces.push(0..0);
        }
        return pieces;
    }
    let mut pieces = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (at, ch) in line.char_indices() {
        let width = char_columns(ch);
        if used + width > columns && at > start {
            pieces.push(start..at);
            start = at;
            used = 0;
        }
        used += width;
    }
    pieces.push(start..line.len());
    pieces
}

/// Columns `ch` takes in a monospace font: two for the East Asian wide and
/// full-width blocks, one otherwise.
fn char_columns(ch: char) -> usize {
    match u32::from(ch) {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// How a key changed between two key→value snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvDelta {
    /// Key exists only in the new snapshot.
    Added,
    /// Key exists only in the old snapshot.
    Removed,
    /// Key exists in both with different values.
    Changed,
}

/// One differing entry of a key→value snapshot comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvDiffEntry {
    pub key: String,
    /// Value in the old snapshot (`None` for [`KvDelta::Added`]).
    pub old: Option<String>,
    /// Value in the new snapshot (`None` for [`KvDelta::Removed`]).
    pub new: Option<String>,
    pub delta: KvDelta,
}

/// Field-level comparison of two key→value snapshots (e.g. two `INFO`
/// captures). Unlike [`line_diff`] this is order-insensitive: a key that
/// merely moved produces no entry, so section reordering between
/// captures doesn't drown the real changes in noise. Unchanged pairs are
/// omitted. Entries follow the new snapshot's order, with removed keys
/// appended in old-snapshot order. Duplicate keys keep the last value.
pub fn kv_diff(old: &[(String, String)], new: &[(String, String)]) -> Vec<KvDiffEntry> {
    use std::collections::{HashMap, HashSet};

    let old_map: HashMap<&str, &str> = old.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let new_keys: HashSet<&str> = new.iter().map(|(k, _)| k.as_str()).collect();

    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (key, value) in new {
        // Only diff a duplicated key once, against its last value.
        if !seen.insert(key.as_str()) {
            continue;
        }
        let value = new
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or(value);
        match old_map.get(key.as_str()) {
            None => out.push(KvDiffEntry {
                key: key.clone(),
                old: None,
                new: Some(value.to_string()),
                delta: KvDelta::Added,
            }),
            Some(old_value) if *old_value != value => out.push(KvDiffEntry {
                key: key.clone(),
                old: Some((*old_value).to_string()),
                new: Some(value.to_string()),
                delta: KvDelta::Changed,
            }),
            Some(_) => {}
        }
    }
    for (key, value) in old {
        if !new_keys.contains(key.as_str()) && seen.insert(key.as_str()) {
            out.push(KvDiffEntry {
                key: key.clone(),
                old: Some(value.clone()),
                new: None,
                delta: KvDelta::Removed,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        DiffLayout, DiffOp, KvDelta, LayoutRow, SideBySideRow, fold_runs, kv_diff, layout_rows, line_diff,
        side_by_side, wrap_line,
    };

    /// Every left line exactly once, in order, from `Equal` / `Delete`;
    /// every right line likewise from `Equal` / `Insert`; and an `Equal`
    /// pairs lines that are the same.
    fn assert_valid(ops: &[DiffOp], left: &str, right: &str) {
        let l: Vec<&str> = left.split_inclusive('\n').collect();
        let r: Vec<&str> = right.split_inclusive('\n').collect();
        let (mut next_l, mut next_r) = (0, 0);
        for op in ops {
            match *op {
                DiffOp::Equal(a, b) => {
                    assert_eq!((a, b), (next_l, next_r), "out of order at {op:?}");
                    assert_eq!(l[a], r[b], "unequal lines paired at {op:?}");
                    next_l += 1;
                    next_r += 1;
                }
                DiffOp::Delete(a) => {
                    assert_eq!(a, next_l, "out of order at {op:?}");
                    next_l += 1;
                }
                DiffOp::Insert(b) => {
                    assert_eq!(b, next_r, "out of order at {op:?}");
                    next_r += 1;
                }
            }
        }
        assert_eq!((next_l, next_r), (l.len(), r.len()), "not every line was placed");
    }

    fn changes(ops: &[DiffOp]) -> (usize, usize) {
        let deleted = ops.iter().filter(|op| matches!(op, DiffOp::Delete(_))).count();
        let inserted = ops.iter().filter(|op| matches!(op, DiffOp::Insert(_))).count();
        (deleted, inserted)
    }

    /// Pretty-printed JSON as the diff view sees it: every record's braces
    /// repeat hundreds of thousands of times, its id and name do not.
    fn records(count: usize, renamed: &[usize]) -> String {
        let mut text = String::from("[\n");
        for i in 0..count {
            let name = if renamed.contains(&i) {
                format!("renamed-{i}")
            } else {
                format!("user-{i}")
            };
            text.push_str(&format!(
                "  {{\n    \"id\": {i},\n    \"name\": \"{name}\",\n    \"active\": true\n  }},\n"
            ));
        }
        text.push_str("]\n");
        text
    }

    #[test]
    fn one_edit_in_a_large_value_is_one_line_changed() {
        // ~120,000 lines, the size a 1 MiB JSON value pretty-prints to — far
        // past any LCS table, which used to turn this into 120,000 lines
        // deleted and 120,000 inserted.
        let left = records(24_000, &[]);
        let right = records(24_000, &[12_345]);
        let ops = line_diff(&left, &right);
        assert_valid(&ops, &left, &right);
        assert_eq!(changes(&ops), (1, 1));
    }

    #[test]
    fn scattered_edits_in_a_large_value_are_each_found() {
        let renamed: Vec<usize> = (0..50).map(|k| k * 397 + 11).collect();
        let left = records(20_000, &[]);
        let right = records(20_000, &renamed);
        let ops = line_diff(&left, &right);
        assert_valid(&ops, &left, &right);
        assert_eq!(changes(&ops), (50, 50));
        // A record added and one removed, too.
        let fewer = records(20_000, &[]).replacen(
            "  {\n    \"id\": 7,\n    \"name\": \"user-7\",\n    \"active\": true\n  },\n",
            "",
            1,
        );
        let ops = line_diff(&left, &fewer);
        assert_valid(&ops, &left, &fewer);
        assert_eq!(changes(&ops), (5, 0));
    }

    /// A small xorshift, so the random cases are the same on every run.
    fn random_lines(seed: &mut u64, count: usize, alphabet: u64) -> String {
        (0..count)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                format!("{}\n", *seed % alphabet)
            })
            .collect()
    }

    /// The length of a longest common subsequence of the two texts' lines.
    fn lcs_len(left: &str, right: &str) -> usize {
        let l: Vec<&str> = left.split_inclusive('\n').collect();
        let r: Vec<&str> = right.split_inclusive('\n').collect();
        let mut row = vec![0usize; r.len() + 1];
        for a in &l {
            let mut diagonal = 0;
            for (j, b) in r.iter().enumerate() {
                let above = row[j + 1];
                row[j + 1] = if a == b { diagonal + 1 } else { above.max(row[j]) };
                diagonal = above;
            }
        }
        row[r.len()]
    }

    #[test]
    fn small_inputs_still_get_a_shortest_diff() {
        let mut seed = 0x2026_0927_u64;
        for _ in 0..300 {
            let left = random_lines(&mut seed, 40, 6);
            let right = random_lines(&mut seed, 35, 6);
            let ops = line_diff(&left, &right);
            assert_valid(&ops, &left, &right);
            let equal = ops.iter().filter(|op| matches!(op, DiffOp::Equal(..))).count();
            assert_eq!(equal, lcs_len(&left, &right), "{left:?} / {right:?}");
        }
    }

    #[test]
    fn large_inputs_are_always_a_valid_diff() {
        let mut seed = 0x5eed_u64;
        // Few distinct lines (almost nothing unique to anchor on) and many.
        for alphabet in [3, 50, 5_000, 1_000_000] {
            let left = random_lines(&mut seed, 3_000, alphabet);
            let mut right = left.clone();
            right.insert_str(right.len() / 2, &random_lines(&mut seed, 1_500, alphabet));
            let ops = line_diff(&left, &right);
            assert_valid(&ops, &left, &right);
        }
    }

    #[test]
    fn an_edit_sits_beside_what_it_replaced() {
        let ops = [
            DiffOp::Equal(0, 0),
            DiffOp::Delete(1),
            DiffOp::Delete(2),
            DiffOp::Insert(1),
            DiffOp::Equal(3, 2),
            DiffOp::Insert(3),
        ];
        let row = |left, right, changed| SideBySideRow { left, right, changed };
        assert_eq!(
            side_by_side(&ops),
            [
                row(Some(0), Some(0), false),
                row(Some(1), Some(1), true),
                row(Some(2), None, true),
                row(Some(3), Some(2), false),
                row(None, Some(3), true),
            ]
        );
    }

    /// `(start, end)` pairs as ranges — a one-range literal reads as "the
    /// whole range" to clippy.
    fn ranges(pairs: &[(usize, usize)]) -> Vec<std::ops::Range<usize>> {
        pairs.iter().map(|&(start, end)| start..end).collect()
    }

    fn unchanged(count: usize) -> Vec<SideBySideRow> {
        (0..count as u32)
            .map(|i| SideBySideRow {
                left: Some(i),
                right: Some(i),
                changed: false,
            })
            .collect()
    }

    #[test]
    fn unchanged_stretches_fold_but_keep_their_context() {
        let mut rows = unchanged(20);
        rows[10].changed = true;
        // Top: from the first row; bottom: to the last; three rows of context
        // on the side of the change.
        assert_eq!(fold_runs(&rows, 3, 4), ranges(&[(0, 7), (14, 20)]));
        // A stretch that would hide fewer rows than the minimum stays open.
        assert_eq!(fold_runs(&rows, 3, 7), ranges(&[(0, 7)]));
        let mut between = unchanged(20);
        between[2].changed = true;
        between[17].changed = true;
        assert_eq!(fold_runs(&between, 3, 4), ranges(&[(6, 14)]));
        assert!(fold_runs(&unchanged(0), 3, 4).is_empty());
    }

    #[test]
    fn a_long_line_wraps_by_the_columns_a_monospace_font_gives_it() {
        assert_eq!(wrap_line("", 4), ranges(&[(0, 0)]));
        assert_eq!(wrap_line("abcdefghij", 4), ranges(&[(0, 4), (4, 8), (8, 10)]));
        assert_eq!(wrap_line("abcd", 4), ranges(&[(0, 4)]));
        // Two columns each for CJK: two per piece of four.
        let text = "键名很长";
        assert_eq!(wrap_line(text, 4), ranges(&[(0, 6), (6, 12)]));
        // A wide character never splits, even in a column too narrow for it.
        assert_eq!(wrap_line("a键", 2), ranges(&[(0, 1), (1, 4)]));
    }

    #[test]
    fn the_layout_folds_wraps_and_marks_where_changes_start() {
        let left = ["a", "b", "c", "d", "e", "f", "g", "h", "old", "i"];
        let right = ["a", "b", "c", "d", "e", "f", "g", "h", "brand-new-line", "i"];
        let ops = line_diff(&(left.join("\n") + "\n"), &(right.join("\n") + "\n"));
        let rows = side_by_side(&ops);
        let folds = fold_runs(&rows, 2, 3);
        assert_eq!(folds, ranges(&[(0, 6)]));
        let line = |row: u32, chunk: u32, left: Option<std::ops::Range<u32>>, right: Option<std::ops::Range<u32>>| {
            LayoutRow::Line {
                row,
                chunk,
                left,
                right,
            }
        };
        let DiffLayout { rows: laid, changes } = layout_rows(&rows, &left, &right, &folds, &[false], 8);
        assert_eq!(
            laid,
            [
                LayoutRow::Fold { fold: 0, hidden: 6 },
                line(6, 0, Some(0..1), Some(0..1)),
                line(7, 0, Some(0..1), Some(0..1)),
                line(8, 0, Some(0..3), Some(0..8)),
                line(8, 1, None, Some(8..14)),
                line(9, 0, Some(0..1), Some(0..1)),
            ]
        );
        assert_eq!(changes, [3]);
        // Opened, the fold is its rows again.
        let open = layout_rows(&rows, &left, &right, &folds, &[true], 80);
        assert_eq!(open.rows.len(), 10);
        assert_eq!(open.changes, [8]);
    }

    #[test]
    fn a_line_that_differs_only_in_its_ending_is_marked() {
        let ops = line_diff("a\r\nb\n", "a\nb\n");
        assert!(
            ops.iter().any(|op| !matches!(op, DiffOp::Equal(..))),
            "CRLF against LF is a change: {ops:?}"
        );
        // And the same text is still all equal, last line with or without
        // its newline counted the way `lines()` counts it.
        assert_eq!(
            line_diff("a\nb", "a\nb"),
            vec![DiffOp::Equal(0, 0), DiffOp::Equal(1, 1)]
        );
    }

    #[test]
    fn identical_inputs_yield_only_equal_ops() {
        let ops = line_diff("a\nb\nc", "a\nb\nc");
        assert_eq!(ops, vec![DiffOp::Equal(0, 0), DiffOp::Equal(1, 1), DiffOp::Equal(2, 2)]);
    }

    #[test]
    fn pure_insert_marks_every_right_line() {
        let ops = line_diff("", "a\nb");
        assert_eq!(ops, vec![DiffOp::Insert(0), DiffOp::Insert(1)]);
    }

    #[test]
    fn pure_delete_marks_every_left_line() {
        let ops = line_diff("a\nb", "");
        assert_eq!(ops, vec![DiffOp::Delete(0), DiffOp::Delete(1)]);
    }

    #[test]
    fn mid_change_keeps_unchanged_lines_aligned() {
        // Classic single-line replacement: the surrounding lines stay
        // as Equal, the differing line becomes Delete then Insert
        // (matches `diff(1)` order so the side-by-side view shows the
        // removed line on the left, the inserted one on the right).
        let ops = line_diff("a\nold\nc", "a\nnew\nc");
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal(0, 0),
                DiffOp::Delete(1),
                DiffOp::Insert(1),
                DiffOp::Equal(2, 2),
            ]
        );
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn kv_diff_reports_added_removed_changed_only() {
        let old = pairs(&[("uptime", "100"), ("version", "7.2"), ("gone", "x")]);
        let new = pairs(&[("uptime", "160"), ("version", "7.2"), ("fresh", "y")]);
        let entries = kv_diff(&old, &new);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key, "uptime");
        assert_eq!(entries[0].delta, KvDelta::Changed);
        assert_eq!(entries[0].old.as_deref(), Some("100"));
        assert_eq!(entries[0].new.as_deref(), Some("160"));
        assert_eq!(entries[1].key, "fresh");
        assert_eq!(entries[1].delta, KvDelta::Added);
        assert_eq!(entries[2].key, "gone");
        assert_eq!(entries[2].delta, KvDelta::Removed);
    }

    #[test]
    fn kv_diff_ignores_reordering() {
        let old = pairs(&[("a", "1"), ("b", "2")]);
        let new = pairs(&[("b", "2"), ("a", "1")]);
        assert!(kv_diff(&old, &new).is_empty());
    }

    #[test]
    fn kv_diff_duplicate_keys_use_last_value() {
        let old = pairs(&[("k", "1")]);
        let new = pairs(&[("k", "1"), ("k", "2")]);
        let entries = kv_diff(&old, &new);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].delta, KvDelta::Changed);
        assert_eq!(entries[0].new.as_deref(), Some("2"));
    }

    #[test]
    fn very_large_input_degrades_to_all_replace() {
        // Too large for an LCS table and without a single common line to
        // split at: shown as replaced, so the UI does not lock up. Use
        // DIFFERENT big inputs — identical ones hit the equality fast-path.
        let left_big: String = (0..2500).map(|i| format!("L{i}\n")).collect();
        let right_big: String = (0..2500).map(|i| format!("R{i}\n")).collect();
        let ops = line_diff(&left_big, &right_big);
        let dels = ops.iter().filter(|o| matches!(o, DiffOp::Delete(_))).count();
        let ins = ops.iter().filter(|o| matches!(o, DiffOp::Insert(_))).count();
        assert_eq!(dels, 2500);
        assert_eq!(ins, 2500);
    }
}
