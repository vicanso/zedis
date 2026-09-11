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
    App, ClipboardItem, Entity, EventEmitter, SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
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

/// Horizontal step per nesting level.
const INDENT: f32 = 16.0;
/// Left inset of every row, chevron included.
const ROW_INSET: f32 = 8.0;
/// Width a scalar row leaves blank where a container has its chevron.
const CHEVRON_WIDTH: f32 = 16.0;

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
            expanded: HashSet::new(),
            _subscriptions: vec![subscription],
        }
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
            ))
            .item(item(
                RowCommand::Query,
                i18n_editor(cx, "json_tree_query"),
                Some(IconName::Search),
            ));
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

fn render_row(ix: usize, entry: &TreeEntry, rows: &HashMap<SharedString, Row>, cx: &App) -> ListItem {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
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
    let scalar = kind.is_some_and(|kind| !kind.is_container());
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
            .pl(px(INDENT * entry.depth() as f32 + ROW_INSET))
            .font_family(get_mono_font_family())
            .child(match chevron {
                Some(icon) => Icon::new(icon).xsmall().text_color(muted).into_any_element(),
                None => div().w(px(CHEVRON_WIDTH)).flex_shrink_0().into_any_element(),
            })
            .child(Label::new(name).text_sm().text_color(name_color).whitespace_nowrap())
            .when(scalar, |this| this.child(Label::new(":").text_sm().text_color(muted)))
            .when(!detail.is_empty(), |this| {
                this.child(
                    Label::new(detail)
                        .text_sm()
                        .text_color(detail_color)
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
        };
        Tree::new(&self.tree_state, move |ix, entry, _selected, _window, cx| {
            render_row(ix, entry, &rows, cx)
        })
        .context_menu(move |_ix, entry, menu, _window, cx| scope.menu(entry, menu, cx))
        .size_full()
        .into_any_element()
    }
}
