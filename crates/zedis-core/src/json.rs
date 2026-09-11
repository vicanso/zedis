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

//! JSON as the value editor edits it: a syntax check that names a
//! position, the pretty / compact renderings, the layout a value keeps when
//! it is written back, the tree the JSON tree view shows, and the
//! path-level operations that tree runs.
//!
//! The operations mirror the `JSON.*` commands (`JSON.SET`, `JSON.DEL`,
//! `JSON.NUMINCRBY`, `JSON.TOGGLE`, `JSON.ARRAPPEND`, `JSON.STRAPPEND`,
//! `JSON.CLEAR`) on purpose: a RedisJSON key sends them to the server, a
//! plain string holding JSON applies them here to the parsed document and
//! saves the whole value — the same menu either way.

use serde::de::IgnoredAny;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::fmt;

/// Where a JSON parse failed, so the message can point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSyntaxError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for JsonSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

impl From<serde_json::Error> for JsonSyntaxError {
    fn from(error: serde_json::Error) -> Self {
        // serde_json appends " at line L column C" to its own message; the
        // position is carried separately here so the UI can lay it out.
        let message = error.to_string();
        let message = message
            .rfind(" at line ")
            .map(|at| message[..at].to_string())
            .unwrap_or(message);
        Self {
            line: error.line(),
            column: error.column(),
            message,
        }
    }
}

/// Whether `text` is well-formed JSON. Validates without building a DOM.
pub fn check_json(text: &str) -> Result<(), JsonSyntaxError> {
    serde_json::from_str::<IgnoredAny>(text)?;
    Ok(())
}

/// `text` re-rendered with indentation.
pub fn format_json(text: &str) -> Result<String, JsonSyntaxError> {
    let value: Value = serde_json::from_str(text)?;
    Ok(serde_json::to_string_pretty(&value)?)
}

/// `text` re-rendered on one line, without any insignificant whitespace.
pub fn minify_json(text: &str) -> Result<String, JsonSyntaxError> {
    let value: Value = serde_json::from_str(text)?;
    Ok(serde_json::to_string(&value)?)
}

/// The text to write back for `edited` so the value keeps the layout it
/// was stored with. The editor shows every JSON value indented; a value
/// that was stored on one line goes back compacted, so editing one field
/// does not turn a compact document into an indented one. Anything else —
/// a value stored indented, or an edit that is no longer JSON and that the
/// caller chose to save anyway — is written exactly as edited.
pub fn keep_stored_layout<'a>(stored: &str, edited: &'a str) -> Cow<'a, str> {
    if stored.trim().contains('\n') {
        return Cow::Borrowed(edited);
    }
    match minify_json(edited) {
        Ok(compact) => Cow::Owned(compact),
        Err(_) => Cow::Borrowed(edited),
    }
}

/// One step of a path into a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// `segments` as a JSONPath: `$`, `$.name`, `$['odd key']`, `$[0]`. A key
/// that is not a plain identifier goes in brackets so RedisJSON reads it
/// back the same way.
pub fn render_path(segments: &[PathSegment]) -> String {
    let mut path = String::from("$");
    for segment in segments {
        match segment {
            PathSegment::Key(key) if is_identifier(key) => {
                path.push('.');
                path.push_str(key);
            }
            PathSegment::Key(key) => {
                path.push_str("['");
                for c in key.chars() {
                    match c {
                        '\\' => path.push_str("\\\\"),
                        '\'' => path.push_str("\\'"),
                        c => path.push(c),
                    }
                }
                path.push_str("']");
            }
            PathSegment::Index(index) => {
                path.push('[');
                path.push_str(&index.to_string());
                path.push(']');
            }
        }
    }
    path
}

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The segments of a path in the subset this module renders — `$` or `.`
/// prefixed dotted names, bracketed quoted names (either quote), bracketed
/// indexes — or `None` for anything else (wildcards, filters, slices, a
/// negative index): those select several values and cannot address one.
pub fn parse_path(path: &str) -> Option<Vec<PathSegment>> {
    let mut rest = path.trim();
    rest = rest.strip_prefix('$').unwrap_or(rest);
    let mut segments = Vec::new();
    while !rest.is_empty() {
        if let Some(after_dot) = rest.strip_prefix('.') {
            let end = after_dot.find(['.', '[']).unwrap_or(after_dot.len());
            let name = &after_dot[..end];
            if name.is_empty() || name == "*" || name.starts_with('.') {
                return None;
            }
            segments.push(PathSegment::Key(name.to_string()));
            rest = &after_dot[end..];
        } else {
            let after_bracket = rest.strip_prefix('[')?;
            let (segment, consumed) = parse_bracket(after_bracket)?;
            segments.push(segment);
            rest = &after_bracket[consumed..];
        }
    }
    Some(segments)
}

