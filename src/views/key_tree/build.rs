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

//! Pure key-tree construction: SCAN snapshot + local metadata in,
//! ordered `KeyTreeItem` rows out. No gpui entities — everything here
//! is unit-tested at the bottom of the file.

use super::*;
use regex::Regex;

/// When a tag-colour filter is active, derive the input key list
/// **directly from local metadata** rather than from the SCAN snapshot.
/// SCAN is paginated and bounded — until it completes, the snapshot is
/// a strict subset of the server's keyspace, so filtering after the
/// scan would silently hide tagged keys that haven't been scanned yet.
///
/// For each tagged key we try to recover its `KeyType` from the SCAN
/// snapshot (constant-time lookup via a `name → type` index built
/// here). Keys outside the snapshot fall back to `KeyType::Unknown` —
/// the subsequent local AND with a type filter drops those (so tag
/// rows cannot bypass `SCAN TYPE`). Keys that have been deleted on the
/// server but still carry local metadata also show up this way when no
/// type filter is set; that's intentional, since the loud "this key is
/// gone" feedback helps the user spot dangling annotations.
pub(super) fn build_tagged_keys_list(
    color: TagColor,
    snapshot_keys: &[(SharedString, KeyType)],
    metadata: &std::collections::HashMap<String, KeyMetadata>,
) -> Vec<(SharedString, KeyType)> {
    // O(1) type lookup — `metadata` may contain hundreds of entries on
    // a heavily-annotated server, so a linear scan per entry would be
    // wasteful even if the snapshot is small.
    let type_by_key: std::collections::HashMap<&str, KeyType> =
        snapshot_keys.iter().map(|(k, t)| (k.as_ref(), *t)).collect();
    let mut tagged: Vec<(SharedString, KeyType)> = metadata
        .iter()
        .filter(|(_, m)| m.tag == Some(color))
        .map(|(key, _)| {
            let key_type = type_by_key.get(key.as_str()).copied().unwrap_or(KeyType::Unknown);
            (SharedString::from(key.clone()), key_type)
        })
        .collect();
    // `new_key_tree_items` requires sorted input (the snapshot path is
    // pre-sorted at cache time); HashMap iteration order is not. Cheap:
    // only this colour's tagged keys, dozens-to-hundreds.
    tagged.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    tagged
}

/// Local AND over the candidate key list (already obtained from SCAN
/// and/or the local tag index). Does **not** issue Redis commands —
/// type may still have been narrowed server-side via `SCAN TYPE`.
///
/// Dimensions (any `None` / `All` is a no-op for that axis):
/// - `type_filter`: exact `KeyType` match (one probabilistic filter
///   matches every sketch kind); `Unknown` never matches
/// - `tag_filter`: exact local metadata tag colour
/// - `ttl_filter`: cached TTL range (missing / `-2` never match)
pub(super) fn apply_local_key_filters(
    keys: Vec<(SharedString, KeyType)>,
    type_filter: Option<KeyType>,
    tag_filter: Option<TagColor>,
    ttl_filter: TtlFilter,
    key_ttls: &AHashMap<SharedString, i64>,
    metadata: &std::collections::HashMap<String, KeyMetadata>,
) -> Vec<(SharedString, KeyType)> {
    if type_filter.is_none() && tag_filter.is_none() && matches!(ttl_filter, TtlFilter::All) {
        return keys;
    }
    keys.into_iter()
        .filter(|(key, key_type)| {
            if let Some(want) = type_filter
                && !key_type.matches_filter(want)
            {
                return false;
            }
            if let Some(want) = tag_filter {
                let tag = metadata.get(key.as_ref()).and_then(|m| m.tag);
                if tag != Some(want) {
                    return false;
                }
            }
            if !matches!(ttl_filter, TtlFilter::All) {
                let ttl = key_ttls.get(key).copied();
                if !ttl_filter.matches(ttl) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Index into [`TagColor::ALL`] / a fixed 6-slot histogram.
pub(super) fn tag_color_index(color: TagColor) -> usize {
    TagColor::ALL.iter().position(|&c| c == color).unwrap_or(0)
}

/// Split a key into tree segments — the single source of truth every
/// tree-building pass shares, so folder ids, expansion bookkeeping and tag
/// aggregates can never disagree about where a key's levels are.
///
/// Behaves like `key.splitn(max_depth, separator)` except that separators
/// **inside `{…}`, inside double quotes, or inside a timestamp are not level
/// boundaries**:
///
/// * `{…}` is Redis's cluster hash tag, so `user:{tenant:42}:profile` has
///   three levels, not four.
/// * Keys that carry a JSON-ish blob (`item:1/{"name": "x", "at": "12:13:05"}`)
///   would otherwise shatter along the colons *inside* the payload, which is
///   what made such keys look truncated in the tree (issue #119).
/// * A timestamp stamped into a key (`…/2022-05-11 11:55:44.487892+00:00/…`)
///   brings its own colons; splitting there produced folders labelled `55`
///   and `44.487892+00` — fragments that read like numbers Zedis had invented
///   rather than parts of the key (issue #127). Only a full ISO-8601 date
///   anchors this: a bare `12:30:45` stays three levels, because digits alone
///   are an ordinary namespace (`shard:03:07`) far more often than a time.
///
/// A key whose braces or quotes never close is rescanned with both treated as
/// ordinary text, so one malformed key can't collapse into a single
/// unsplittable segment.
pub(super) fn split_key_segments<'a>(key: &'a str, separator: &str, max_depth: usize) -> Vec<&'a str> {
    let depth = max_depth.max(1);
    if depth == 1 || separator.is_empty() {
        return vec![key];
    }
    let (segments, balanced) = scan_key_segments(key, separator, depth, true);
    if balanced {
        return segments;
    }
    // Unterminated brace / quote: the scan above suppressed separators that a
    // reader would consider real, so redo it with braces and quotes as plain
    // text. Timestamps still hold — they don't depend on a closing delimiter.
    scan_key_segments(key, separator, depth, false).0
}

/// One splitting pass. `honor_groups` off makes `{`, `}` and `"` ordinary
/// bytes. The returned flag says whether every group opened in this pass also
/// closed — always true when groups aren't honoured, since nothing was
/// suppressed on their account.
fn scan_key_segments<'a>(key: &'a str, separator: &str, depth: usize, honor_groups: bool) -> (Vec<&'a str>, bool) {
    let bytes = key.as_bytes();
    let sep = separator.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut brace_depth = 0usize;
    let mut in_quote = false;
    // End (exclusive) of the timestamp the scan is currently inside, 0 when it
    // is not in one. Timestamps never nest, so a single cursor covers it.
    let mut stamp_end = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // `{`, `}`, `"`, ASCII digits and any separator byte are ASCII-leading,
        // so byte scanning never lands mid-codepoint (continuation bytes are
        // 0x80-0xBF).
        if i >= stamp_end && b.is_ascii_digit() {
            let len = iso_timestamp_len(bytes, i);
            if len > 0 {
                stamp_end = i + len;
            }
        }
        if honor_groups {
            if b == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_quote = !in_quote;
                i += 1;
                continue;
            }
            if !in_quote {
                if b == b'{' {
                    brace_depth += 1;
                    i += 1;
                    continue;
                }
                if b == b'}' {
                    brace_depth = brace_depth.saturating_sub(1);
                    i += 1;
                    continue;
                }
            }
        }
        let splittable = brace_depth == 0 && !in_quote && i >= stamp_end && segments.len() + 1 < depth;
        if splittable && bytes[i..].starts_with(sep) {
            segments.push(&key[start..i]);
            i += sep.len();
            start = i;
            continue;
        }
        i += 1;
    }
    segments.push(&key[start..]);
    (segments, brace_depth == 0 && !in_quote)
}

