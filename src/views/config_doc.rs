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

//! Per-parameter help text for the CONFIG editor.
//!
//! Descriptions live in embedded JSON (`assets/config_docs/{en,zh}.json`),
//! compressed by `rust-embed`'s `compression` feature. Nothing is loaded at
//! app startup. The CONFIG editor view loads the matching language once when
//! opened (and again only if the UI locale changes while the view is live).

use crate::assets::Assets;
use std::collections::HashMap;
use tracing::info;

/// Official redis.conf help for one UI language, keyed by CONFIG parameter name.
pub(crate) type ConfigDocMap = HashMap<String, String>;

/// Load and parse one language's config docs from the embedded asset.
///
/// Inflates only `config_docs/en.json` or `config_docs/zh.json`. No process-
/// global cache — the CONFIG editor stores the result on the view entity.
pub(crate) fn load_config_docs(zh: bool) -> ConfigDocMap {
    let path = if zh {
        "config_docs/zh.json"
    } else {
        "config_docs/en.json"
    };
    info!(path, zh, "loading config_docs");
    let Some(file) = Assets::get(path) else {
        tracing::warn!(path, "config_docs asset missing");
        return ConfigDocMap::new();
    };
    match serde_json::from_slice::<ConfigDocMap>(file.data.as_ref()) {
        Ok(map) => {
            info!(path, entries = map.len(), "loaded config_docs");
            map
        }
        Err(err) => {
            tracing::warn!(path, error = %err, "failed to parse config_docs JSON");
            ConfigDocMap::new()
        }
    }
}

/// Look up one key by loading the language file (no caching). Prefer
/// [`load_config_docs`] when many keys are needed in the same paint.
#[cfg(test)]
pub(crate) fn config_doc(key: &str, zh: bool) -> Option<String> {
    load_config_docs(zh).remove(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_en_maxmemory() {
        let doc = config_doc("maxmemory", false).expect("maxmemory en");
        assert!(doc.contains("memory usage limit"));
    }

    #[test]
    fn loads_zh_maxmemory() {
        let doc = config_doc("maxmemory", true).expect("maxmemory zh");
        assert!(doc.contains("内存"));
    }

    #[test]
    fn unknown_key_is_none() {
        assert!(config_doc("__no_such_config_key__", false).is_none());
    }
}