/// One `[...]` segment; `text` starts just after the `[`. Answers the
/// segment and how many bytes of `text` it used, closing bracket included.
fn parse_bracket(text: &str) -> Option<(PathSegment, usize)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if first == '\'' || first == '"' {
        let mut name = String::new();
        let mut escaped = false;
        for (at, c) in chars {
            match (escaped, c) {
                (true, c) => {
                    name.push(c);
                    escaped = false;
                }
                (false, '\\') => escaped = true,
                (false, c) if c == first => {
                    let after_quote = &text[at + c.len_utf8()..];
                    let close = after_quote.strip_prefix(']')?;
                    let consumed = text.len() - close.len();
                    return Some((PathSegment::Key(name), consumed));
                }
                (false, c) => name.push(c),
            }
        }
        return None;
    }
    let close = text.find(']')?;
    let index: usize = text[..close].trim().parse().ok()?;
    Some((PathSegment::Index(index), close + 1))
}

fn resolve_mut<'a>(doc: &'a mut Value, segments: &[PathSegment]) -> Option<&'a mut Value> {
    let mut current = doc;
    for segment in segments {
        current = match segment {
            PathSegment::Key(key) => current.as_object_mut()?.get_mut(key)?,
            PathSegment::Index(index) => current.as_array_mut()?.get_mut(*index)?,
        };
    }
    Some(current)
}

fn resolve<'a>(doc: &'a Value, segments: &[PathSegment]) -> Option<&'a Value> {
    let mut current = doc;
    for segment in segments {
        current = match segment {
            PathSegment::Key(key) => current.as_object()?.get(key)?,
            PathSegment::Index(index) => current.as_array()?.get(*index)?,
        };
    }
    Some(current)
}

/// The value at `path` in `doc`, when the path is one this module
/// understands and something is there.
pub fn value_at<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    resolve(doc, &parse_path(path)?)
}

/// A path-level operation, one per `JSON.*` write the tree offers.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonPathOp {
    /// `JSON.SET` — replace the value at the path, or add it as a new
    /// member of an existing object.
    Set(Value),
    /// `JSON.DEL`.
    Del,
    /// `JSON.NUMINCRBY`.
    NumIncrBy(f64),
    /// `JSON.TOGGLE` — flip a boolean.
    Toggle,
    /// `JSON.ARRAPPEND` — one value onto the end of an array.
    ArrAppend(Value),
    /// `JSON.STRAPPEND`.
    StrAppend(String),
    /// `JSON.CLEAR` — empty a container, zero a number.
    Clear,
}

impl JsonPathOp {
    /// The command's name, for a menu or a log line.
    pub fn command(&self) -> &'static str {
        match self {
            JsonPathOp::Set(_) => "JSON.SET",
            JsonPathOp::Del => "JSON.DEL",
            JsonPathOp::NumIncrBy(_) => "JSON.NUMINCRBY",
            JsonPathOp::Toggle => "JSON.TOGGLE",
            JsonPathOp::ArrAppend(_) => "JSON.ARRAPPEND",
            JsonPathOp::StrAppend(_) => "JSON.STRAPPEND",
            JsonPathOp::Clear => "JSON.CLEAR",
        }
    }

    /// Whether the operation throws data away.
    pub fn is_destructive(&self) -> bool {
        matches!(self, JsonPathOp::Del | JsonPathOp::Clear)
    }
}

/// Why an operation could not be applied to a local document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonOpError {
    /// The path selects several values, or uses syntax this module does not
    /// evaluate.
    PathUnsupported,
    /// Nothing at the path.
    NotFound,
    /// The value at the path is not what the operation works on.
    WrongType(JsonNodeKind),
    /// Deleting the root leaves no document.
    RootDelete,
}