/// Byte length of the ISO-8601 timestamp starting at `bytes[at]`, or 0 when
/// there isn't one. Shape: `YYYY-MM-DD`, optionally followed by `T`/space +
/// `HH:MM`, then an optional `:SS`, an optional `.`/`,` fraction, and an
/// optional `Z` or `±HH:MM` / `±HHMM` offset.
///
/// The date is mandatory by design. It is the part that cannot be mistaken
/// for a namespace, and it is what licenses swallowing the separators that
/// follow it; a bare `12:30:45` is left to split normally.
fn iso_timestamp_len(bytes: &[u8], at: usize) -> usize {
    let two_digits = |i: usize| i + 1 < bytes.len() && bytes[i].is_ascii_digit() && bytes[i + 1].is_ascii_digit();
    // YYYY-MM-DD
    if at + 10 > bytes.len()
        || !bytes[at..at + 4].iter().all(u8::is_ascii_digit)
        || bytes[at + 4] != b'-'
        || !two_digits(at + 5)
        || bytes[at + 7] != b'-'
        || !two_digits(at + 8)
    {
        return 0;
    }
    let mut i = at + 10;
    // [T ]HH:MM — the whole time part is optional, so a plain date is a match.
    if !(matches!(bytes.get(i), Some(b'T' | b't' | b' '))
        && two_digits(i + 1)
        && bytes.get(i + 3) == Some(&b':')
        && two_digits(i + 4))
    {
        return i - at;
    }
    i += 6;
    if bytes.get(i) == Some(&b':') && two_digits(i + 1) {
        i += 3;
    }
    if matches!(bytes.get(i), Some(b'.' | b',')) && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    match bytes.get(i) {
        Some(b'Z' | b'z') => i += 1,
        Some(b'+' | b'-') if two_digits(i + 1) => {
            i += 3;
            if bytes.get(i) == Some(&b':') && two_digits(i + 1) {
                i += 3;
            } else if two_digits(i) {
                i += 2;
            }
        }
        _ => {}
    }
    i - at
}

/// Folder path prefixes for a key under the same rules as
/// [`new_key_tree_items`] — every intermediate segment is a folder id,
/// the final segment is the leaf (not returned).
pub(super) fn folder_prefixes(key: &str, separator: &str, max_key_tree_depth: usize) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut dir = String::new();
    for (index, part) in split_key_segments(key, separator, max_key_tree_depth)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            prefixes.push(dir.clone());
            dir.push_str(separator);
        }
        dir.push_str(part);
    }
    prefixes
}

/// Resolve a per-colour histogram into (mode colour, is_mixed, tooltip).
/// `None` when no tagged descendants.
pub(super) fn resolve_folder_tag_histogram(counts: &[u32; 6]) -> Option<(TagColor, bool, SharedString)> {
    let total: u32 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let mut best_ix = 0usize;
    let mut best = 0u32;
    let mut distinct = 0u32;
    for (i, &c) in counts.iter().enumerate() {
        if c > 0 {
            distinct += 1;
        }
        if c > best {
            best = c;
            best_ix = i;
        }
    }
    let mode = TagColor::ALL[best_ix];
    let mixed = distinct > 1;
    // Stable display order follows TagColor::ALL.
    let mut parts: Vec<String> = Vec::new();
    for (i, &c) in counts.iter().enumerate() {
        if c > 0 {
            parts.push(format!("{} {c}", TagColor::ALL[i].as_str()));
        }
    }
    let summary: SharedString = parts.join(" · ").into();
    Some((mode, mixed, summary))
}

/// Stamp folder rows with aggregated tag colours derived from **local**
/// metadata (not Redis). Only folders present in `items` are updated.
/// Statistics cover every tagged key whose path falls under that folder
/// prefix — including keys not yet in the SCAN page — so the bar matches
/// the tag-filter's "local metadata" philosophy.
pub(super) fn stamp_folder_tag_aggregates(
    items: &mut AHashMap<SharedString, KeyTreeItem>,
    metadata: &std::collections::HashMap<String, KeyMetadata>,
    separator: &str,
    max_key_tree_depth: usize,
) {
    if items.is_empty() || metadata.is_empty() {
        return;
    }
    let mut counts: AHashMap<String, [u32; 6]> = AHashMap::new();
    for (key, meta) in metadata {
        let Some(tag) = meta.tag else {
            continue;
        };
        let ix = tag_color_index(tag);
        for prefix in folder_prefixes(key, separator, max_key_tree_depth) {
            // Skip prefixes that are not folders in this tree (or not loaded).
            let Some(item) = items.get(prefix.as_str()) else {
                continue;
            };
            if !item.is_folder {
                continue;
            }
            counts.entry(prefix).or_insert([0; 6])[ix] += 1;
        }
    }
    for (folder_id, hist) in counts {
        let Some((mode, mixed, summary)) = resolve_folder_tag_histogram(&hist) else {
            continue;
        };
        if let Some(item) = items.get_mut(folder_id.as_str())
            && item.is_folder
        {
            item.tag = Some(mode);
            item.tag_mixed = mixed;
            item.folder_tag_summary = summary;
        }
    }
}

