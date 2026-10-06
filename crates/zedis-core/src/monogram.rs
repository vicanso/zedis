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

//! Two-letter labels for a collapsed connection rail.
//!
//! The first two alphanumeric ASCII letters of a name collide as soon as
//! several connections share a prefix (`aliyun-cluster` / `aliyun-tls` /
//! `aliyun-valkey` all become `AL`). [`unique_monograms`] walks a small
//! candidate list per name and greedily assigns the first label that no
//! earlier name has taken, so the rail stays tellable apart at a glance.
//! CJK names stay a single glyph.

use std::collections::HashSet;

/// One label per name, unique within the list, in the same order.
///
/// Latin names prefer two uppercase ASCII letters; a CJK name whose first
/// alphanumeric character is non-ASCII keeps a single glyph. An empty name
/// (or one with no alphanumeric characters) is `"?"`.
pub fn unique_monograms<'a, I>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let names: Vec<&str> = names.into_iter().collect();
    let mut taken = HashSet::with_capacity(names.len());
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let label = assign(name, &taken);
        taken.insert(label.clone());
        out.push(label);
    }
    out
}

fn assign(name: &str, taken: &HashSet<String>) -> String {
    for candidate in candidates(name) {
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    let first = first_mark(name);
    for n in 2u32..100 {
        let fallback = format!("{first}{n}");
        if !taken.contains(&fallback) {
            return fallback;
        }
    }
    format!("?{}", taken.len())
}

fn candidates(name: &str) -> Vec<String> {
    let alnum: Vec<char> = name.chars().filter(|c| c.is_alphanumeric()).collect();
    if alnum.first().is_some_and(|c| !c.is_ascii()) {
        return cjk_candidates(&alnum);
    }

    let ascii: Vec<char> = alnum
        .iter()
        .copied()
        .filter(char::is_ascii)
        .map(|c| c.to_ascii_uppercase())
        .collect();

    let mut out = Vec::new();
    push_unique(&mut out, pair(&ascii));

    let segments: Vec<&str> = name
        .split(['-', '_', ':'])
        .filter(|segment| !segment.is_empty())
        .collect();
    for segment in segments.iter().rev() {
        let chars: Vec<char> = segment
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_uppercase())
            .collect();
        push_unique(&mut out, pair(&chars));
    }

    if ascii.len() >= 2 {
        push_unique(&mut out, Some(format!("{}{}", ascii[0], ascii[ascii.len() - 1])));
    }

    for skip in 1..ascii.len() {
        push_unique(&mut out, pair(&ascii[skip..]));
    }

    if out.is_empty() {
        out.push(first_mark(name).to_string());
    }
    out
}

fn cjk_candidates(alnum: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    for &c in alnum {
        if !c.is_ascii() {
            push_unique(&mut out, Some(c.to_string()));
        }
    }
    let ascii: Vec<char> = alnum
        .iter()
        .copied()
        .filter(char::is_ascii)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    push_unique(&mut out, pair(&ascii));
    if out.is_empty() {
        out.push("?".to_string());
    }
    out
}

fn pair(chars: &[char]) -> Option<String> {
    match chars {
        [a, b, ..] => Some(format!("{a}{b}")),
        [a] => Some(a.to_string()),
        [] => None,
    }
}

fn first_mark(name: &str) -> char {
    match name.chars().find(|c| c.is_alphanumeric()) {
        Some(c) if c.is_ascii() => c.to_ascii_uppercase(),
        Some(c) => c,
        None => '?',
    }
}

fn push_unique(out: &mut Vec<String>, candidate: Option<String>) {
    if let Some(candidate) = candidate
        && !candidate.is_empty()
        && !out.iter().any(|existing| existing == &candidate)
    {
        out.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::unique_monograms;
    use std::collections::HashSet;

    #[test]
    fn aliyun_family_does_not_collapse_to_al() {
        let names = [
            "aliyun-cluster",
            "aliyun-tls",
            "aliyun-tls-ssh",
            "aliyun-valkey",
            "localhost",
            "local-cluster",
            "redis-cloud",
            "redis3",
        ];
        assert_eq!(
            unique_monograms(names),
            ["AL", "TL", "SS", "VA", "LO", "CL", "RE", "R3"]
        );
    }

    #[test]
    fn labels_in_one_list_are_unique() {
        let got = unique_monograms(["prod", "prod-2", "prod-3", "prod-replica"]);
        let set: HashSet<&String> = got.iter().collect();
        assert_eq!(set.len(), got.len());
    }

    #[test]
    fn cjk_uses_a_single_glyph() {
        assert_eq!(unique_monograms(["缓存", "中文"]), ["缓", "中"]);
    }

    #[test]
    fn empty_name_is_a_question_mark() {
        assert_eq!(unique_monograms([""]), ["?"]);
    }

    #[test]
    fn identical_names_still_differ() {
        let got = unique_monograms(["alpha", "alpha"]);
        assert_eq!(got.len(), 2);
        assert_ne!(got[0], got[1]);
    }

    #[test]
    fn latin_keeps_the_first_two_letters_when_they_are_free() {
        assert_eq!(unique_monograms(["upstash"]), ["UP"]);
    }
}
