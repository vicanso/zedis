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

//! Conservative YAML detection.
//!
//! Because almost any plain text is technically a YAML scalar, detection
//! requires multi-line structure (or an explicit `---` document marker) that
//! parses into at least one YAML mapping or sequence.

use yaml_rust2::{Yaml, YamlLoader};

/// Returns `Some(text)` when `text` is unmistakably a YAML mapping or
/// sequence document, or `None` for plain text / single-line strings / JSON.
pub fn decode(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('<') {
        return None;
    }

    let has_doc_marker = trimmed.starts_with("---\n") || trimmed.starts_with("---\r\n");
    if !has_doc_marker && !trimmed.contains('\n') {
        return None;
    }

    let docs = YamlLoader::load_from_str(trimmed).ok()?;
    if docs.is_empty() {
        return None;
    }

    let is_structured = docs.iter().all(|doc| match doc {
        Yaml::Hash(map) => {
            !map.is_empty()
                && (has_doc_marker
                    || map.len() >= 2
                    || map.values().any(|v| matches!(v, Yaml::Hash(_) | Yaml::Array(_))))
        }
        Yaml::Array(items) => !items.is_empty(),
        _ => false,
    });

    is_structured.then_some(text)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn detects_multi_line_yaml_and_document_markers() {
        let yaml = "server:\n  host: 127.0.0.1\n  port: 6379\nfeatures:\n  - resp3\n  - cluster\n";
        assert!(decode(yaml).is_some());

        let doc = "---\nname: zedis\n";
        assert!(decode(doc).is_some());

        let list = "- alpha\n- beta\n";
        assert!(decode(list).is_some());
    }

    #[test]
    fn rejects_plain_text_single_line_and_json() {
        assert!(decode("hello\nworld").is_none());
        assert!(decode("host: 127.0.0.1").is_none());
        assert!(decode(r#"{"a": 1}"#).is_none());
        assert!(decode("<root><a/></root>").is_none());
    }
}