impl fmt::Display for JsonOpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonOpError::PathUnsupported => write!(f, "the path must select a single value"),
            JsonOpError::NotFound => write!(f, "nothing at that path"),
            JsonOpError::WrongType(expected) => write!(f, "the value at that path is not {}", expected.name()),
            JsonOpError::RootDelete => write!(f, "the root cannot be deleted"),
        }
    }
}

/// What an operation left, in the shape the server would have answered.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonOpOutcome {
    /// The number after `NumIncrBy`.
    Number(String),
    /// The boolean after `Toggle`.
    Bool(bool),
    /// Values deleted, the new length, or containers cleared.
    Count(u64),
    Done,
}

/// Apply `op` at `path` in `doc`, as the corresponding `JSON.*` command
/// would on the server.
pub fn apply_json_op(doc: &mut Value, path: &str, op: JsonPathOp) -> Result<JsonOpOutcome, JsonOpError> {
    let segments = parse_path(path).ok_or(JsonOpError::PathUnsupported)?;
    match op {
        JsonPathOp::Set(value) => {
            if let Some(target) = resolve_mut(doc, &segments) {
                *target = value;
                return Ok(JsonOpOutcome::Done);
            }
            // A missing last key on an existing object is an insert — how
            // JSON.SET creates a member.
            let Some((PathSegment::Key(key), parents)) = segments.split_last() else {
                return Err(JsonOpError::NotFound);
            };
            let parent = resolve_mut(doc, parents).ok_or(JsonOpError::NotFound)?;
            let object = parent.as_object_mut().ok_or(JsonOpError::NotFound)?;
            object.insert(key.clone(), value);
            Ok(JsonOpOutcome::Done)
        }
        JsonPathOp::Del => {
            let Some((last, parents)) = segments.split_last() else {
                return Err(JsonOpError::RootDelete);
            };
            let parent = resolve_mut(doc, parents).ok_or(JsonOpError::NotFound)?;
            let removed = match (last, parent) {
                (PathSegment::Key(key), Value::Object(map)) => map.remove(key).is_some(),
                (PathSegment::Index(index), Value::Array(items)) if *index < items.len() => {
                    items.remove(*index);
                    true
                }
                _ => false,
            };
            Ok(JsonOpOutcome::Count(u64::from(removed)))
        }
        JsonPathOp::NumIncrBy(delta) => {
            let target = resolve_mut(doc, &segments).ok_or(JsonOpError::NotFound)?;
            let current = target.as_f64().ok_or(JsonOpError::WrongType(JsonNodeKind::Number))?;
            let sum = current + delta;
            // An integer stays an integer, as the server keeps it.
            *target = if sum.fract() == 0.0 && sum.abs() < 9007199254740992.0 {
                Value::from(sum as i64)
            } else {
                serde_json::Number::from_f64(sum)
                    .map(Value::Number)
                    .ok_or(JsonOpError::WrongType(JsonNodeKind::Number))?
            };
            Ok(JsonOpOutcome::Number(target.to_string()))
        }
        JsonPathOp::Toggle => {
            let target = resolve_mut(doc, &segments).ok_or(JsonOpError::NotFound)?;
            let flipped = !target.as_bool().ok_or(JsonOpError::WrongType(JsonNodeKind::Bool))?;
            *target = Value::Bool(flipped);
            Ok(JsonOpOutcome::Bool(flipped))
        }
        JsonPathOp::ArrAppend(value) => {
            let target = resolve_mut(doc, &segments).ok_or(JsonOpError::NotFound)?;
            let items = target
                .as_array_mut()
                .ok_or(JsonOpError::WrongType(JsonNodeKind::Array))?;
            items.push(value);
            Ok(JsonOpOutcome::Count(items.len() as u64))
        }
        JsonPathOp::StrAppend(text) => {
            let target = resolve_mut(doc, &segments).ok_or(JsonOpError::NotFound)?;
            let Value::String(current) = target else {
                return Err(JsonOpError::WrongType(JsonNodeKind::String));
            };
            current.push_str(&text);
            Ok(JsonOpOutcome::Count(current.chars().count() as u64))
        }
        JsonPathOp::Clear => {
            let target = resolve_mut(doc, &segments).ok_or(JsonOpError::NotFound)?;
            let cleared = match target {
                Value::Object(map) => {
                    map.clear();
                    true
                }
                Value::Array(items) => {
                    items.clear();
                    true
                }
                Value::Number(_) => {
                    *target = Value::from(0);
                    true
                }
                _ => false,
            };
            Ok(JsonOpOutcome::Count(u64::from(cleared)))
        }
    }
}