/// Expands the user's `expanded_items` through single-child folder chains:
/// while an expanded folder's only child is itself a folder, that child is
/// treated as expanded too. Lets a deep single-child namespace
/// (`app:user` → `profile` → leaves) open in one click instead of one click
/// per level. Returns the augmented set (owned, so the caller can borrow
/// `&str` views into it). Recomputed every rebuild, so a streaming scan that
/// later reveals a second child stops the auto-expand at that level on the
/// next pass. No-op (skips the child-map pass) when nothing is expanded.
pub(super) fn single_child_expanded_set(
    keys: &[(SharedString, KeyType)],
    expanded_items: &AHashSet<SharedString>,
    suppressed: &AHashSet<SharedString>,
    keyword: &str,
    separator: &str,
    max_depth: usize,
) -> AHashSet<String> {
    let mut effective: AHashSet<String> = expanded_items.iter().map(|s| s.to_string()).collect();
    if effective.is_empty() {
        return effective;
    }
    // For each folder prefix: (sole-child id, whether that child is itself a
    // folder, whether more than one distinct child was seen). Tracking just
    // the first child plus a "multiple" flag avoids a per-folder child set.
    let mut child_info: AHashMap<String, (String, bool, bool)> = AHashMap::new();
    for (key, _) in keys {
        if !keyword.is_empty() && !key.contains(keyword) {
            continue;
        }
        let segs = split_key_segments(key, separator, max_depth);
        // One segment = a plain leaf (no separator, or every separator sat
        // inside a hash tag / quoted blob) — nothing to fold.
        if segs.len() <= 1 {
            continue;
        }
        let mut dir = String::new();
        for (i, seg) in segs.iter().enumerate() {
            let parent = dir.clone();
            if i > 0 {
                dir.push_str(separator);
            }
            dir.push_str(seg);
            let child_is_folder = i + 1 < segs.len();
            match child_info.entry(parent) {
                Vacant(e) => {
                    e.insert((dir.clone(), child_is_folder, false));
                }
                Occupied(mut e) => {
                    let info = e.get_mut();
                    if info.0 == dir {
                        info.1 |= child_is_folder;
                    } else {
                        info.2 = true;
                    }
                }
            }
        }
    }
    // Follow single-folder-child links transitively from each expanded folder,
    // but never auto-open a folder the user explicitly collapsed.
    let suppressed_set: AHashSet<String> = suppressed.iter().map(|s| s.to_string()).collect();
    let mut stack: Vec<String> = effective.iter().cloned().collect();
    while let Some(dir) = stack.pop() {
        let Some((child, child_is_folder, multiple)) = child_info.get(dir.as_str()) else {
            continue;
        };
        if *multiple || !*child_is_folder || suppressed_set.contains(child) {
            continue;
        }
        if effective.insert(child.clone()) {
            stack.push(child.clone());
        }
    }
    effective
}

/// How sibling rows are ordered. Folders always come before leaves; this
/// decides the order inside each group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum KeySort {
    #[default]
    NameAsc,
    NameDesc,
    /// Soonest expiry first. Keys that never expire — and keys whose TTL
    /// the tree has not loaded — sort last in **both** directions: "never"
    /// is not a small number, and putting it at one end would bury the
    /// keys the order is for.
    TtlAsc,
    TtlDesc,
}

impl KeySort {
    pub(super) const ALL: [KeySort; 4] = [KeySort::NameAsc, KeySort::NameDesc, KeySort::TtlAsc, KeySort::TtlDesc];

    /// Wire id carried by the action payload.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            KeySort::NameAsc => "name_asc",
            KeySort::NameDesc => "name_desc",
            KeySort::TtlAsc => "ttl_asc",
            KeySort::TtlDesc => "ttl_desc",
        }
    }

    pub(super) fn from_name(name: &str) -> Self {
        KeySort::ALL
            .into_iter()
            .find(|sort| sort.as_str() == name)
            .unwrap_or_default()
    }

    /// Key in the `key_tree` i18n section.
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            KeySort::NameAsc => "sort_name_asc",
            KeySort::NameDesc => "sort_name_desc",
            KeySort::TtlAsc => "sort_ttl_asc",
            KeySort::TtlDesc => "sort_ttl_desc",
        }
    }

    /// Ordering for one pair of sibling rows, folders first.
    fn compare(self, a: &KeyTreeItem, b: &KeyTreeItem) -> std::cmp::Ordering {
        b.is_folder.cmp(&a.is_folder).then_with(|| match self {
            KeySort::NameAsc => a.label.cmp(&b.label),
            KeySort::NameDesc => b.label.cmp(&a.label),
            KeySort::TtlAsc => {
                let (group_a, ttl_a) = ttl_rank(a);
                let (group_b, ttl_b) = ttl_rank(b);
                group_a
                    .cmp(&group_b)
                    .then(ttl_a.cmp(&ttl_b))
                    .then_with(|| a.label.cmp(&b.label))
            }
            KeySort::TtlDesc => {
                let (group_a, ttl_a) = ttl_rank(a);
                let (group_b, ttl_b) = ttl_rank(b);
                group_a
                    .cmp(&group_b)
                    .then(ttl_b.cmp(&ttl_a))
                    .then_with(|| a.label.cmp(&b.label))
            }
        })
    }
}

/// `(group, ttl)` — group 0 is a key with a live expiry, group 1 everything
/// else (no expiry, unknown, folders), so the second group always trails
/// whichever direction the TTL sort runs in.
fn ttl_rank(item: &KeyTreeItem) -> (u8, i64) {
    match item.ttl_secs {
        Some(ttl) if ttl >= 0 => (0, ttl),
        _ => (1, 0),
    }
}

/// How the keyword narrows the already-loaded keys during the build.
pub(super) enum KeywordMatch {
    /// Everything passes — no keyword, or a regex that would not compile.
    All,
    /// Substring, the same test the server-side `SCAN MATCH *kw*` makes.
    Contains(SharedString),
    /// A regex over the whole key name. Local only: `SCAN` has no regex, so
    /// the scan that produced this list ran unfiltered.
    Regex(Box<Regex>),
}

impl KeywordMatch {
    fn matches(&self, key: &str) -> bool {
        match self {
            KeywordMatch::All => true,
            KeywordMatch::Contains(keyword) => key.contains(keyword.as_str()),
            KeywordMatch::Regex(regex) => regex.is_match(key),
        }
    }

    /// The substring the folder passes use to keep a path visible. A regex
    /// has no such prefix, so those passes fall back to "no keyword".
    fn substring(&self) -> SharedString {
        match self {
            KeywordMatch::Contains(keyword) => keyword.clone(),
            _ => SharedString::default(),
        }
    }
}

