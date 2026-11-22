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

//! The JSON tree: one row per member of a document, with the `JSON.*`
//! operations on the row under the pointer.
//!
//! Two kinds of document look the same here and are edited differently. A
//! RedisJSON key sends each operation to the server as its command, and the
//! reloaded value rebuilds the tree. A plain string holding JSON has no such
//! commands: the operation is applied to the parsed document here, the
//! editor's text is replaced, and Save writes the whole value as usual.

use crate::connection::{KeyOp, ServerCommand, ServerFeatures};
use crate::helpers::{KeyOpAction, get_mono_font_family};
use crate::states::{ZedisGlobalStore, ZedisServerState, i18n_editor, i18n_key_ops};
use crate::views::{
    KeyOpPrefill, key_op_title_key, open_key_op_dialog_prefilled, open_key_op_form, run_key_op_confirmed,
};
use gpui::{
    App, ClipboardItem, Entity, EventEmitter, SharedString, Subscription, WeakEntity, Window, prelude::*, relative,
    rems,
};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, h_flex,
    label::Label,
    list::ListItem,
    menu::{PopupMenu, PopupMenuItem},
    tree::{Tree, TreeEntry, TreeEvent, TreeItem, TreeState},
    v_flex,
};
use rust_i18n::t;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use zedis_core::json::{JsonNodeKind, JsonOpError, JsonPathOp, JsonTreeNode, apply_json_op, json_tree, value_at};

/// Horizontal step per nesting level, in rems so indentation scales with UI font size.
const INDENT_REMS: f32 = 1.0;
/// Left inset of every row, chevron included, in rems.
const ROW_INSET_REMS: f32 = 0.5;
/// Relative line height so tree rows scale smoothly with `mono_font_size`
/// (from 10px up to 28px) without vertical clipping.
const JSON_TREE_LINE_HEIGHT: f32 = 1.4;
const JSON_TREE_NAME_ID_PREFIX: &str = "json-tree-name";
const JSON_TREE_COLON_ID_PREFIX: &str = "json-tree-colon";
const JSON_TREE_DETAIL_ID_PREFIX: &str = "json-tree-detail";
const JSON_TREE_INVALID_ID: &str = "json-tree-invalid";

/// Where the tree's operations go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTreeTarget {
    /// A RedisJSON key: each operation is its `JSON.*` command.
    Server,
    /// A plain string: operations edit the document here, Save writes it.
    Local,
}

pub enum JsonTreeEvent {
    /// A local operation changed the document; `text` is what the editor
    /// shows now.
    DocEdited(SharedString),
    /// Evaluate this path in the JSONPath bar.
    QueryPath(SharedString),
}

/// What a row shows.
struct Row {
    name: SharedString,
    /// `None` for the marker standing in for members past the cap.
    kind: Option<JsonNodeKind>,
    /// A scalar's preview, or a container's member count.
    detail: SharedString,
}

/// What the row menu offers.
#[derive(Debug, Clone, Copy)]
enum RowCommand {
    CopyPath,
    CopyValue,
    Query,
    Op(KeyOpAction),
    /// `JSON.SET` with the path left for the user to finish.
    AddMember,
}

pub struct ZedisJsonTree {
    server_state: Entity<ZedisServerState>,
    tree_state: Entity<TreeState>,
    doc: Option<Value>,
    rows: Rc<HashMap<SharedString, Row>>,
    target: JsonTreeTarget,
    editable: bool,
    /// Whether the row menu offers *Query* — only where a JSONPath bar
    /// exists to receive it.
    query: bool,
    /// Containers the user opened, kept across rebuilds so a reload after
    /// an operation does not fold the tree back up.
    expanded: HashSet<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<JsonTreeEvent> for ZedisJsonTree {}

impl ZedisJsonTree {
    pub fn new(server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let subscription = cx.subscribe(&tree_state, |this, _, event: &TreeEvent, _| match event {
            TreeEvent::Expanded(id) => {
                this.expanded.insert(id.clone());
            }
            TreeEvent::Collapsed(id) => {
                this.expanded.remove(id);
            }
        });
        Self {
            server_state,
            tree_state,
            doc: None,
            rows: Rc::new(HashMap::new()),
            target: JsonTreeTarget::Local,
            editable: false,
            query: true,
            expanded: HashSet::new(),
            _subscriptions: vec![subscription],
        }
    }

    /// This tree sits nowhere near a JSONPath bar: drop the *Query* item
    /// from the row menu, so it never offers a command with no destination.
    pub fn without_query(mut self) -> Self {
        self.query = false;
        self
    }

