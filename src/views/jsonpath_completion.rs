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

//! Context-aware JSONPath autocomplete for the value editor's query
//! bar (Tier 2): typing `$.user.` suggests the actual document's keys.
//!
//! The parsing/resolution logic is pure and unit-tested in
//! `helpers::jsonpath`; this file is only the thin
//! `gpui_component::input::CompletionProvider` adapter plus a lazily
//! parsed, cached document handle shared with the editor.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui::{Context, SharedString, Task, Window};
use gpui_component::input::{CompletionProvider, InputState, Rope, RopeExt};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, InsertReplaceEdit,
    Range as LspRange,
};
use serde_json::Value;

use crate::helpers::{jsonpath_completion_prefix, jsonpath_key_suggestions};

/// Shared, lazily-parsed view of the value currently in the editor.
///
/// The editor only sets the raw text (cheap); the JSON DOM is built
/// once, on the first completion request, and cached — so a large
/// value is never DOM-parsed just to power a suggestion the user
/// might not ask for, and never twice.
#[derive(Default)]
pub(crate) struct JsonDoc {
    raw: Option<SharedString>,
    parsed: Option<Rc<Value>>,
    /// Set once we've attempted to parse `raw`, so a non-JSON value
    /// isn't re-parsed on every keystroke.
    tried: bool,
}

impl JsonDoc {
    /// Point the cache at a new raw value (or `None` to disable
    /// completion). Resets the cached DOM only when the text changed.
    pub(crate) fn set_raw(&mut self, raw: Option<SharedString>) {
        if self.raw.as_deref() != raw.as_deref() {
            self.raw = raw;
            self.parsed = None;
            self.tried = false;
        }
    }

    /// Lazily parse + cache the document. Returns `None` when there is
    /// no value or it isn't valid JSON.
    fn value(&mut self) -> Option<Rc<Value>> {
        if self.parsed.is_none() && !self.tried {
            self.tried = true;
            if let Some(raw) = &self.raw
                && let Ok(v) = serde_json::from_str::<Value>(raw)
            {
                self.parsed = Some(Rc::new(v));
            }
        }
        self.parsed.clone()
    }
}

/// `CompletionProvider` that suggests object keys based on the partial
/// JSONPath under the cursor and the cached JSON document.
pub(crate) struct JsonPathCompletionProvider {
    doc: Rc<RefCell<JsonDoc>>,
}

impl JsonPathCompletionProvider {
    pub(crate) fn new(doc: Rc<RefCell<JsonDoc>>) -> Self {
        Self { doc }
    }
}

impl CompletionProvider for JsonPathCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let empty = || Task::ready(Ok(CompletionResponse::Array(vec![])));

        let text: String = rope.slice(..).into();
        let Some(prefix) = jsonpath_completion_prefix(&text, offset) else {
            return empty();
        };
        let Some(value) = self.doc.borrow_mut().value() else {
            return empty();
        };
        let keys = jsonpath_key_suggestions(&value, &prefix);
        if keys.is_empty() {
            return empty();
        }

        // Replace exactly the partial token under the cursor with the
        // chosen key, so accepting a suggestion never duplicates what
        // was already typed.
        let range = LspRange::new(
            rope.offset_to_position(prefix.replace_start),
            rope.offset_to_position(offset),
        );
        let items = keys
            .into_iter()
            .map(|k| CompletionItem {
                label: k.clone(),
                kind: Some(CompletionItemKind::FIELD),
                text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                    new_text: k,
                    insert: range,
                    replace: range,
                })),
                insert_text: None,
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, _new_text: &str, _cx: &mut Context<InputState>) -> bool {
        // Re-query on every edit; `completions` returns an empty set
        // (which hides the menu) whenever the cursor isn't on a
        // navigable JSONPath prefix, so this stays correct and cheap
        // (small query text + a cached DOM).
        true
    }
}