/// Inputs for [`new_key_tree_items`] — keeps the builder signature under
/// clippy's argument limit without losing the per-concern docs.
pub(super) struct KeyTreeBuildInput<'a> {
    /// **Must be sorted by key** (see [`new_key_tree_items`]'s contract).
    pub keys: Vec<(SharedString, KeyType)>,
    pub keyword: KeywordMatch,
    /// Order of sibling rows.
    pub sort: KeySort,
    /// Flat mode: one row per key, full name as the label, no folders. The
    /// separator still groups nothing, so a deep namespace reads as a plain
    /// list instead of a tree.
    pub flat: bool,
    pub expanded_items: AHashSet<SharedString>,
    pub suppressed: AHashSet<SharedString>,
    pub separator: &'a str,
    pub max_key_tree_depth: usize,
    pub key_ttls: &'a AHashMap<SharedString, i64>,
    /// Pre-loaded client-side annotations for the current server.
    /// Looked up by exact key name when building leaf items so each
    /// row carries its own tag/note copy and `render_item` doesn't
    /// have to touch the manager per frame. Empty map is fine — no
    /// metadata simply means no badges. Tag / type / TTL filtering
    /// happens upstream via [`apply_local_key_filters`].
    pub metadata: &'a std::collections::HashMap<String, KeyMetadata>,
}

/// Build the tree rows. **`input.keys` must be sorted by key** — both
/// producers guarantee it (the snapshot is sorted once per key-set change
/// in `update_key_tree`, the tag union sorts its own output), so the
/// per-rebuild O(N log N) sort that used to live here is gone: an
/// expand/collapse or filter change now only pays the linear passes.
pub(super) fn new_key_tree_items(input: KeyTreeBuildInput<'_>) -> Vec<KeyTreeItem> {
    let KeyTreeBuildInput {
        keys,
        keyword,
        sort,
        flat,
        expanded_items,
        suppressed,
        separator,
        max_key_tree_depth,
        key_ttls,
        metadata,
    } = input;
    debug_assert!(
        keys.is_sorted_by(|(a, _), (b, _)| a <= b),
        "new_key_tree_items requires key-sorted input"
    );
    if flat {
        return flat_key_items(keys, &keyword, sort, key_ttls, metadata);
    }
    // The folder passes reason about a literal prefix; a regex has none, so
    // they see "no keyword" and the per-key test below does the filtering.
    let keyword_substring = keyword.substring();
    // Effective expansion = the user-expanded folders plus any single-child
    // folder chains hanging off them, so drilling into a deep single-child
    // namespace (`app:user` → `profile` → leaves) opens straight through in
    // one click instead of one click per level.
    let effective_expanded = single_child_expanded_set(
        &keys,
        &expanded_items,
        &suppressed,
        &keyword_substring,
        separator,
        max_key_tree_depth,
    );
    let expanded_items_set = effective_expanded
        .iter()
        .map(|s| s.as_str())
        .collect::<AHashSet<&str>>();
    let mut items: AHashMap<SharedString, KeyTreeItem> = AHashMap::with_capacity(100);
    // Tracks standalone keys whose HashMap slot was taken over by a folder
    // with the same name (e.g. key "test" exists alongside "test:key1").
    // These are re-inserted as **siblings** of the folder so both remain
    // visible at the same tree level.
    let mut promoted_leaves: Vec<(SharedString, KeyType, SharedString, usize)> = Vec::new();

    for (key, key_type) in keys {
        if !keyword.matches(key.as_ref()) {
            continue;
        }
        let ttl_for_leaf = key_ttls.get(&key).copied();
        let (tag_for_leaf, note_for_leaf) = match metadata.get(key.as_ref()) {
            Some(m) => (m.tag, SharedString::from(m.note.clone())),
            None => (None, SharedString::default()),
        };
        let segments = split_key_segments(key.as_ref(), separator, max_key_tree_depth);
        if segments.len() <= 1 {
            items.insert(
                key.clone(),
                KeyTreeItem {
                    id: key.clone(),
                    label: key.clone(),
                    key_type,
                    ttl_secs: ttl_for_leaf,
                    tag: tag_for_leaf,
                    note: note_for_leaf,
                    ..Default::default()
                },
            );
            continue;
        }

        let mut dir = String::with_capacity(50);
        // Deferred pending ancestor as `(id_len, label_len, depth, expanded)`
        // spans into `dir` — in a dense namespace most folds hit an existing
        // folder entry, so materialising id/label strings eagerly (the old
        // shape) allocated two throwaway strings per key per level. Strings
        // are now built only on first sight (the miss branch below).
        let mut pending: Option<(usize, usize, usize, bool)> = None;
        for (index, k) in segments.into_iter().enumerate() {
            let expanded = index == 0 || expanded_items_set.contains(dir.as_str());
            if let Some((id_len, label_len, depth, _)) = pending.take() {
                // `dir` still ends exactly at the pending path — the current
                // segment is appended only after the fold.
                let path = &dir[..id_len];
                if let Some(existing) = items.get_mut(path) {
                    if !existing.is_folder {
                        promoted_leaves.push((
                            existing.id.clone(),
                            existing.key_type,
                            existing.label.clone(),
                            existing.depth,
                        ));
                    }
                    existing.is_folder = true;
                    existing.children_count += 1;
                    existing.expanded = expanded;
                } else {
                    let id: SharedString = path.to_string().into();
                    let label: SharedString = dir[id_len - label_len..id_len].to_string().into();
                    items.insert(
                        id.clone(),
                        KeyTreeItem {
                            id,
                            label,
                            key_type,
                            depth,
                            expanded,
                            is_folder: true,
                            children_count: 1,
                            ..Default::default()
                        },
                    );
                }
            }

            if !expanded {
                break;
            }
            if index != 0 {
                dir.push_str(separator);
            };
            dir.push_str(k);
            pending = Some((dir.len(), k.len(), index, expanded));
        }
        if let Some((id_len, label_len, depth, expanded)) = pending.take() {
            // This is the deepest level for this key — guaranteed a leaf
            // since no further segment was promoted. `dir` now equals the
            // full key, so the id reuses the key's `SharedString` (an Arc
            // bump for heap-backed keys) instead of allocating a copy.
            debug_assert_eq!(&dir[..id_len], key.as_ref());
            let label: SharedString = dir[id_len - label_len..id_len].to_string().into();
            items.insert(
                key.clone(),
                KeyTreeItem {
                    id: key.clone(),
                    label,
                    key_type,
                    depth,
                    expanded,
                    ttl_secs: ttl_for_leaf,
                    tag: tag_for_leaf,
                    note: note_for_leaf.clone(),
                    ..Default::default()
                },
            );
        }
    }

    // After all leaves/folders exist, derive folder left-bar colours from
    // local tag metadata (mode + mixed summary). Leaves already stamped.
    stamp_folder_tag_aggregates(&mut items, metadata, separator, max_key_tree_depth);

    let mut children_map: AHashMap<String, Vec<KeyTreeItem>> = AHashMap::new();

    for item in items.into_values() {
        let size = item.id.len() - item.label.len();
        let parent_id = if size == 0 { "" } else { &item.id[..(size - 1)] };
        // `entry(parent_id.to_string())` would allocate the key for every
        // item; allocate only when the bucket doesn't exist yet (folders
        // are a small fraction of items).
        match children_map.get_mut(parent_id) {
            Some(bucket) => bucket.push(item),
            None => {
                children_map.insert(parent_id.to_string(), vec![item]);
            }
        }
    }

    for (key_id, key_type, label, depth) in promoted_leaves {
        let size = key_id.len() - label.len();
        let parent_id = if size == 0 { "" } else { &key_id[..(size - 1)] };
        let ttl_secs = key_ttls.get(&key_id).copied();
        // Same lookup as the main leaf path; promoted leaves are
        // standalone keys displaced from their slot by a same-named
        // folder, so they still want their own annotation.
        let (tag, note) = match metadata.get(key_id.as_ref()) {
            Some(m) => (m.tag, SharedString::from(m.note.clone())),
            None => (None, SharedString::default()),
        };
        let leaf = KeyTreeItem {
            // Clone (an Arc bump) — `parent_id` still borrows `key_id` for
            // the bucket lookup below.
            id: key_id.clone(),
            label,
            depth,
            key_type,
            ttl_secs,
            tag,
            note,
            ..Default::default()
        };
        match children_map.get_mut(parent_id) {
            Some(bucket) => bucket.push(leaf),
            None => {
                children_map.insert(parent_id.to_string(), vec![leaf]);
            }
        }
    }

    let mut result = Vec::with_capacity(children_map.values().map(|v| v.len()).sum());

    fn build_sorted_list(
        parent_id: &str,
        map: &mut AHashMap<String, Vec<KeyTreeItem>>,
        result: &mut Vec<KeyTreeItem>,
        sort: KeySort,
    ) {
        if let Some(mut children) = map.remove(parent_id) {
            children.sort_unstable_by(|a, b| sort.compare(a, b));

            // Zebra index restarts under each parent: among this parent's leaf
            // (non-folder) children, every second one (2nd, 4th, …) is striped.
            // Folders are skipped and don't advance the count.
            let mut leaf_ix = 0usize;
            for mut child in children {
                if !child.is_folder {
                    child.stripe = leaf_ix % 2 == 1;
                    leaf_ix += 1;
                }
                // SharedString clone instead of `to_string` — the id only
                // needs to outlive the recursive call below.
                let child_id = child.id.clone();
                result.push(child);
                build_sorted_list(child_id.as_ref(), map, result, sort);
            }
        }
    }

    build_sorted_list("", &mut children_map, &mut result, sort);

    result
}