    /// Show `doc` (`None` when the text is not JSON), with the operations
    /// going to `target` and offered at all only when `editable`.
    pub fn set_document(&mut self, doc: Option<Value>, target: JsonTreeTarget, editable: bool, cx: &mut Context<Self>) {
        self.doc = doc;
        self.target = target;
        self.editable = editable;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut rows = HashMap::new();
        let items = match &self.doc {
            Some(doc) => vec![self.item_for(&json_tree(doc), true, &mut rows, &locale)],
            None => Vec::new(),
        };
        self.rows = Rc::new(rows);
        self.tree_state.update(cx, |state, cx| state.set_items(items, cx));
        cx.notify();
    }

    fn item_for(
        &self,
        node: &JsonTreeNode,
        is_root: bool,
        rows: &mut HashMap<SharedString, Row>,
        locale: &str,
    ) -> TreeItem {
        let id: SharedString = node.path.clone().into();
        let detail: SharedString = match node.kind {
            JsonNodeKind::Object => t!("editor.json_tree_members", count = node.len, locale = locale)
                .to_string()
                .into(),
            JsonNodeKind::Array => t!("editor.json_tree_items", count = node.len, locale = locale)
                .to_string()
                .into(),
            _ => node.preview.clone().into(),
        };
        rows.insert(
            id.clone(),
            Row {
                name: node.name.clone().into(),
                kind: Some(node.kind),
                detail,
            },
        );
        let mut item = TreeItem::new(id.clone(), node.name.clone()).expanded(is_root || self.expanded.contains(&id));
        for child in &node.children {
            item = item.child(self.item_for(child, false, rows, locale));
        }
        if node.omitted > 0 {
            let more_id: SharedString = format!("{}#more", node.path).into();
            let label: SharedString = t!("editor.json_tree_more", count = node.omitted, locale = locale)
                .to_string()
                .into();
            rows.insert(
                more_id.clone(),
                Row {
                    name: label.clone(),
                    kind: None,
                    detail: SharedString::default(),
                },
            );
            item = item.child(TreeItem::new(more_id, label).disabled(true));
        }
        item
    }

    fn perform(&mut self, command: RowCommand, path: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            RowCommand::CopyPath => cx.write_to_clipboard(ClipboardItem::new_string(path.to_string())),
            RowCommand::CopyValue => {
                let text = self
                    .doc
                    .as_ref()
                    .and_then(|doc| value_at(doc, &path))
                    .and_then(|value| serde_json::to_string_pretty(value).ok());
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            RowCommand::Query => cx.emit(JsonTreeEvent::QueryPath(path)),
            RowCommand::AddMember => {
                let prefill = KeyOpPrefill {
                    path: Some(format!("{path}.")),
                    value: None,
                };
                self.open_form(KeyOpAction::JsonSet, prefill, window, cx);
            }
            RowCommand::Op(action) => {
                // The three that take nothing but the path skip the form.
                let direct = match action {
                    KeyOpAction::JsonDel => Some(JsonPathOp::Del),
                    KeyOpAction::JsonToggle => Some(JsonPathOp::Toggle),
                    KeyOpAction::JsonClear => Some(JsonPathOp::Clear),
                    _ => None,
                };
                match direct {
                    Some(op) => self.run(
                        KeyOp::Json {
                            path: path.to_string(),
                            op,
                        },
                        key_op_title_key(action),
                        window,
                        cx,
                    ),
                    None => {
                        let current = self
                            .doc
                            .as_ref()
                            .and_then(|doc| value_at(doc, &path))
                            .map(|value| value.to_string());
                        let prefill = KeyOpPrefill {
                            path: Some(path.to_string()),
                            // JSON.SET starts from what is there; the appends
                            // want something new.
                            value: current.filter(|_| action == KeyOpAction::JsonSet),
                        };
                        self.open_form(action, prefill, window, cx);
                    }
                }
            }
        }
    }

    fn open_form(&mut self, action: KeyOpAction, prefill: KeyOpPrefill, window: &mut Window, cx: &mut Context<Self>) {
        match self.target {
            JsonTreeTarget::Server => {
                let Some(key) = self.server_state.read(cx).key() else {
                    return;
                };
                open_key_op_dialog_prefilled(self.server_state.clone(), key, action, prefill, window, cx);
            }
            JsonTreeTarget::Local => {
                let this = cx.entity().downgrade();
                open_key_op_form(
                    action,
                    prefill,
                    move |op, _window, cx| {
                        let _ = this.update(cx, |this, cx| this.apply_local(op, cx));
                    },
                    window,
                    cx,
                );
            }
        }
    }

