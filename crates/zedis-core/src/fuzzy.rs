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

//! Tiny dependency-free fuzzy matcher for the command palette.
//!
//! Subsequence match (case-insensitive): every char of `query` must
//! appear in `candidate` in order. Score rewards matches that are
//! consecutive and that land on word starts, so "es" ranks
//! `EditorSettings` above `presets`. Good enough for the short,
//! curated lists (servers, commands) the palette searches — not a
//! general-purpose ranker.

/// Per-character scoring weights. Kept conservative so a long exact
/// substring still beats a scattered subsequence.
const MATCH_BASE: i32 = 1;
const CONSECUTIVE_BONUS: i32 = 8;
const WORD_START_BONUS: i32 = 12;
/// Penalty per skipped (non-matching) candidate char before the first
/// match, so an early match is preferred over a late one.
const LEADING_GAP_PENALTY: i32 = -1;

fn is_word_boundary(prev: Option<char>, cur: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            (!p.is_alphanumeric() && cur.is_alphanumeric())
                || (p.is_lowercase() && cur.is_uppercase())
                || (p.is_ascii_digit() != cur.is_ascii_digit())
        }
    }
}

/// Lowercase a query once for scoring a whole batch of candidates —
/// pairs with [`fuzzy_score_prepared`]. [`fuzzy_score`] re-derives this
/// per call, which is wasteful when one keystroke scores thousands of
/// keys.
pub fn prepare_fuzzy_query(query: &str) -> Vec<char> {
    query.chars().flat_map(|c| c.to_lowercase()).collect()
}

/// [`fuzzy_score`] with the query pre-lowercased via
/// [`prepare_fuzzy_query`]. Allocation-free per candidate: the
/// word-boundary check tracks the previous char in a register instead
/// of materialising the candidate as a `Vec<char>`.
pub fn fuzzy_score_prepared(query: &[char], candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let mut qi = 0usize;
    let mut score = 0i32;
    let mut consecutive = false;
    let mut matched_any = false;
    let mut prev: Option<char> = None;

    for cc in candidate.chars() {
        if qi >= query.len() {
            break;
        }
        // Compare case-insensitively without allocating per char.
        let cc_lower = cc.to_lowercase().next().unwrap_or(cc);
        if cc_lower == query[qi] {
            score += MATCH_BASE;
            if consecutive {
                score += CONSECUTIVE_BONUS;
            }
            if is_word_boundary(prev, cc) {
                score += WORD_START_BONUS;
            }
            qi += 1;
            consecutive = true;
            matched_any = true;
        } else {
            consecutive = false;
            if !matched_any {
                score += LEADING_GAP_PENALTY;
            }
        }
        prev = Some(cc);
    }

    if qi == query.len() { Some(score) } else { None }
}

/// Returns `Some(score)` when every char of `query` occurs in
/// `candidate` in order (case-insensitive), else `None`. Higher score
/// = better match. An empty query matches everything with score 0 so
/// the palette can show the full list before any typing.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    fuzzy_score_prepared(&prepare_fuzzy_query(query), candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_all_with_zero() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn non_subsequence_returns_none() {
        assert_eq!(fuzzy_score("xyz", "editor"), None);
        // Out of order fails.
        assert_eq!(fuzzy_score("rotide", "editor"), None);
    }

    #[test]
    fn case_insensitive_subsequence_matches() {
        assert!(fuzzy_score("ED", "editor").is_some());
        assert!(fuzzy_score("edtr", "Editor").is_some());
    }

    #[test]
    fn consecutive_beats_scattered() {
        let tight = fuzzy_score("set", "settings").expect("subsequence matches");
        let loose = fuzzy_score("set", "stale entry text").expect("subsequence matches");
        assert!(tight > loose, "tight {tight} should beat loose {loose}");
    }

    #[test]
    fn word_start_beats_mid_word() {
        // "m" at the start of a word should outrank "m" mid-word.
        let word_start = fuzzy_score("m", "memory analysis").expect("subsequence matches");
        let mid_word = fuzzy_score("m", "command").expect("subsequence matches");
        assert!(
            word_start > mid_word,
            "word_start {word_start} should beat mid_word {mid_word}"
        );
    }

    #[test]
    fn camelcase_boundary_scores_above_mid_word() {
        // Same match position (index 6) in both, so the leading-gap
        // penalty cancels — the only difference is the camelCase
        // boundary bonus on the capital S.
        let boundary = fuzzy_score("s", "EditorSettings").expect("subsequence matches");
        let mid_word = fuzzy_score("s", "editorsettings").expect("subsequence matches");
        assert_eq!(boundary - mid_word, WORD_START_BONUS);
    }

    #[test]
    fn full_exact_scores_high() {
        let s = fuzzy_score("metrics", "metrics").expect("subsequence matches");
        // 7 chars, all consecutive, first is word start.
        assert!(s > 7 * CONSECUTIVE_BONUS);
    }
}