/// Flat mode: every key is one depth-0 row labelled with its full name.
/// None of the folder machinery runs, so the later folder passes
/// (`fill_parent_indices`, tag aggregates, "Load more") find nothing to do.
fn flat_key_items(
    keys: Vec<(SharedString, KeyType)>,
    keyword: &KeywordMatch,
    sort: KeySort,
    key_ttls: &AHashMap<SharedString, i64>,
    metadata: &std::collections::HashMap<String, KeyMetadata>,
) -> Vec<KeyTreeItem> {
    let mut items: Vec<KeyTreeItem> = keys
        .into_iter()
        .filter(|(key, _)| keyword.matches(key.as_ref()))
        .map(|(key, key_type)| {
            let (tag, note) = match metadata.get(key.as_ref()) {
                Some(meta) => (meta.tag, SharedString::from(meta.note.clone())),
                None => (None, SharedString::default()),
            };
            KeyTreeItem {
                id: key.clone(),
                label: key.clone(),
                key_type,
                ttl_secs: key_ttls.get(&key).copied(),
                tag,
                note,
                ..Default::default()
            }
        })
        .collect();
    items.sort_unstable_by(|a, b| sort.compare(a, b));
    // Zebra stripes run down the whole list here — there is no parent to
    // restart them under.
    for (index, item) in items.iter_mut().enumerate() {
        item.stripe = index % 2 == 1;
    }
    items
}

/// Appends a synthetic "Load more" row after the visible children of any
/// expanded folder whose prefix scan stopped at the page cap (tracked in
/// `incomplete`). Clicking that row resumes the scan. No-op when nothing is
/// incomplete. `incomplete` holds prefixes in `"{folder_id}:"` form — the same
/// shape `scan_prefix` receives.
pub(super) fn append_load_more_rows(
    items: Vec<KeyTreeItem>,
    incomplete: &AHashSet<SharedString>,
    label: &SharedString,
    separator: &str,
) -> Vec<KeyTreeItem> {
    if incomplete.is_empty() {
        return items;
    }
    let len = items.len();
    // For each incomplete + expanded folder: the index just past its last
    // descendant (where the row belongs) and the row to insert there.
    let mut inserts: Vec<(usize, KeyTreeItem)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !(item.is_folder && item.expanded) {
            continue;
        }
        let prefix = SharedString::from(format!("{}{separator}", item.id));
        if !incomplete.contains(&prefix) {
            continue;
        }
        // A folder's subtree is contiguous and strictly deeper; it ends at the
        // first later row whose depth is not greater (or the end of the list).
        let mut end = i + 1;
        while end < len && items[end].depth > item.depth {
            end += 1;
        }
        let row = KeyTreeItem {
            id: SharedString::from(format!("{prefix}\u{1}load_more")),
            // Suffix the folder name so stacked rows from nested incomplete
            // folders ("bench:" and a deeper "…:rank:" both ending at the same
            // list position) are tellable apart.
            label: SharedString::from(format!("{label} · {}", item.label)),
            depth: item.depth + 1,
            load_more_prefix: Some(prefix),
            // Loaded-so-far count, rendered right-aligned exactly like the
            // folder rows' own count — same column, same meaning, so no
            // localized wording ("300 loaded") is needed.
            children_count: item.children_count,
            ..Default::default()
        };
        inserts.push((end, row));
    }
    if inserts.is_empty() {
        return items;
    }
    let mut result: Vec<KeyTreeItem> = Vec::with_capacity(len + inserts.len());
    for (i, item) in items.into_iter().enumerate() {
        // Nested folders share their subtree-end index with their ancestors;
        // the deeper row was generated later, and must be emitted first so it
        // sits inside the parent's subtree (right under its own folder's
        // children) with the ancestor's row below it.
        for (end, row) in inserts.iter().rev() {
            if *end == i {
                result.push(row.clone());
            }
        }
        result.push(item);
    }
    // Folders whose subtree runs to the very end of the list — reversed for
    // the same deepest-first ordering as above.
    for (end, row) in inserts.into_iter().rev() {
        if end == len {
            result.push(row);
        }
    }
    result
}