    fn run(&mut self, op: KeyOp, title_key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        match self.target {
            JsonTreeTarget::Server => {
                let Some(key) = self.server_state.read(cx).key() else {
                    return;
                };
                run_key_op_confirmed(self.server_state.clone(), key, op, title_key, window, cx);
            }
            // Nothing is lost until Save, so no confirmation here.
            JsonTreeTarget::Local => self.apply_local(op, cx),
        }
    }

    /// Apply `op` to the local document and hand the editor the result.
    fn apply_local(&mut self, op: KeyOp, cx: &mut Context<Self>) {
        let KeyOp::Json { path, op } = op else {
            return;
        };
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        match apply_json_op(doc, &path, op) {
            Ok(_) => {
                let text: SharedString = serde_json::to_string_pretty(doc).unwrap_or_default().into();
                cx.emit(JsonTreeEvent::DocEdited(text));
                self.rebuild(cx);
            }
            Err(error) => {
                let message = local_op_error(&error, &path, cx);
                self.server_state
                    .update(cx, |state, cx| state.emit_error_notification(message, cx));
            }
        }
    }
}

/// What a row's menu is built from, captured once per render.
struct MenuScope {
    tree: WeakEntity<ZedisJsonTree>,
    rows: Rc<HashMap<SharedString, Row>>,
    features: Arc<ServerFeatures>,
    target: JsonTreeTarget,
    editable: bool,
    query: bool,
}

impl MenuScope {
    /// The menu for `entry`: copy and query for everyone, the operations
    /// that fit the node's type when the document is editable — each one
    /// only where the server has its command.
    fn menu(&self, entry: &TreeEntry, menu: PopupMenu, cx: &App) -> PopupMenu {
        let id = entry.item().id.clone();
        let Some(kind) = self.rows.get(&id).and_then(|row| row.kind) else {
            return menu;
        };
        let item = |command: RowCommand, label: SharedString, icon: Option<IconName>| {
            let tree = self.tree.clone();
            let id = id.clone();
            let mut item = PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = tree.update(cx, |tree, cx| tree.perform(command, id.clone(), window, cx));
            });
            if let Some(icon) = icon {
                item = item.icon(icon);
            }
            item
        };
        let mut menu = menu
            .item(item(
                RowCommand::CopyPath,
                i18n_editor(cx, "json_tree_copy_path"),
                Some(IconName::Copy),
            ))
            .item(item(
                RowCommand::CopyValue,
                i18n_editor(cx, "json_tree_copy_value"),
                None,
            ));
        if self.query {
            menu = menu.item(item(
                RowCommand::Query,
                i18n_editor(cx, "json_tree_query"),
                Some(IconName::Search),
            ));
        }
        if !self.editable {
            return menu;
        }
        // The operations a value of this type supports, with the command
        // each one needs on a server.
        let mut ops: Vec<(RowCommand, ServerCommand)> =
            vec![(RowCommand::Op(KeyOpAction::JsonSet), ServerCommand::JsonSet)];
        match kind {
            JsonNodeKind::Object => {
                ops.push((RowCommand::AddMember, ServerCommand::JsonSet));
                ops.push((RowCommand::Op(KeyOpAction::JsonClear), ServerCommand::JsonClear));
            }
            JsonNodeKind::Array => {
                ops.push((RowCommand::Op(KeyOpAction::JsonArrAppend), ServerCommand::JsonArrAppend));
                ops.push((RowCommand::Op(KeyOpAction::JsonClear), ServerCommand::JsonClear));
            }
            JsonNodeKind::Number => {
                ops.push((RowCommand::Op(KeyOpAction::JsonNumIncrBy), ServerCommand::JsonNumIncrBy));
                ops.push((RowCommand::Op(KeyOpAction::JsonClear), ServerCommand::JsonClear));
            }
            JsonNodeKind::Bool => ops.push((RowCommand::Op(KeyOpAction::JsonToggle), ServerCommand::JsonToggle)),
            JsonNodeKind::String => {
                ops.push((RowCommand::Op(KeyOpAction::JsonStrAppend), ServerCommand::JsonStrAppend))
            }
            JsonNodeKind::Null => {}
        }
        if !entry.is_root() {
            ops.push((RowCommand::Op(KeyOpAction::JsonDel), ServerCommand::JsonDel));
        }
        let ops: Vec<RowCommand> = ops
            .into_iter()
            .filter(|(_, command)| self.target == JsonTreeTarget::Local || self.features.is_usable(*command))
            .map(|(command, _)| command)
            .collect();
        if ops.is_empty() {
            return menu;
        }
        menu = menu.separator();
        for command in ops {
            let label = match command {
                RowCommand::AddMember => i18n_editor(cx, "json_tree_add_member"),
                RowCommand::Op(action) => i18n_key_ops(cx, key_op_title_key(action)),
                _ => continue,
            };
            menu = menu.item(item(command, label, None));
        }
        menu
    }
}

