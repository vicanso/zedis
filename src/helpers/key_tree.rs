// Copyright 2025 Tree xie.
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

use ahash::AHashMap;
use gpui_component::tree::TreeItem;

// KeyNode is a node in the key tree.
#[derive(Debug, Default)]
struct KeyNode {
    /// full path (e.g. "dir1:dir2")
    full_path: String,

    /// is this node a real key?
    is_key: bool,

    /// children nodes (key is short name, e.g. "dir2")
    children: AHashMap<String, KeyNode>,
}

impl KeyNode {
    /// create a new child node
    fn new(full_path: String) -> Self {
        Self {
            full_path,
            is_key: false,
            children: AHashMap::new(),
        }
    }

    /// recursively insert a key (by parts) into this node.
    /// 'self' is the parent node (e.g. "dir1")
    /// 'mut parts' is the remaining parts (e.g. ["dir2", "name"])
    fn insert(&mut self, mut parts: std::str::Split<'_, &str>) {
        let Some(part_name) = parts.next() else {
            self.is_key = true;
            return;
        };

        let child_full_path = if self.full_path.is_empty() {
            part_name.to_string()
        } else {
            format!("{}:{}", self.full_path, part_name)
        };

        let child_node = self
            .children
            .entry(part_name.to_string()) // Key in map is short name
            .or_insert_with(|| KeyNode::new(child_full_path));

        child_node.insert(parts);
    }
}

pub fn build_key_tree(keys: &Vec<String>) -> Vec<TreeItem> {
    let mut root_trie_node = KeyNode {
        full_path: "".to_string(),
        is_key: false,
        children: AHashMap::new(),
    };

    for key in keys {
        root_trie_node.insert(key.split(":"));
    }

    fn convert_map_to_vec_tree(children_map: &AHashMap<String, KeyNode>) -> Vec<TreeItem> {
        let mut children_vec = Vec::new();

        for (short_name, internal_node) in children_map {
            let node = TreeItem::new(internal_node.full_path.clone(), short_name.clone());
            let node = node.children(convert_map_to_vec_tree(&internal_node.children));
            children_vec.push(node);
        }

        children_vec.sort_unstable_by(|a, b| {
            let a_is_dir = !a.children.is_empty();
            let b_is_dir = !b.children.is_empty();

            let type_ordering = a_is_dir.cmp(&b_is_dir).reverse();

            type_ordering.then_with(|| a.id.cmp(&b.id))
        });

        children_vec
    }

    convert_map_to_vec_tree(&root_trie_node.children)
}
