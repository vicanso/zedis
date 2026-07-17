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
    metadata
        .iter()
        .filter(|(_, m)| m.tag == Some(color))
        .map(|(key, _)| {
            let key_type = type_by_key.get(key.as_str()).copied().unwrap_or(KeyType::Unknown);
            (SharedString::from(key.clone()), key_type)
        })
        .collect()
}

/// Local AND over the candidate key list (already obtained from SCAN
/// and/or the local tag index). Does **not** issue Redis commands —
/// type may still have been narrowed server-side via `SCAN TYPE`.
///
/// Dimensions (any `None` / `All` is a no-op for that axis):
/// - `type_filter`: exact `KeyType` match; `Unknown` never matches
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
                && *key_type != want
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

/// Folder path prefixes for a key under the same `splitn` rules as
/// [`new_key_tree_items`] — every intermediate segment is a folder id,
/// the final segment is the leaf (not returned).
pub(super) fn folder_prefixes(key: &str, separator: &str, max_key_tree_depth: usize) -> Vec<String> {
    let depth = max_key_tree_depth.max(1);
    let mut prefixes = Vec::new();
    let mut dir = String::new();
    for (index, part) in key.splitn(depth, separator).enumerate() {
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
        if !key.contains(separator) {
            continue;
        }
        let segs: Vec<&str> = key.splitn(max_depth, separator).collect();
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

/// Inputs for [`new_key_tree_items`] — keeps the builder signature under
/// clippy's argument limit without losing the per-concern docs.
pub(super) struct KeyTreeBuildInput<'a> {
    pub keys: Vec<(SharedString, KeyType)>,
    pub keyword: SharedString,
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

pub(super) fn new_key_tree_items(input: KeyTreeBuildInput<'_>) -> Vec<KeyTreeItem> {
    let KeyTreeBuildInput {
        mut keys,
        keyword,
        expanded_items,
        suppressed,
        separator,
        max_key_tree_depth,
        key_ttls,
        metadata,
    } = input;
    // `sort_unstable_by_key` would clone the `SharedString` on *every*
    // comparison (~n·log n Arc bumps); compare by borrow instead.
    keys.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    // Effective expansion = the user-expanded folders plus any single-child
    // folder chains hanging off them, so drilling into a deep single-child
    // namespace (`app:user` → `profile` → leaves) opens straight through in
    // one click instead of one click per level.
    let effective_expanded = single_child_expanded_set(
        &keys,
        &expanded_items,
        &suppressed,
        &keyword,
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
        if !keyword.is_empty() && !key.contains(keyword.as_str()) {
            continue;
        }
        let ttl_for_leaf = key_ttls.get(&key).copied();
        let (tag_for_leaf, note_for_leaf) = match metadata.get(key.as_ref()) {
            Some(m) => (m.tag, SharedString::from(m.note.clone())),
            None => (None, SharedString::default()),
        };
        if !key.contains(separator) {
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
        for (index, k) in key.splitn(max_key_tree_depth, separator).enumerate() {
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

    fn build_sorted_list(parent_id: &str, map: &mut AHashMap<String, Vec<KeyTreeItem>>, result: &mut Vec<KeyTreeItem>) {
        if let Some(mut children) = map.remove(parent_id) {
            children.sort_unstable_by(|a, b| b.is_folder.cmp(&a.is_folder).then_with(|| a.label.cmp(&b.label)));

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
                build_sorted_list(child_id.as_ref(), map, result);
            }
        }
    }

    build_sorted_list("", &mut children_map, &mut result);

    result
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
        let prefix = SharedString::from(format!("{}:", item.id));
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
        let out = append_load_more_rows(items, &incomplete, &label);
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