fn local_op_error(error: &JsonOpError, path: &str, cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let reason = match error {
        JsonOpError::PathUnsupported => t!("editor.json_op_path_unsupported", locale = locale).to_string(),
        JsonOpError::NotFound => t!("editor.json_op_not_found", locale = locale).to_string(),
        JsonOpError::WrongType(expected) => {
            let expected = i18n_editor(
                cx,
                match expected {
                    JsonNodeKind::Object => "json_kind_object",
                    JsonNodeKind::Array => "json_kind_array",
                    JsonNodeKind::String => "json_kind_string",
                    JsonNodeKind::Number => "json_kind_number",
                    JsonNodeKind::Bool => "json_kind_boolean",
                    JsonNodeKind::Null => "json_kind_null",
                },
            );
            t!("editor.json_op_wrong_type", expected = expected, locale = locale).to_string()
        }
        JsonOpError::RootDelete => t!("editor.json_op_root_delete", locale = locale).to_string(),
    };
    format!("{path}: {reason}").into()
}

/// Whether a tree row renders `:` between its name and its detail text.
///
/// Every scalar shows `name : value` (root scalars included). Non-root
/// containers also show `key : N members` / `key : N items` so the key and
/// summary stay clearly separated at every editor font size (`13px`–`28px`),
/// while the root container `$` skips `:` and uses a rem margin instead.
fn row_shows_colon(kind: Option<JsonNodeKind>, is_root: bool) -> bool {
    let Some(kind) = kind else {
        return false;
    };
    !is_root || !kind.is_container()
}

fn render_row(ix: usize, entry: &TreeEntry, rows: &HashMap<SharedString, Row>, cx: &App) -> ListItem {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let font_size = theme.mono_font_size;
    let row = rows.get(&entry.item().id);
    let name = row.map(|row| row.name.clone()).unwrap_or_default();
    let kind = row.and_then(|row| row.kind);
    let detail = row.map(|row| row.detail.clone()).unwrap_or_default();
    let detail_color = match kind {
        Some(JsonNodeKind::String) => theme.green,
        Some(JsonNodeKind::Number) => theme.blue,
        Some(JsonNodeKind::Bool) => theme.magenta,
        _ => muted,
    };
    let name_color = if kind.is_some() { theme.foreground } else { muted };
    let show_colon = row_shows_colon(kind, entry.is_root());
    let chevron = entry.is_folder().then(|| {
        if entry.is_expanded() {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        }
    });
    ListItem::new(ix).w_full().py_1().px_2().child(
        h_flex()
            .w_full()
            .gap_1()
            .items_center()
            .pl(rems(INDENT_REMS * entry.depth() as f32 + ROW_INSET_REMS))
            .font_family(get_mono_font_family())
            .child(
                h_flex()
                    .w_4()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .children(chevron.map(|icon| Icon::new(icon).xsmall().text_color(muted))),
            )
            .child(
                Label::new(name)
                    .id((JSON_TREE_NAME_ID_PREFIX, ix))
                    .text_size(font_size)
                    .line_height(relative(JSON_TREE_LINE_HEIGHT))
                    .text_color(name_color)
                    .whitespace_nowrap(),
            )
            .when(show_colon, |this| {
                this.child(
                    Label::new(":")
                        .id((JSON_TREE_COLON_ID_PREFIX, ix))
                        .text_size(font_size)
                        .line_height(relative(JSON_TREE_LINE_HEIGHT))
                        .text_color(muted),
                )
            })
            .when(!detail.is_empty(), |this| {
                this.child(
                    Label::new(detail)
                        .id((JSON_TREE_DETAIL_ID_PREFIX, ix))
                        .text_size(font_size)
                        .line_height(relative(JSON_TREE_LINE_HEIGHT))
                        .text_color(detail_color)
                        .when(!show_colon, |this| this.ml_2())
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .min_w_0()
                        .flex_1(),
                )
            }),
    )
}

