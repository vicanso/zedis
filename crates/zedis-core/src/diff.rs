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

//! Tiny LCS-based line diff used by the value diff view.
//!
//! We hand-roll instead of pulling `similar` or `diff-rs` because the
//! payload is small (capped at ~64 KB in practice — large enough that
//! quadratic LCS still finishes well under a frame, but small enough we
//! don't need myers/patience optimisations). Keeps the dependency
//! surface lean, matching the project's policy of preferring in-crate
//! implementations for self-contained needs.
//!
//! The output is **side-by-side oriented**: each row in the result
//! either matches a left/right line pair (`Equal`), shows a deleted-only
//! left line (`Delete`), or shows an inserted-only right line
//! (`Insert`). The view renders these straight to a two-column layout
//! with paired empty cells for the lopsided ops.

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

/// Compute a side-by-side line diff between `left` and `right`.
///
/// Algorithm: classic LCS DP table, then walk back from `[n][m]` to
/// `[0][0]` building the op list. Bounded at `MAX_LINES` per side
/// because the DP is `O(n·m)` — beyond that, the table itself is the
/// problem (1k×1k = 8MB just for the lengths array), so we degrade
/// gracefully to an all-replace diff rather than freeze the UI.
pub fn line_diff(left: &str, right: &str) -> Vec<DiffOp> {
    const MAX_LINES: usize = 2000;

    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let n = left_lines.len();
    let m = right_lines.len();

    // Fast path: identical content.
    if n == m && left == right {
        return (0..n).map(|i| DiffOp::Equal(i, i)).collect();
    }

    // Degrade for very large inputs — show as "all deleted, all
    // inserted" rather than spend seconds on a 10k-line LCS. The view
    // can show a "diff too large for line-level" hint when this fires.
    if n > MAX_LINES || m > MAX_LINES {
        let mut out = Vec::with_capacity(n + m);
        for i in 0..n {
            out.push(DiffOp::Delete(i));
        }
        for j in 0..m {
            out.push(DiffOp::Insert(j));
        }
        return out;
    }

    // LCS length table — `lens[i][j]` = length of LCS of left[..i] and
    // right[..j]. One row of (m+1) per (n+1) rows.
    let mut lens = vec![vec![0_usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            lens[i + 1][j + 1] = if left_lines[i] == right_lines[j] {
                lens[i][j] + 1
            } else {
                lens[i + 1][j].max(lens[i][j + 1])
            };
        }
    }

    // Walk back from (n, m). Standard LCS reconstruction with one
    // tiebreaker tweak: when both deletion and insertion paths score
    // the same, prefer insert-first so deletes cluster at the bottom
    // of each change block (matches what `diff(1)` outputs).
    let mut ops: Vec<DiffOp> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && left_lines[i - 1] == right_lines[j - 1] {
            ops.push(DiffOp::Equal(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || lens[i][j - 1] >= lens[i - 1][j]) {
            ops.push(DiffOp::Insert(j - 1));
            j -= 1;
        } else {
            ops.push(DiffOp::Delete(i - 1));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::{DiffOp, line_diff};

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

    #[test]
    fn very_large_input_degrades_to_all_replace() {
        // Above MAX_LINES we fall back to the trivial diff so the UI
        // doesn't lock up. Use DIFFERENT big inputs — identical ones
        // hit the equality fast-path first.
        let left_big: String = (0..2500).map(|i| format!("L{i}\n")).collect();
        let right_big: String = (0..2500).map(|i| format!("R{i}\n")).collect();
        let ops = line_diff(&left_big, &right_big);
        let dels = ops.iter().filter(|o| matches!(o, DiffOp::Delete(_))).count();
        let ins = ops.iter().filter(|o| matches!(o, DiffOp::Insert(_))).count();
        assert_eq!(dels, 2500);
        assert_eq!(ins, 2500);
    }
}
