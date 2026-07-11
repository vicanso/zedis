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

//! `ListDelegate` for the virtualized key tree: row rendering (icon,
//! tag bar, TTL chip, inline delete) and the right-click context menu.

use super::*;

pub(super) struct KeyTreeDelegate {
    pub(super) items: Vec<KeyTreeItem>,
    pub(super) enabled_multiple_selection: bool,
    pub(super) selected_items: AHashSet<SharedString>,
    pub(super) readonly: bool,
    /// Read in `render_item` to highlight the row whose key is the editor's
    /// active key. Keyed off the persistent `ZedisServerState::key()` instead
    /// of the list's transient selected index (reset on every tree rebuild —
    /// which made the highlight vanish a moment after selecting).
    pub(super) server_state: Entity<ZedisServerState>,
}

impl KeyTreeDelegate {
    pub(super) fn toggle_multiple_selection(&mut self, cx: &mut Context<ListState<Self>>) {
        self.enabled_multiple_selection = !self.enabled_multiple_selection;
        if self.enabled_multiple_selection {
            self.selected_items.clear();
        }
        cx.notify();
    }
}

impl ListDelegate for KeyTreeDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        // Folder icons use a calm neutral instead of a loud yellow — the folder
        // name (brighter) already carries the emphasis and the design keeps the
        // tree quiet.
        let folder_icon_color = cx.theme().foreground.alpha(0.7);
        let entry = self.items.get(ix.row)?;
        // Synthetic "Load more" row for an incomplete folder scan — a simple
        // clickable line indented to the folder's child level. The list's
        // Select event routes the click to `load_more_prefix` (see
        // `select_item_by_index`).
        if entry.load_more_prefix.is_some() {
            let primary = cx.theme().primary;
            let muted = cx.theme().muted_foreground;
            let loaded_count = entry.children_count;
            let label = entry.label.clone();
            return Some(
                ListItem::new(ix).w_full().py_2().px_2().child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        // Indent to the folder's child level (same formula as
                        // normal rows) so the row visually belongs to its
                        // folder — centering it made nested folders' stacked
                        // "Load more" rows indistinguishable.
                        .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
                        .text_color(primary)
                        .child(Icon::new(CustomIconName::ChevronsDown))
                        .child(Label::new(label).text_sm().text_color(primary).text_ellipsis())
                        // Loaded-so-far count at the right edge, mirroring the
                        // folder rows' count column (bare number, no wording).
                        .when(loaded_count > 0, |this| {
                            this.child(div().flex_1()).child(
                                Label::new(group_thousands(loaded_count as u64))
                                    .text_sm()
                                    .text_color(muted),
                            )
                        }),
                ),
            );
        }
        let icon = if !entry.is_folder {
            // Key item: plain type label in a fixed-width column so the key
            // names line up regardless of type (design). The left margin opens
            // a gap between the folder guide line and the type label without
            // touching the per-level indent or the type→name spacing.
            div()
                .flex_none()
                .ml(px(8.))
                .w(px(TYPE_BADGE_COL_WIDTH))
                .child(KeyTypeBadge::new(entry.key_type).plain(true))
                .into_any_element()
        } else if entry.expanded {
            // Expanded folder: open icon, one brightness step up (paired with
            // the brighter label below) so open folders are scannable among
            // collapsed siblings without adding a chevron column.
            Icon::new(IconName::FolderOpen)
                .text_color(cx.theme().foreground.alpha(0.9))
                .into_any_element()
        } else {
            // Collapsed folder: Show closed folder icon
            Icon::new(IconName::Folder)
                .text_color(folder_icon_color)
                .into_any_element()
        };

        let is_dark = cx.theme().is_dark();
        let is_folder = entry.is_folder;
        let is_scanning = entry.is_scanning;

        // Selection — highlight the row whose key is the editor's active key,
        // keyed off the persistent server-state key so it survives tree
        // rebuilds (the list's selected index is cleared on every rebuild,
        // which made the accent vanish moments after selecting). Gated to leaf
        // keys: folders toggle expand/collapse on click, never "select".
        let is_selected_row = !is_folder && self.server_state.read(cx).key().as_ref() == Some(&entry.id);
        // Theme-neutral selection fill (~10% foreground), the same recipe the
        // sidebar uses so the highlight reads on any theme.
        let selected_row_bg = cx.theme().foreground.alpha(0.1);
        // Faint zebra stripe fill for banded leaf rows. Dark needs a higher
        // alpha than light — white-on-dark reads weaker than black-on-light at
        // the same opacity.
        let stripe_bg = if is_dark {
            Hsla::white().alpha(0.06)
        } else {
            Hsla::black().alpha(0.03)
        };
        // Selection accent (#6b95c4, both themes) for the left bar.
        let accent_color: Hsla = rgb(0x6b95c4).into();

        // Folders sit one brightness step below leaves; an *expanded* folder
        // steps back up to full foreground (with its open icon, see above) so
        // "which folders are open" reads at a glance.
        let label_color = if is_folder && !entry.expanded {
            cx.theme().foreground.alpha(0.85)
        } else {
            cx.theme().foreground
        };
        // Row label — built up front so the builder chain below just drops it
        // in. No weight change on selection (a font-weight swap re-rasterizes
        // the glyphs and reads as a flicker); the accent bar + fill mark it.
        // `whitespace_nowrap` is what makes `text_ellipsis` actually truncate:
        // without it a long folder name wraps to a second line instead
        // (same pairing as the editor-header key name).
        let row_label = Label::new(entry.label.clone())
            .text_color(label_color)
            .text_ellipsis()
            .whitespace_nowrap();
        // TTL text — rendered on every leaf row that has a known TTL value.
        // `< 1h` ⇒ warm amber accent, everything else (comfortably live, or
        // perm `-1` ∞) ⇒ muted gray. Missing (`-2`, race between SCAN and TTL)
        // renders nothing. Gated by the user setting; when off the SCAN loop
        // also skipped the TTL command (see `RedisClient::scan`).
        let show_ttl = self.server_state.read(cx).show_key_tree_ttl();
        let ttl_chip: Option<(SharedString, Hsla)> = if is_folder || !show_ttl {
            None
        } else {
            entry.ttl_secs.and_then(|secs| {
                // `ttl_chip_kind` is the "render a chip?" gate (None ⇒ the -2
                // SCAN/TTL race ⇒ no chip); the colour below is keyed off the
                // raw seconds so the warning window is exactly "< 1h".
                ttl_chip_kind(secs)?;
                let label = format_ttl_chip(secs)?;
                // Three-tier TTL colour (design): seconds left (< 1m) ⇒ red
                // (about to vanish), minutes left (< 1h) ⇒ warm amber (#cba26a),
                // anything calmer (≥ 1h) or perm ∞ (secs == -1) ⇒ muted.
                let amber: Hsla = rgb(0xcba26a).into();
                let color = if (0..60).contains(&secs) {
                    cx.theme().red
                } else if (60..3600).contains(&secs) {
                    amber
                } else {
                    cx.theme().muted_foreground
                };
                Some((label, color))
            })
        };

        // Full-bleed hover fill applied on the content div below. In dark a grey
        // tint blends into the row, so use a saturated blue that stands out; in
        // light the native `list_hover` already reads fine — leave it there.
        let hover_bg = if is_dark {
            cx.theme().blue.alpha(0.3)
        } else {
            cx.theme().blue.alpha(0.1)
        };
        let show_check_icon = self.enabled_multiple_selection && !is_folder;
        let selected = if show_check_icon && let Some(item) = self.items.get(ix.row) {
            let id = &item.id;
            self.selected_items.contains(id)
        } else {
            false
        };
        let selected_items_count = self.selected_items.len();
        let id = entry.id.clone();
        let readonly = self.readonly;
        // Pre-resolve client-side annotation visuals before the
        // ListItem builder chain — we need both the colour (for the
        // left-edge bar) and the note (for hover tooltip) below, and
        // theming reads can't borrow `cx` across the chain.
        let tag_color = entry.tag.map(|c| theme_color_for_tag(c, cx));
        // Left bar: the selection accent wins on the active row; otherwise it
        // carries the tag colour (transparent when untagged → identical height).
        let row_border = if is_selected_row {
            accent_color
        } else {
            tag_color.unwrap_or(gpui::transparent_black())
        };
        let note = entry.note.clone();
        let has_note = !note.is_empty();
        let folder_tag_summary = entry.folder_tag_summary.clone();
        let has_folder_tags = is_folder && !folder_tag_summary.is_empty();
        let tag_mixed = entry.tag_mixed;
        let edit_tag_id = entry.id.clone();
        let del_id = entry.id.clone();
        // Inline delete only on leaf keys (folders use their context-menu
        // delete) and only when writes are allowed.
        let show_inline_delete = !is_folder && Capability::DeleteKey.allowed(readonly);
        // When the row shows a TTL chip, scope the inline delete to that chip
        // slot (hover the chip to swap it for the delete) instead of revealing
        // on hover of the whole row. Rows without a chip fall back to row-hover.
        let has_ttl_chip = ttl_chip.is_some();
        let delete_tooltip = i18n_key_tree(cx, "delete_key_tooltip");
        // Dashed tree connectors: one vertical guide per ancestor level so the
        // children under an expanded folder read as a connected group. Each
        // segment bridges the row's `mb_1` gap (bottom −4px) to meet the row
        // below into a continuous dashed line. Top-level rows (depth 0) have
        // none.
        let guide_color = cx.theme().muted_foreground.alpha(0.5);
        let guides: Vec<gpui::AnyElement> = (1..=entry.depth)
            .map(|level| {
                div()
                    .absolute()
                    .top_0()
                    .bottom(px(-4.))
                    // Centered on the ancestor folder's icon (one indent level
                    // left of the child content) so the line drops from the
                    // folder; the wider TREE_INDENT_BASE leaves the gap before
                    // the child's type label.
                    .left(px((level - 1) as f32 * TREE_INDENT_BASE + TREE_INDENT_OFFSET + 8.0))
                    .w_0()
                    .border_l_1()
                    .border_dashed()
                    .border_color(guide_color)
                    .into_any_element()
            })
            .collect();
        Some(
            ListItem::new(ix)
                .font_family(get_mono_font_family())
                .w_full()
                // Padding/background/hover live on the content div below so the
                // hover fill is full-bleed and high-contrast; zero ListItem's
                // own padding here.
                .py_0()
                .px_0()
                .mb_1()
                .child(
                    div()
                        // Hover group so the inline delete button (below)
                        // can reveal itself only while this row is hovered.
                        .group("ktree-row")
                        // Positioning context for the absolute dashed guides.
                        .relative()
                        .w_full()
                        .py_2()
                        .px_2()
                        .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
                        // Extra right padding so the floating scrollbar (16px
                        // track) doesn't cover the right-aligned TTL / inline
                        // delete button.
                        .pr(px(14.))
                        // 3px left bar carries the selection accent (or the tag
                        // colour); transparent when neither, so row height never
                        // jitters.
                        .border_l_3()
                        .border_color(row_border)
                        // Zebra: every second leaf under a folder gets a faint
                        // stripe; the selected row's fill overrides it, and
                        // hover overrides both.
                        .when(entry.stripe, |this| this.bg(stripe_bg))
                        .when(is_selected_row, |this| this.bg(selected_row_bg))
                        .hover(|s| s.bg(hover_bg))
                        .children(guides)
                        .context_menu(move |mut menu, _window, cx| {
                            let id = id.clone();
                            let multi_selection_count = if selected { selected_items_count } else { 0 };
                            // Capability matrix (`connection::Capability`) is the
                            // source of truth for what survives read-only mode.
                            if selected && selected_items_count > 1 {
                                if Capability::DeleteKeys.allowed(readonly) {
                                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                                    let text = t!(
                                        "key_tree.delete_keys_tooltip",
                                        count = selected_items_count,
                                        locale = locale
                                    );
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::ListX,
                                        Box::new(KeyTreeAction::DeleteMultipleKeys),
                                        move |_, _cx| Label::new(text.clone()),
                                    );
                                }
                                if Capability::SetTtl.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Clock3,
                                        Box::new(KeyTreeAction::SetTtlMultipleKeys),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                                    );
                                }
                                if Capability::PersistTtl.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Clock3,
                                        Box::new(KeyTreeAction::PersistMultipleKeys),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                                    );
                                }
                            } else if is_folder {
                                if Capability::RefreshFolder.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::RotateCw,
                                        Box::new(KeyTreeAction::RefreshFolder(id.clone())),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "refresh_folder_tooltip")),
                                    );
                                }
                                if Capability::DeleteFolder.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::X,
                                        Box::new(KeyTreeAction::DeleteFolder(id.clone())),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "delete_folder_tooltip")),
                                    );
                                }
                                if Capability::SetTtl.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Clock3,
                                        Box::new(KeyTreeAction::SetTtlFolder(id.clone())),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                                    );
                                }
                                if Capability::PersistTtl.allowed(readonly) {
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Clock3,
                                        Box::new(KeyTreeAction::PersistFolder(id.clone())),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                                    );
                                }
                            } else if Capability::DeleteKey.allowed(readonly) {
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::X,
                                    Box::new(KeyTreeAction::DeleteKey(id.clone())),
                                    move |_, cx| Label::new(i18n_key_tree(cx, "delete_key_tooltip")),
                                );
                            }
                            // Export is a Redis read + local write — always OK.
                            if Capability::ExportKeys.allowed(readonly) {
                                if multi_selection_count > 0 {
                                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                                    let text = t!(
                                        "key_tree.export_selected_tooltip",
                                        count = multi_selection_count,
                                        locale = locale
                                    );
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Save,
                                        Box::new(KeyTreeAction::ExportSelectedKeys),
                                        move |_, _cx| Label::new(text.clone()),
                                    );
                                } else if is_folder {
                                    let folder_id = id.clone();
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Save,
                                        Box::new(KeyTreeAction::ExportFolder(folder_id)),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "export_folder_tooltip")),
                                    );
                                } else {
                                    let key_id = id.clone();
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::Save,
                                        Box::new(KeyTreeAction::ExportKey(key_id)),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "export_key_tooltip")),
                                    );
                                }
                            }
                            // Import keys lives under Tools (status bar) —
                            // it targets the current server/db, not a tree
                            // prefix, so the context menu no longer offers it.
                            // Tag & note — local redb only.
                            // Multi-select → batch colour only (notes preserved).
                            // Single leaf → full tag + note dialog.
                            if Capability::EditLocalMetadata.allowed(readonly) {
                                if multi_selection_count > 1 {
                                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                                    let text = t!(
                                        "key_tag.batch_menu_label",
                                        count = multi_selection_count,
                                        locale = locale
                                    );
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::SwatchBook,
                                        Box::new(KeyTreeAction::BatchTagSelectedKeys),
                                        move |_, _cx| Label::new(text.clone()),
                                    );
                                } else if !is_folder && multi_selection_count == 0 {
                                    let tag_key = edit_tag_id.clone();
                                    menu = menu.menu_element_with_icon(
                                        CustomIconName::FilePenLine,
                                        Box::new(KeyTreeAction::EditKeyTag(tag_key)),
                                        move |_, cx| Label::new(i18n_key_tag(cx, "edit_menu_label")),
                                    );
                                }
                            }
                            // Single-row affordances: copy name / prefix,
                            // rename, favorite toggle.
                            if multi_selection_count == 0 {
                                if Capability::CopyToClipboard.allowed(readonly) {
                                    if is_folder {
                                        menu = menu.menu_element_with_icon(
                                            IconName::Copy,
                                            Box::new(KeyTreeAction::CopyFolderPrefix(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "copy_prefix_tooltip")),
                                        );
                                    } else {
                                        menu = menu.menu_element_with_icon(
                                            IconName::Copy,
                                            Box::new(KeyTreeAction::CopyKeyName(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "copy_key_tooltip")),
                                        );
                                    }
                                }
                                if !is_folder {
                                    if Capability::RenameKey.allowed(readonly) {
                                        menu = menu.menu_element_with_icon(
                                            CustomIconName::FilePenLine,
                                            Box::new(KeyTreeAction::RenameKey(id.clone())),
                                            move |_, cx| Label::new(i18n_key_tree(cx, "rename_key_tooltip")),
                                        );
                                    }
                                    if Capability::EditLocalMetadata.allowed(readonly) {
                                        // Star state comes from the local
                                        // favorites store, keyed by server.
                                        let is_favorited = cx
                                            .global::<ZedisGlobalStore>()
                                            .read(cx)
                                            .selected_server()
                                            .map(|(server_id, _)| {
                                                get_favorites_manager()
                                                    .records(server_id.as_ref())
                                                    .unwrap_or_default()
                                                    .iter()
                                                    .any(|k| k.as_ref() == id.as_ref())
                                            })
                                            .unwrap_or(false);
                                        let (icon, label_key) = if is_favorited {
                                            (IconName::StarFill, "remove_favorite_tooltip")
                                        } else {
                                            (IconName::Star, "add_favorite_tooltip")
                                        };
                                        menu = menu.menu_element_with_icon(
                                            icon,
                                            Box::new(KeyTreeAction::ToggleFavoriteKey(id.clone())),
                                            move |_, cx| Label::new(i18n_editor(cx, label_key)),
                                        );
                                    }
                                }
                            }
                            menu
                        })
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .flex_1()
                                .min_w_0()
                                // Positioning context for the absolute, hover-only
                                // delete button below (out of flow → reserves no
                                // width). The TTL chip hides on hover so the button
                                // takes its spot without overlapping it.
                                .relative()
                                .child(icon)
                                .child(
                                    div()
                                        .id(("ktree-label", ix.row))
                                        .flex_1()
                                        .min_w_0()
                                        // Hover tooltip: full key / prefix path
                                        // (visible label is last segment only),
                                        // plus leaf note or folder tag aggregate.
                                        .tooltip({
                                            let text: SharedString = if has_note {
                                                format!("{}\n{}", entry.id, note).into()
                                            } else if has_folder_tags {
                                                let mix = if tag_mixed { " (mixed)" } else { "" };
                                                format!("{}\nTags{mix}: {}", entry.id, folder_tag_summary).into()
                                            } else {
                                                entry.id.clone()
                                            };
                                            move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                                            }
                                        })
                                        .child(row_label),
                                )
                                .when(show_check_icon, |this| {
                                    let check_icon = if selected {
                                        CustomIconName::SquareCheck
                                    } else {
                                        CustomIconName::Square
                                    };
                                    this.child(Icon::new(check_icon))
                                })
                                .when_some(ttl_chip, |this, (chip_label, chip_color)| {
                                    this.child(
                                        div()
                                            // Hover scope limited to this chip slot
                                            // (its own group): hovering the chip — not
                                            // the whole row — swaps it for the delete
                                            // button, so the TTL stays visible when
                                            // hovering elsewhere on the row.
                                            .group("ktree-ttl")
                                            .relative()
                                            .flex_none()
                                            .child(
                                                div().group_hover("ktree-ttl", |s| s.invisible()).child(
                                                    // Plain TTL text (no chip chrome) —
                                                    // calm and right-aligned, matching
                                                    // the design.
                                                    Label::new(chip_label)
                                                        .text_size(px(10.))
                                                        .w(px(TTL_CHIP_WIDTH))
                                                        .text_right()
                                                        .text_color(chip_color)
                                                        .flex_none(),
                                                ),
                                            )
                                            .when(show_inline_delete, |this| {
                                                let del_id = del_id.clone();
                                                let tooltip = delete_tooltip.clone();
                                                this.child(
                                                    div()
                                                        .absolute()
                                                        .inset_0()
                                                        .flex()
                                                        .items_center()
                                                        // Right-align so the small X hugs the
                                                        // right edge of the (wider) TTL slot
                                                        // instead of floating centred with a
                                                        // big gap to its right.
                                                        .justify_end()
                                                        .invisible()
                                                        .group_hover("ktree-ttl", |s| s.visible())
                                                        .child(
                                                            Button::new(("ktree-del", ix.row))
                                                                .ghost()
                                                                .xsmall()
                                                                .icon(CustomIconName::X)
                                                                .tooltip(tooltip)
                                                                .on_click(move |_, window, cx| {
                                                                    cx.stop_propagation();
                                                                    window.dispatch_action(
                                                                        Box::new(KeyTreeAction::DeleteKey(
                                                                            del_id.clone(),
                                                                        )),
                                                                        cx,
                                                                    );
                                                                }),
                                                        ),
                                                )
                                            }),
                                    )
                                })
                                .when(is_folder && is_scanning, |this| {
                                    // Inline spinner while this folder's lazy
                                    // prefix-scan is still in flight.
                                    this.child(Spinner::new().with_size(px(14.)).color(cx.theme().muted_foreground))
                                })
                                .when(entry.is_folder, |this| {
                                    this.child(
                                        Label::new(group_thousands(entry.children_count as u64))
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                })
                                // Row-hover inline delete — used only when the row has
                                // no TTL chip (chip rows scope the delete to the chip
                                // slot above). Absolutely positioned (out of flow → no
                                // reserved width); dispatches the same `DeleteKey`
                                // action as the context menu (confirm + PROD escalation).
                                .when(show_inline_delete && !has_ttl_chip, |this| {
                                    let del_id = del_id.clone();
                                    let tooltip = delete_tooltip.clone();
                                    this.child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            .top_0()
                                            .bottom_0()
                                            .w(px(INLINE_DELETE_WIDTH))
                                            .flex()
                                            .items_center()
                                            // Right-align the X to the slot's right edge so it
                                            // doesn't sit centred with a big gap to its right.
                                            .justify_end()
                                            .invisible()
                                            .group_hover("ktree-row", |s| s.visible())
                                            .child(
                                                Button::new(("ktree-del", ix.row))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(CustomIconName::X)
                                                    .tooltip(tooltip)
                                                    .on_click(move |_, window, cx| {
                                                        cx.stop_propagation();
                                                        window.dispatch_action(
                                                            Box::new(KeyTreeAction::DeleteKey(del_id.clone())),
                                                            cx,
                                                        );
                                                    }),
                                            ),
                                    )
                                }),
                        ),
                ),
        )
    }

    fn set_selected_index(&mut self, ix: Option<IndexPath>, _window: &mut Window, _cx: &mut Context<ListState<Self>>) {
        if self.enabled_multiple_selection
            && let Some(ix) = ix
            && let Some(item) = self.items.get(ix.row)
        {
            let id = &item.id;
            if self.selected_items.contains(id) {
                self.selected_items.remove(id);
            } else {
                self.selected_items.insert(id.clone());
            }
        }
    }
}