impl Render for ZedisJsonTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.doc.is_none() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    Label::new(i18n_editor(cx, "json_tree_invalid"))
                        .id(JSON_TREE_INVALID_ID)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
                .into_any_element();
        }
        let rows = self.rows.clone();
        let scope = MenuScope {
            tree: cx.entity().downgrade(),
            rows: self.rows.clone(),
            features: self.server_state.read(cx).features(),
            target: self.target,
            editable: self.editable,
            query: self.query,
        };
        Tree::new(&self.tree_state, move |ix, entry, _selected, _window, cx| {
            render_row(ix, entry, &rows, cx)
        })
        .context_menu(move |_ix, entry, menu, _window, cx| scope.menu(entry, menu, cx))
        .size_full()
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::apply_editor_font_size;
    use crate::test_support::init_ui;
    use gpui::{TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use serde_json::json;

    #[test]
    fn row_shows_colon_for_scalars_and_non_root_containers() {
        assert!(!row_shows_colon(Some(JsonNodeKind::Object), true));
        assert!(!row_shows_colon(Some(JsonNodeKind::Array), true));
        assert!(row_shows_colon(Some(JsonNodeKind::String), true));
        assert!(row_shows_colon(Some(JsonNodeKind::Number), true));
        assert!(row_shows_colon(Some(JsonNodeKind::Object), false));
        assert!(row_shows_colon(Some(JsonNodeKind::Array), false));
        assert!(row_shows_colon(Some(JsonNodeKind::String), false));
        assert!(row_shows_colon(Some(JsonNodeKind::Bool), false));
        assert!(row_shows_colon(Some(JsonNodeKind::Null), false));
        assert!(!row_shows_colon(None, false));
    }

    #[gpui::test]
    fn collapsed_containers_separate_key_and_count_and_scale_with_font_size(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let doc = json!({
            "alpha": 2,
            "nested": { "z": 10, "a": 20, "m": 30 },
            "features": ["first", "second", "third"]
        });

        let mut view = None;
        let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
            let tree = cx.new(|cx| {
                let mut tree = ZedisJsonTree::new(server_state.clone(), cx);
                tree.set_document(Some(doc), JsonTreeTarget::Local, false, cx);
                tree
            });
            view = Some(tree.clone());
            Root::new(tree, window, cx)
        });
        let tree = view.expect("the window built the json tree");

        let mut prev_row_height = px(0.0);
        for font_size in [13.0, 18.0, 20.0, 28.0] {
            cx.update(|cx| apply_editor_font_size(cx, Some(font_size)));
            let row_height = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);

                    // Row 0: root `$` has a rem gap before its member count and no colon.
                    let root_name = window.find((JSON_TREE_NAME_ID_PREFIX, 0usize)).bounds();
                    let root_detail = window.find((JSON_TREE_DETAIL_ID_PREFIX, 0usize)).bounds();
                    assert!(
                        !window.try_find((JSON_TREE_COLON_ID_PREFIX, 0usize)).is_some(),
                        "root container should not show a colon"
                    );
                    assert!(
                        root_detail.left() - root_name.right() >= px(8.0),
                        "root `$` and summary must have clear gap at {font_size}px"
                    );

                    // Rows 1 (scalar `alpha`), 2 (collapsed object `nested`), and
                    // 3 (collapsed array `features`) all separate name and detail with `:`.
                    for row_ix in [1usize, 2, 3] {
                        let name = window.find((JSON_TREE_NAME_ID_PREFIX, row_ix)).bounds();
                        let colon = window.find((JSON_TREE_COLON_ID_PREFIX, row_ix)).bounds();
                        let detail = window.find((JSON_TREE_DETAIL_ID_PREFIX, row_ix)).bounds();
                        assert!(
                            name.right() < colon.left() && colon.right() < detail.left(),
                            "row {row_ix} must separate name, `:`, and detail at {font_size}px"
                        );
                    }

                    window.find((JSON_TREE_NAME_ID_PREFIX, 2usize)).bounds().size.height
                })
                .expect("the window is open");

            assert!(
                row_height >= px(font_size),
                "row height ({row_height:?}) must not clip {font_size}px text"
            );
            assert!(
                row_height > prev_row_height,
                "row height must grow with font size ({row_height:?} > {prev_row_height:?} at {font_size}px)"
            );
            prev_row_height = row_height;
        }

        // Empty / invalid JSON state renders its dedicated label cleanly.
        tree.update(cx, |tree, cx| {
            tree.set_document(None, JsonTreeTarget::Local, false, cx);
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(JSON_TREE_INVALID_ID).is_some());
        })
        .expect("the window is open");

        cx.update(|cx| apply_editor_font_size(cx, None));
    }
}