/// True when a folder's subtree ends inside `items[from..limit]` — i.e. a row
/// at or above the folder's own depth shows up — or the list itself runs out
/// before `limit` (nothing left to scroll to). Used to trim sticky entries:
/// when you can already see where a folder ends, pinning its name adds no
/// context and just covers rows.
pub(super) fn subtree_ends_before(items: &[KeyTreeItem], from: usize, folder_depth: usize, limit: usize) -> bool {
    let limit = limit.min(items.len());
    for item in &items[from..limit] {
        if item.depth <= folder_depth {
            return true;
        }
    }
    limit == items.len()
}

/// Fills each row's `parent_ix` with the index of its nearest ancestor folder,
/// walking the flattened depth-first list once with a depth stack. O(n).
pub(super) fn fill_parent_indices(items: &mut [KeyTreeItem]) {
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..items.len() {
        let depth = items[i].depth;
        while stack.last().is_some_and(|&top| items[top].depth >= depth) {
            stack.pop();
        }
        items[i].parent_ix = stack.last().copied();
        if items[i].is_folder {
            stack.push(i);
        }
    }
}

#[cfg(test)]
mod split_key_segments_tests {
    use super::split_key_segments;

    #[test]
    fn plain_keys_split_like_splitn() {
        assert_eq!(
            split_key_segments("user:1:profile", ":", 5),
            vec!["user", "1", "profile"]
        );
        // No separator at all — one segment, so callers treat it as a leaf.
        assert_eq!(split_key_segments("plainkey", ":", 5), vec!["plainkey"]);
        // The depth cap keeps the remainder in the last segment.
        assert_eq!(split_key_segments("a:b:c:d", ":", 3), vec!["a", "b", "c:d"]);
        // Multi-character separators work the same way.
        assert_eq!(split_key_segments("a::b::c", "::", 5), vec!["a", "b", "c"]);
    }

    #[test]
    fn hash_tags_stay_one_segment() {
        // Redis cluster hash tag containing the separator (issue #119).
        assert_eq!(
            split_key_segments("user:{tenant:42}:profile", ":", 5),
            vec!["user", "{tenant:42}", "profile"]
        );
    }

    #[test]
    fn json_blob_in_key_is_not_shattered() {
        // The reported key shape: metadata JSON appended to the key name.
        let key = r#"table:{j-groups}:item:007dc97e/{"name": "td-splunk", "at": "12:13:05"}"#;
        assert_eq!(
            split_key_segments(key, ":", 5),
            vec![
                "table",
                "{j-groups}",
                "item",
                r#"007dc97e/{"name": "td-splunk", "at": "12:13:05"}"#,
            ]
        );
    }