/// The type of a value, as the tree labels it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonNodeKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl JsonNodeKind {
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Object(_) => JsonNodeKind::Object,
            Value::Array(_) => JsonNodeKind::Array,
            Value::String(_) => JsonNodeKind::String,
            Value::Number(_) => JsonNodeKind::Number,
            Value::Bool(_) => JsonNodeKind::Bool,
            Value::Null => JsonNodeKind::Null,
        }
    }

    pub fn is_container(self) -> bool {
        matches!(self, JsonNodeKind::Object | JsonNodeKind::Array)
    }

    /// The type's name as JSON calls it.
    pub fn name(self) -> &'static str {
        match self {
            JsonNodeKind::Object => "object",
            JsonNodeKind::Array => "array",
            JsonNodeKind::String => "string",
            JsonNodeKind::Number => "number",
            JsonNodeKind::Bool => "boolean",
            JsonNodeKind::Null => "null",
        }
    }
}

/// One row of the JSON tree.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonTreeNode {
    /// The node's JSONPath — its identity, and what the operations target.
    pub path: String,
    /// The member name, `[index]`, or `$` for the root.
    pub name: String,
    pub kind: JsonNodeKind,
    /// A scalar rendered for display (strings quoted and clipped);
    /// empty for a container.
    pub preview: String,
    /// Members of a container, `0` for a scalar.
    pub len: usize,
    pub children: Vec<JsonTreeNode>,
    /// Members past [`JSON_TREE_MAX_CHILDREN`] that got no node.
    pub omitted: usize,
}

/// A container shows at most this many members: a 100k-element array is
/// a JSONPath query, not a scroll.
pub const JSON_TREE_MAX_CHILDREN: usize = 500;

/// Characters of a string a tree row shows before clipping.
pub const JSON_TREE_PREVIEW_CHARS: usize = 80;

/// The tree of `doc`, rooted at `$`.
pub fn json_tree(doc: &Value) -> JsonTreeNode {
    let mut segments = Vec::new();
    build_node(doc, "$".to_string(), &mut segments)
}

fn build_node(value: &Value, name: String, segments: &mut Vec<PathSegment>) -> JsonTreeNode {
    let kind = JsonNodeKind::of(value);
    let path = render_path(segments);
    let (len, children, omitted) = match value {
        Value::Object(map) => {
            let mut children = Vec::with_capacity(map.len().min(JSON_TREE_MAX_CHILDREN));
            for (key, child) in map.iter().take(JSON_TREE_MAX_CHILDREN) {
                segments.push(PathSegment::Key(key.clone()));
                children.push(build_node(child, key.clone(), segments));
                segments.pop();
            }
            (map.len(), children, map.len().saturating_sub(JSON_TREE_MAX_CHILDREN))
        }
        Value::Array(items) => {
            let mut children = Vec::with_capacity(items.len().min(JSON_TREE_MAX_CHILDREN));
            for (index, child) in items.iter().enumerate().take(JSON_TREE_MAX_CHILDREN) {
                segments.push(PathSegment::Index(index));
                children.push(build_node(child, format!("[{index}]"), segments));
                segments.pop();
            }
            (
                items.len(),
                children,
                items.len().saturating_sub(JSON_TREE_MAX_CHILDREN),
            )
        }
        _ => (0, Vec::new(), 0),
    };
    JsonTreeNode {
        path,
        name,
        kind,
        preview: preview(value),
        len,
        children,
        omitted,
    }
}

fn preview(value: &Value) -> String {
    match value {
        Value::Object(_) | Value::Array(_) => String::new(),
        Value::String(text) => {
            let clipped: String = text.chars().take(JSON_TREE_PREVIEW_CHARS).collect();
            let mut shown = serde_json::to_string(&clipped).unwrap_or_default();
            if clipped.len() < text.len() {
                shown.pop();
                shown.push('…');
                shown.push('"');
            }
            shown
        }
        other => other.to_string(),
    }
}