    #[test]
    fn quoted_separator_outside_braces_is_ignored_too() {
        assert_eq!(
            split_key_segments(r#"log:"12:13:05":done"#, ":", 5),
            vec!["log", r#""12:13:05""#, "done"]
        );
        // An escaped quote does not open a quoted run.
        assert_eq!(split_key_segments(r#"a:\":b"#, ":", 5), vec!["a", r#"\""#, "b"]);
    }

    /// Issue #127: the colons of a timestamp are not namespace levels.
    #[test]
    fn timestamps_keep_their_own_colons() {
        // The reported key: an ISO timestamp with fraction and UTC offset,
        // sitting in a keyspace that separates its levels with "/".
        let key = "jtable#abc/user/2022-05-11 11:55:44.487892+00:00/x";
        assert_eq!(split_key_segments(key, ":", 5), vec![key]);
        // Colons around the timestamp still separate levels.
        assert_eq!(
            split_key_segments("evt:2022-05-11T11:55:44Z:done", ":", 5),
            vec!["evt", "2022-05-11T11:55:44Z", "done"]
        );
        assert_eq!(
            split_key_segments("a:2022-05-11 11:55:44.487892+00:00:b", ":", 5),
            vec!["a", "2022-05-11 11:55:44.487892+00:00", "b"]
        );
        // A date holds together for a "-" separator the same way.
        assert_eq!(
            split_key_segments("log-2022-05-11-x", "-", 5),
            vec!["log", "2022-05-11", "x"]
        );
    }

    /// The date is what marks a timestamp: digits on their own are a normal
    /// namespace far more often than a time, so they keep splitting.
    #[test]
    fn digits_without_a_date_still_split() {
        assert_eq!(
            split_key_segments("app:a02:svc06:prod:sh03:k005", ":", 10),
            vec!["app", "a02", "svc06", "prod", "sh03", "k005"]
        );
        assert_eq!(
            split_key_segments("job:12:30:45", ":", 10),
            vec!["job", "12", "30", "45"]
        );
        // Year-like runs are not dates either.
        assert_eq!(
            split_key_segments("stats:2024:01:02", ":", 10),
            vec!["stats", "2024", "01", "02"]
        );
    }

    #[test]
    fn unbalanced_braces_or_quotes_fall_back_to_plain_split() {
        // Without the fallback these would collapse into a single segment.
        assert_eq!(split_key_segments("a:{b:c", ":", 5), vec!["a", "{b", "c"]);
        assert_eq!(split_key_segments(r#"a:"b:c"#, ":", 5), vec!["a", r#""b"#, "c"]);
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert_eq!(split_key_segments("a:b", "", 5), vec!["a:b"]);
        assert_eq!(split_key_segments("a:b", ":", 1), vec!["a:b"]);
        assert_eq!(split_key_segments("a:b", ":", 0), vec!["a:b"]);
        // Multi-byte content must not panic or split mid-codepoint.
        assert_eq!(split_key_segments("用户:1:名字", ":", 5), vec!["用户", "1", "名字"]);
    }
}

#[cfg(test)]
mod sort_and_flat_tests {
    use super::*;

    fn leaf(label: &str, ttl: Option<i64>) -> KeyTreeItem {
        KeyTreeItem {
            id: label.into(),
            label: label.into(),
            ttl_secs: ttl,
            ..Default::default()
        }
    }

    fn ordered(sort: KeySort, mut items: Vec<KeyTreeItem>) -> Vec<String> {
        items.sort_unstable_by(|a, b| sort.compare(a, b));
        items.into_iter().map(|item| item.label.to_string()).collect()
    }

    #[test]
    fn folders_lead_every_order() {
        let folder = KeyTreeItem {
            id: "zzz".into(),
            label: "zzz".into(),
            is_folder: true,
            ..Default::default()
        };
        for sort in KeySort::ALL {
            let order = ordered(sort, vec![leaf("aaa", Some(1)), folder.clone()]);
            assert_eq!(order.first().map(String::as_str), Some("zzz"), "{sort:?}");
        }
    }

    #[test]
    fn ttl_orders_put_the_ones_that_never_expire_last_either_way() {
        let items = || {
            vec![
                leaf("soon", Some(10)),
                leaf("later", Some(1000)),
                leaf("never", Some(-1)),
                leaf("unknown", None),
            ]
        };
        assert_eq!(
            ordered(KeySort::TtlAsc, items()),
            vec!["soon", "later", "never", "unknown"]
        );
        // Reversed among the keys that do expire; the other two still trail,
        // in a stable order of their own.
        assert_eq!(
            ordered(KeySort::TtlDesc, items()),
            vec!["later", "soon", "never", "unknown"]
        );
    }

    #[test]
    fn name_orders_are_plain_reverses() {
        let items = || vec![leaf("b", None), leaf("a", None), leaf("c", None)];
        assert_eq!(ordered(KeySort::NameAsc, items()), vec!["a", "b", "c"]);
        assert_eq!(ordered(KeySort::NameDesc, items()), vec!["c", "b", "a"]);
    }

    #[test]
    fn sort_ids_round_trip_and_an_unknown_one_falls_back() {
        for sort in KeySort::ALL {
            assert_eq!(KeySort::from_name(sort.as_str()), sort);
        }
        assert_eq!(KeySort::from_name("nope"), KeySort::NameAsc);
    }

    #[test]
    fn the_keyword_matcher_covers_substring_and_regex() {
        assert!(KeywordMatch::All.matches("anything"));
        assert!(KeywordMatch::All.substring().is_empty());

        let contains = KeywordMatch::Contains("ser".into());
        assert!(contains.matches("user:1"));
        assert!(!contains.matches("order:1"));
        assert_eq!(contains.substring().as_ref(), "ser");

        let regex = KeywordMatch::Regex(Box::new(Regex::new(r"^user:\d+$").expect("regex")));
        assert!(regex.matches("user:42"));
        assert!(!regex.matches("user:abc"));
        // A regex has no literal prefix for the folder passes to reason about.
        assert!(regex.substring().is_empty());
    }

    #[test]
    fn flat_mode_is_one_row_per_key_with_no_folders() {
        let keys = vec![
            ("user:1".into(), KeyType::String),
            ("user:2".into(), KeyType::String),
            ("order:9".into(), KeyType::Hash),
        ];
        let mut ttls: AHashMap<SharedString, i64> = AHashMap::new();
        ttls.insert("user:2".into(), 5);
        let metadata = std::collections::HashMap::new();
        let items = flat_key_items(keys, &KeywordMatch::All, KeySort::NameAsc, &ttls, &metadata);
        assert_eq!(
            items.iter().map(|i| i.label.to_string()).collect::<Vec<_>>(),
            vec!["order:9", "user:1", "user:2"],
            "full key names, no folder rows"
        );
        assert!(items.iter().all(|i| !i.is_folder && i.depth == 0));
        // Stripes run down the whole list, since there is no parent.
        assert_eq!(
            items.iter().map(|i| i.stripe).collect::<Vec<_>>(),
            vec![false, true, false]
        );
        // The TTL order reaches across what would have been folders.
        let keys = vec![
            ("user:1".into(), KeyType::String),
            ("user:2".into(), KeyType::String),
            ("order:9".into(), KeyType::Hash),
        ];
        let by_ttl = flat_key_items(keys, &KeywordMatch::All, KeySort::TtlAsc, &ttls, &metadata);
        assert_eq!(by_ttl.first().map(|i| i.label.to_string()), Some("user:2".to_string()));
    }
}

#[cfg(test)]
mod folder_tag_aggregate_tests {
    use super::*;

    #[test]
    fn folder_prefixes_match_tree_splitn() {
        assert!(folder_prefixes("solo", ":", 10).is_empty());
        assert_eq!(
            folder_prefixes("a:b:c", ":", 10),
            vec!["a".to_string(), "a:b".to_string()]
        );
        // Depth cap: remaining path is one leaf segment.
        assert_eq!(
            folder_prefixes("a:b:c:d", ":", 3),
            vec!["a".to_string(), "a:b".to_string()]
        );
    }

    #[test]
    fn histogram_mode_and_mixed() {
        let mut counts = [0u32; 6];
        counts[tag_color_index(TagColor::Red)] = 3;
        counts[tag_color_index(TagColor::Blue)] = 1;
        let (mode, mixed, summary) = resolve_folder_tag_histogram(&counts).expect("some");
        assert_eq!(mode, TagColor::Red);
        assert!(mixed);
        assert!(summary.as_ref().contains("red 3"));
        assert!(summary.as_ref().contains("blue 1"));
    }

    #[test]
    fn stamp_sets_folder_mode_from_metadata() {
        let mut items: AHashMap<SharedString, KeyTreeItem> = AHashMap::new();
        items.insert(
            "user".into(),
            KeyTreeItem {
                id: "user".into(),
                label: "user".into(),
                is_folder: true,
                children_count: 2,
                ..Default::default()
            },
        );
        items.insert(
            "user:1".into(),
            KeyTreeItem {
                id: "user:1".into(),
                label: "1".into(),
                depth: 1,
                tag: Some(TagColor::Red),
                ..Default::default()
            },
        );
        let mut meta = std::collections::HashMap::new();
        meta.insert(
            "user:1".into(),
            KeyMetadata {
                tag: Some(TagColor::Red),
                note: String::new(),
            },
        );
        meta.insert(
            "user:2".into(),
            KeyMetadata {
                tag: Some(TagColor::Red),
                note: String::new(),
            },
        );
        // user:2 not in items (not scanned) still counts for folder aggregate.
        stamp_folder_tag_aggregates(&mut items, &meta, ":", 10);
        let folder = items.get("user").expect("folder");
        assert_eq!(folder.tag, Some(TagColor::Red));
        assert!(!folder.tag_mixed);
        assert!(folder.folder_tag_summary.as_ref().contains("red 2"));
    }
}

#[cfg(test)]
mod local_filter_tests {
    use super::*;

    fn keys(items: &[(&str, KeyType)]) -> Vec<(SharedString, KeyType)> {
        items.iter().map(|(k, t)| ((*k).into(), *t)).collect()
    }

    #[test]
    fn type_and_drops_unknown_and_mismatches() {
        let input = keys(&[("a", KeyType::String), ("b", KeyType::Hash), ("c", KeyType::Unknown)]);
        let out = apply_local_key_filters(
            input,
            Some(KeyType::String),
            None,
            TtlFilter::All,
            &AHashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.as_ref(), "a");
    }

    #[test]
    fn one_probabilistic_filter_matches_every_sketch_kind() {
        use crate::states::ProbKind;
        let input = keys(&[
            ("bf", KeyType::Probabilistic(ProbKind::Bloom)),
            ("topk", KeyType::Probabilistic(ProbKind::TopK)),
            ("json", KeyType::Json),
            ("str", KeyType::String),
        ]);
        let out = apply_local_key_filters(
            input,
            // The menu's single "Probabilistic" entry dispatches Bloom as
            // the representative kind.
            Some(KeyType::Probabilistic(ProbKind::Bloom)),
            None,
            TtlFilter::All,
            &AHashMap::new(),
            &std::collections::HashMap::new(),
        );
        let names: Vec<&str> = out.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(names, ["bf", "topk"]);
    }

    #[test]
    fn tag_and_type_and_ttl_intersection() {
        let input = keys(&[
            ("red-hash-live", KeyType::Hash),
            ("red-str-live", KeyType::String),
            ("blue-hash-live", KeyType::Hash),
            ("red-hash-perm", KeyType::Hash),
            ("red-hash-expiring", KeyType::Hash),
        ]);
        let mut ttls = AHashMap::new();
        ttls.insert("red-hash-live".into(), 3600);
        ttls.insert("red-str-live".into(), 3600);
        ttls.insert("blue-hash-live".into(), 3600);
        ttls.insert("red-hash-perm".into(), -1);
        ttls.insert("red-hash-expiring".into(), 30);

        let mut meta = std::collections::HashMap::new();
        for k in ["red-hash-live", "red-str-live", "red-hash-perm", "red-hash-expiring"] {
            meta.insert(
                k.to_string(),
                KeyMetadata {
                    tag: Some(TagColor::Red),
                    note: String::new(),
                },
            );
        }
        meta.insert(
            "blue-hash-live".into(),
            KeyMetadata {
                tag: Some(TagColor::Blue),
                note: String::new(),
            },
        );

        let out = apply_local_key_filters(
            input,
            Some(KeyType::Hash),
            Some(TagColor::Red),
            TtlFilter::Lt1h,
            &ttls,
            &meta,
        );
        // red-hash-live (3600 is NOT < 3600) → out
        // red-hash-expiring (30) → in
        // red-hash-perm (-1) → out of Lt1h
        // red-str-live wrong type
        // blue wrong tag
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.as_ref(), "red-hash-expiring");
    }

    #[test]
    fn missing_ttl_never_matches_constrained_filter() {
        let input = keys(&[("x", KeyType::String)]);
        let out = apply_local_key_filters(
            input,
            None,
            None,
            TtlFilter::NoTtl,
            &AHashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(out.is_empty());
    }
}

#[cfg(test)]
mod load_more_tests {
    use super::*;

    fn folder(id: &str, label: &str, depth: usize) -> KeyTreeItem {
        KeyTreeItem {
            id: id.into(),
            label: label.into(),
            depth,
            is_folder: true,
            expanded: true,
            ..Default::default()
        }
    }
    fn leaf(id: &str, depth: usize) -> KeyTreeItem {
        KeyTreeItem {
            id: id.into(),
            label: id.into(),
            depth,
            ..Default::default()
        }
    }

    /// Nested incomplete folders whose subtrees end at the same (tail)
    /// position: the deeper folder's row must come first, right under its own
    /// children, with the ancestor's row below it — and each row's label names
    /// its folder.
    #[test]
    fn nested_tail_rows_are_deepest_first_and_named() {
        let mut rank = folder("bench:rank", "rank", 1);
        rank.children_count = 300;
        let items = vec![folder("bench", "bench", 0), rank, leaf("bench:rank:1", 2)];
        let incomplete: AHashSet<SharedString> = ["bench:".into(), "bench:rank:".into()].into_iter().collect();
        let label = SharedString::from("Load more");
        let out = append_load_more_rows(items, &incomplete, &label, ":");
        let rows: Vec<_> = out.iter().filter(|i| i.load_more_prefix.is_some()).collect();
        assert_eq!(rows.len(), 2, "one row per incomplete expanded folder");
        assert_eq!(rows[0].load_more_prefix.as_deref(), Some("bench:rank:"));
        assert_eq!(rows[0].label.as_ref(), "Load more · rank");
        assert_eq!(rows[0].depth, 2);
        assert_eq!(rows[0].children_count, 300, "loaded count carried onto the row");
        assert_eq!(rows[1].load_more_prefix.as_deref(), Some("bench:"));
        assert_eq!(rows[1].label.as_ref(), "Load more · bench");
        assert_eq!(rows[1].depth, 1);
    }

    /// Each row's `parent_ix` points at its nearest ancestor folder; siblings
    /// after a nested subtree pop back to the right ancestor.
    #[test]
    fn parent_indices_follow_depth_stack() {
        let mut items = vec![
            folder("bench", "bench", 0),
            folder("bench:rank", "rank", 1),
            leaf("bench:rank:1", 2),
            leaf("bench:x", 1),
            folder("other", "other", 0),
            leaf("other:1", 1),
        ];
        fill_parent_indices(&mut items);
        let parents: Vec<Option<usize>> = items.iter().map(|i| i.parent_ix).collect();
        assert_eq!(parents, vec![None, Some(0), Some(1), Some(0), None, Some(4)]);
    }

    /// Sticky trimming: a folder whose subtree ends within the visible window
    /// (or at the end of the list) should not pin.
    #[test]
    fn subtree_end_visibility() {
        let items = vec![
            folder("bench", "bench", 0),
            leaf("bench:1", 1),
            leaf("bench:2", 1),
            folder("other", "other", 0), // ends bench's subtree at index 3
            leaf("other:1", 1),
        ];
        // Scanning from bench's first child with the boundary in range → ends.
        assert!(subtree_ends_before(&items, 1, 0, 4));
        // Boundary (index 3) outside the window → subtree continues off-screen.
        assert!(!subtree_ends_before(&items, 1, 0, 3));
        // Window running past the end of the list counts as "end visible".
        assert!(subtree_ends_before(&items, 4, 0, 10));
    }
}