/// An empty object, the document a new JSON key starts as.
pub fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_syntax_error_names_its_position_without_repeating_it() {
        let error = check_json("{\n  \"a\": 1,\n}").expect_err("trailing comma");
        assert_eq!(error.line, 3);
        assert_eq!(error.column, 1);
        assert!(!error.message.contains("at line"), "{}", error.message);
        assert_eq!(error.to_string(), format!("line 3, column 1: {}", error.message));
        assert!(check_json("[1, 2]").is_ok());
    }

    #[test]
    fn format_and_minify_round_trip() {
        let compact = r#"{"a":[1,2],"b":"x"}"#;
        let pretty = format_json(compact).expect("pretty");
        assert!(pretty.contains('\n'));
        assert_eq!(minify_json(&pretty).expect("compact"), compact);
        assert!(format_json("{").is_err());
    }

    #[test]
    fn a_value_stored_on_one_line_goes_back_on_one_line() {
        let edited = "{\n  \"a\": 2\n}";
        assert_eq!(keep_stored_layout(r#"{"a":1}"#, edited), r#"{"a":2}"#);
        // Stored indented: written as edited.
        assert_eq!(keep_stored_layout("{\n  \"a\": 1\n}", edited), edited);
        // No longer JSON: the caller chose to save it, so it goes as is.
        assert_eq!(keep_stored_layout(r#"{"a":1}"#, "not json"), "not json");
    }

    #[test]
    fn paths_render_and_parse_both_ways() {
        let segments = vec![
            PathSegment::Key("user".into()),
            PathSegment::Key("first name".into()),
            PathSegment::Index(3),
            PathSegment::Key("it's".into()),
        ];
        let rendered = render_path(&segments);
        assert_eq!(rendered, "$.user['first name'][3]['it\\'s']");
        assert_eq!(parse_path(&rendered), Some(segments));
        assert_eq!(parse_path("$"), Some(vec![]));
        assert_eq!(
            parse_path(".a.b"),
            Some(vec![PathSegment::Key("a".into()), PathSegment::Key("b".into())])
        );
        assert_eq!(parse_path("$[\"x\"]"), Some(vec![PathSegment::Key("x".into())]));
        // Several values, or syntax not evaluated here.
        assert_eq!(parse_path("$.items[*]"), None);
        assert_eq!(parse_path("$..name"), None);
        assert_eq!(parse_path("$[-1]"), None);
        assert_eq!(parse_path("$[?(@.a)]"), None);
    }

    #[test]
    fn set_replaces_or_inserts_a_member() {
        let mut doc = json!({"a": {"b": 1}, "list": [1]});
        assert_eq!(
            apply_json_op(&mut doc, "$.a.b", JsonPathOp::Set(json!("x"))),
            Ok(JsonOpOutcome::Done)
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.a.c", JsonPathOp::Set(json!(true))),
            Ok(JsonOpOutcome::Done),
            "a new key on an existing object"
        );
        assert_eq!(doc, json!({"a": {"b": "x", "c": true}, "list": [1]}));
        assert_eq!(
            apply_json_op(&mut doc, "$.missing.c", JsonPathOp::Set(json!(1))),
            Err(JsonOpError::NotFound),
            "no parent to insert into"
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.list[5]", JsonPathOp::Set(json!(1))),
            Err(JsonOpError::NotFound),
            "an index past the end is not an append"
        );
    }

    #[test]
    fn del_removes_a_member_or_an_element_but_never_the_root() {
        let mut doc = json!({"a": 1, "list": [1, 2, 3]});
        assert_eq!(
            apply_json_op(&mut doc, "$.a", JsonPathOp::Del),
            Ok(JsonOpOutcome::Count(1))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.list[1]", JsonPathOp::Del),
            Ok(JsonOpOutcome::Count(1))
        );
        assert_eq!(doc, json!({"list": [1, 3]}));
        assert_eq!(
            apply_json_op(&mut doc, "$.gone", JsonPathOp::Del),
            Ok(JsonOpOutcome::Count(0))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$", JsonPathOp::Del),
            Err(JsonOpError::RootDelete)
        );
    }

    #[test]
    fn the_scalar_operations_check_the_type_they_need() {
        let mut doc = json!({"n": 5, "f": 1.5, "b": false, "s": "ab", "arr": [1], "o": {"k": 1}});
        assert_eq!(
            apply_json_op(&mut doc, "$.n", JsonPathOp::NumIncrBy(2.0)),
            Ok(JsonOpOutcome::Number("7".into())),
            "an integer stays an integer"
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.f", JsonPathOp::NumIncrBy(1.0)),
            Ok(JsonOpOutcome::Number("2.5".into()))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.s", JsonPathOp::NumIncrBy(1.0)),
            Err(JsonOpError::WrongType(JsonNodeKind::Number))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.b", JsonPathOp::Toggle),
            Ok(JsonOpOutcome::Bool(true))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.n", JsonPathOp::Toggle),
            Err(JsonOpError::WrongType(JsonNodeKind::Bool))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.arr", JsonPathOp::ArrAppend(json!("x"))),
            Ok(JsonOpOutcome::Count(2))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.s", JsonPathOp::StrAppend("cd".into())),
            Ok(JsonOpOutcome::Count(4))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.o", JsonPathOp::Clear),
            Ok(JsonOpOutcome::Count(1))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.n", JsonPathOp::Clear),
            Ok(JsonOpOutcome::Count(1))
        );
        assert_eq!(
            apply_json_op(&mut doc, "$.s", JsonPathOp::Clear),
            Ok(JsonOpOutcome::Count(0)),
            "JSON.CLEAR leaves a string alone"
        );
        assert_eq!(doc["n"], json!(0));
        assert_eq!(doc["o"], json!({}));
        assert_eq!(doc["s"], json!("abcd"));
        assert_eq!(
            apply_json_op(&mut doc, "$.arr[*]", JsonPathOp::Clear),
            Err(JsonOpError::PathUnsupported)
        );
    }

    #[test]
    fn the_tree_names_paths_and_clips_wide_containers() {
        let doc = json!({"user": {"name": "Ann", "tags": ["a", "b"]}, "n": 1, "ok": true, "none": null});
        let tree = json_tree(&doc);
        assert_eq!(tree.path, "$");
        assert_eq!(tree.kind, JsonNodeKind::Object);
        assert_eq!(tree.len, 4);
        let user = &tree.children[0];
        assert_eq!((user.name.as_str(), user.path.as_str()), ("user", "$.user"));
        let tags = &user.children[1];
        assert_eq!(tags.path, "$.user.tags");
        assert_eq!(tags.children[1].path, "$.user.tags[1]");
        assert_eq!(tags.children[1].name, "[1]");
        assert_eq!(tags.children[1].preview, "\"b\"");
        assert_eq!(tree.children[1].preview, "1");
        assert_eq!(tree.children[2].preview, "true");
        assert_eq!(tree.children[3].preview, "null");
        assert_eq!(user.children[0].preview, "\"Ann\"");

        let wide = Value::Array((0..JSON_TREE_MAX_CHILDREN + 7).map(Value::from).collect());
        let tree = json_tree(&wide);
        assert_eq!(tree.children.len(), JSON_TREE_MAX_CHILDREN);
        assert_eq!(tree.omitted, 7);
        assert_eq!(tree.len, JSON_TREE_MAX_CHILDREN + 7);
    }

    #[test]
    fn a_long_string_preview_is_clipped_inside_its_quotes() {
        let long = "x".repeat(JSON_TREE_PREVIEW_CHARS + 10);
        let tree = json_tree(&json!({"s": long}));
        let preview = &tree.children[0].preview;
        assert!(preview.starts_with('"'));
        assert!(preview.ends_with("…\""));
        assert_eq!(preview.chars().count(), JSON_TREE_PREVIEW_CHARS + 3);
    }

    #[test]
    fn value_at_reads_through_a_rendered_path() {
        let doc = json!({"a b": [{"c": 3}]});
        assert_eq!(value_at(&doc, "$['a b'][0].c"), Some(&json!(3)));
        assert_eq!(value_at(&doc, "$.missing"), None);
        assert_eq!(value_at(&doc, "$[*]"), None);
    }
}
