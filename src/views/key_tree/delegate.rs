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
//! tag bar, TTL chip, inline delete) and the selection; the right-click
//! menu is `row_menu`'s.

use super::render::tag_color_label_key;
use super::row_menu::RowMenu;
use super::*;
use gpui::{AnyElement, DismissEvent, Div};

/// The selection checkbox of a key row, with the row's index.
const ROW_CHECKBOX_ID: &str = "ktree-check";

/// The keys between two rows, in visual order. Folders and the synthetic
/// "Load more" row are skipped: a folder id is a prefix, and every batch op
/// reads a selected id as a key name. The two ends may come in either
/// order, and an out-of-range end is clamped.
fn keys_between(items: &[KeyTreeItem], a: usize, b: usize) -> Vec<SharedString> {
    if items.is_empty() {
        return Vec::new();
    }
    let last = items.len() - 1;
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    items[from.min(last)..=to.min(last)]
        .iter()
        .filter(|item| !item.is_folder && item.load_more_prefix.is_none())
        .map(|item| item.id.clone())
        .collect()
}

/// What a Shift-click selects: the picks that stood when the anchor was
/// set, plus the keys spanned now. Built from the *base* rather than from
/// the current selection, so clicking a nearer row shrinks the range
/// instead of only ever growing it, while picks made before the anchor
/// survive either way.
fn range_selection(
    base: &AHashSet<SharedString>,
    items: &[KeyTreeItem],
    anchor: usize,
    row: usize,
) -> AHashSet<SharedString> {
    let mut selection = base.clone();
    selection.extend(keys_between(items, anchor, row));
    selection
}

/// Drop the picks that name no key row of `items`.
fn retain_picks_with_rows(picks: &mut AHashSet<SharedString>, items: &[KeyTreeItem]) {
    if picks.is_empty() {
        return;
    }
    let rows: AHashSet<&SharedString> = items
        .iter()
        .filter(|item| !item.is_folder && item.load_more_prefix.is_none())
        .map(|item| &item.id)
        .collect();
    picks.retain(|id| rows.contains(id));
}

pub(super) struct KeyTreeDelegate {
    pub(super) items: Vec<KeyTreeItem>,
    pub(super) enabled_multiple_selection: bool,
    pub(super) selected_items: AHashSet<SharedString>,
    /// Row the last non-Shift selecting click landed on — one end of a
    /// Shift range. Reset whenever the selection is cleared, so a range
    /// never spans two unrelated trees.
    pub(super) range_anchor: Option<usize>,
    /// The selection as it stood when that anchor was set. A Shift-click
    /// rebuilds the selection as this **plus** the new range, so clicking
    /// a nearer row shrinks the range instead of only ever growing it, and
    /// picks made before the anchor survive either way.
    pub(super) range_base: AHashSet<SharedString>,
    pub(super) readonly: bool,
    /// Read in `render_item` to highlight the row whose key is the editor's
    /// active key. Keyed off the persistent `ZedisServerState::key()` instead
    /// of the list's transient selected index (reset on every tree rebuild —
    /// which made the highlight vanish a moment after selecting).
    pub(super) server_state: Entity<ZedisServerState>,
    /// The key or folder whose right-click menu is open, if one is. Its row
    /// offers no name tooltip meanwhile: the tooltip waits half a second
    /// after the pointer arrives, a right-click does not cancel it, and it
    /// opened over the menu's first item — Delete key…, which a report then
    /// said was not there (#226).
    pub(super) menu_row: Rc<RefCell<Option<SharedString>>>,
}

impl KeyTreeDelegate {
    pub(super) fn clear_selection(&mut self) {
        self.selected_items.clear();
        self.range_base.clear();
        self.range_anchor = None;
    }

    /// Keep the picks that still have a row after a rebuild. They are key
    /// names, so an expand, a SCAN page or a refresh need not drop them; a
    /// key that went away (or sits in a folder now closed) does, since an
    /// action on the selection should only reach rows the user can see. The
    /// Shift anchor is a row index, which a rebuild moves, so it restarts.
    pub(super) fn retain_selection(&mut self) {
        retain_picks_with_rows(&mut self.selected_items, &self.items);
        self.range_base = self.selected_items.clone();
        self.range_anchor = None;
    }

    /// Pick or unpick one key and move the Shift anchor to its row, as a
    /// secondary-modifier click does. The row's checkbox hands it the state
    /// it was ticked to; a row click toggles.
    pub(super) fn set_pick(&mut self, id: SharedString, picked: bool, row: usize) {
        if picked {
            self.selected_items.insert(id);
        } else {
            self.selected_items.remove(&id);
        }
        self.range_anchor = Some(row);
        self.range_base = self.selected_items.clone();
    }

    pub(super) fn toggle_multiple_selection(&mut self, cx: &mut Context<ListState<Self>>) {
        // The entry reads as "multi-selecting", which a modifier click can
        // start without the mode ever being on. So when either is true,
        // clicking it means *stop*: drop the picks and leave the mode off.
        let active = self.enabled_multiple_selection || !self.selected_items.is_empty();
        self.enabled_multiple_selection = !active;
        self.clear_selection();
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
        let entry = self.items.get(ix.row)?;
        if entry.load_more_prefix.is_some() {
            return Some(load_more_row(ix, entry, cx));
        }
        let icon = row_icon(entry, cx);

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

        let show_ttl = self.server_state.read(cx).show_key_tree_ttl();
        let ttl_chip = ttl_chip(entry, show_ttl, cx);

        // Full-bleed hover fill applied on the content div below. In dark a grey
        // tint blends into the row, so use a saturated blue that stands out; in
        // light the native `list_hover` already reads fine — leave it there.
        let hover_bg = if is_dark {
            cx.theme().blue.alpha(0.3)
        } else {
            cx.theme().blue.alpha(0.1)
        };
        // The check column appears in the mode, and also whenever a
        // selection exists — modifier clicks can build one without it.
        let show_check_icon = (self.enabled_multiple_selection || !self.selected_items.is_empty()) && !is_folder;
        let selected = if show_check_icon && let Some(item) = self.items.get(ix.row) {
            let id = &item.id;
            self.selected_items.contains(id)
        } else {
            false
        };
        let selected_items_count = self.selected_items.len();
        let readonly = self.readonly;
        let menu_row = self.menu_row.clone();
        let menu_open_here = menu_row.borrow().as_ref() == Some(&entry.id);
        let tree = cx.entity().downgrade();
        // Remove TTL is offered unless the key is known not to expire
        // (`-1`), or known to be gone (`-2`); with the TTL column off it is
        // not known, and offered.
        let may_expire = entry.ttl_secs.is_none_or(|secs| secs >= 0);
        // Pre-resolve the tag colour (the left-edge bar) before the ListItem
        // builder chain: theming reads can't borrow `cx` across the chain.
        // The note and a folder's tags are `row_name`'s.
        let tag_color = entry.tag.map(|c| theme_color_for_tag(c, cx));
        // Left bar: the key's tag colour, transparent when untagged (so the
        // row is the same width either way). The selected row is marked by
        // its fill alone — it used to take this bar over in the accent
        // colour, which hid the tag of the one key being looked at.
        let row_border = tag_color.unwrap_or(gpui::transparent_black());
        let del_id = entry.id.clone();
        // Inline delete only on leaf keys (folders use their context-menu
        // delete), only when writes are allowed, and never beside the
        // selection checkbox: the row-hover button is drawn over the row's
        // end, which is where the checkbox sits, so ticking a key opened
        // its delete confirmation (#196). The context menu still deletes.
        let show_inline_delete = !is_folder && !show_check_icon && Capability::DeleteKey.allowed(readonly);
        // When the row shows a TTL chip, scope the inline delete to that chip
        // slot (hover the chip to swap it for the delete) instead of revealing
        // on hover of the whole row. Rows without a chip fall back to row-hover.
        let has_ttl_chip = ttl_chip.is_some();
        let delete_tooltip = i18n_key_tree(cx, "delete_key_tooltip");
        let guide_color = cx.theme().muted_foreground.alpha(0.5);
        let guides = depth_guides(entry.depth, guide_color);
        let row_menu = RowMenu {
            id: entry.id.clone(),
            is_folder,
            selected,
            selected_items_count,
            readonly,
            may_expire,
        };
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
                        .py_1p5()
                        .px_2()
                        .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
                        // Extra right padding so the floating scrollbar (16px
                        // track) doesn't cover the right-aligned TTL / inline
                        // delete button.
                        .pr(px(14.))
                        // 3px left bar carries the tag colour; transparent when
                        // untagged, so the row never shifts.
                        .border_l_3()
                        .border_color(row_border)
                        // Zebra: every second leaf under a folder gets a faint
                        // stripe; the selected row's fill overrides it, and
                        // hover overrides both.
                        .when(entry.stripe, |this| this.bg(stripe_bg))
                        .when(is_selected_row, |this| this.bg(selected_row_bg))
                        .hover(|s| s.bg(hover_bg))
                        .children(guides)
                        .context_menu(move |menu, _window, cx| {
                            // This row's tooltip is withheld until the menu
                            // is dismissed (`menu_row`).
                            menu_row.replace(Some(row_menu.id.clone()));
                            let _ = tree.update(cx, |_, cx| cx.notify());
                            let open = menu_row.clone();
                            let tree = tree.clone();
                            cx.subscribe_self(move |_, _: &DismissEvent, cx| {
                                open.borrow_mut().take();
                                let _ = tree.update(cx, |_, cx| cx.notify());
                            })
                            .detach();
                            row_menu.build(menu, cx)
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
                                .child(row_name(ix, entry, menu_open_here, cx))
                                // A checkbox of its own, not a picture of one: it
                                // picks the key whether or not the mode is on (a
                                // selection made with modifier clicks shows it
                                // too, where the row's plain click opens the key),
                                // and the row's click does not also run.
                                .when(show_check_icon, |this| {
                                    let pick_id = entry.id.clone();
                                    let row = ix.row;
                                    this.child(
                                        Checkbox::new((ROW_CHECKBOX_ID, ix.row))
                                            .checked(selected)
                                            .tab_stop(false)
                                            .flex_none()
                                            .on_click(cx.listener(move |state, picked: &bool, _, cx| {
                                                cx.stop_propagation();
                                                state.delegate_mut().set_pick(pick_id.clone(), *picked, row);
                                                cx.notify();
                                            })),
                                    )
                                })
                                .when_some(ttl_chip, |this, (chip_label, chip_color)| {
                                    this.child(ttl_slot(
                                        ix.row,
                                        chip_label,
                                        chip_color,
                                        show_inline_delete,
                                        &del_id,
                                        &delete_tooltip,
                                    ))
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
                                .when(show_inline_delete && !has_ttl_chip, |this| {
                                    this.child(row_hover_delete(ix.row, del_id.clone(), delete_tooltip.clone()))
                                }),
                        ),
                ),
        )
    }

    /// Selection is state, not a mode: Shift and the platform's secondary
    /// modifier always work on it, because neither collides with a plain
    /// click, whose job here is to *open* the key. The mode only decides
    /// whether a plain click selects too — the touch-style path, kept for
    /// picking many rows without holding a key down.
    fn set_selected_index(&mut self, ix: Option<IndexPath>, window: &mut Window, _cx: &mut Context<ListState<Self>>) {
        let Some(ix) = ix else {
            return;
        };
        let Some(item) = self.items.get(ix.row) else {
            return;
        };
        // Neither gesture means anything on a folder or a "Load more" row.
        let selectable = !item.is_folder && item.load_more_prefix.is_none();
        let modifiers = window.modifiers();

        if modifiers.shift
            && let Some(anchor) = self.range_anchor
            && anchor < self.items.len()
        {
            // The base plus the new range, not the old selection plus it:
            // that is what lets a nearer second click shrink the range.
            self.selected_items = range_selection(&self.range_base, &self.items, anchor, ix.row);
            return;
        }
        if !selectable {
            return;
        }
        // `secondary` is Cmd on macOS, Ctrl elsewhere.
        if !modifiers.secondary(_cx) && !self.enabled_multiple_selection {
            // A plain click in normal mode opens the key; the anchor still
            // follows it, so a Shift-click right after works from where the
            // user is looking.
            self.range_anchor = Some(ix.row);
            self.range_base = self.selected_items.clone();
            return;
        }
        let id = item.id.clone();
        let picked = !self.selected_items.contains(&id);
        self.set_pick(id, picked, ix.row);
    }
}

/// Synthetic "Load more" row for an incomplete folder scan — a simple
/// clickable line indented to the folder's child level. The list's
/// Select event routes the click to `load_more_prefix` (see
/// `select_item_by_index`).
fn load_more_row(ix: IndexPath, entry: &KeyTreeItem, cx: &App) -> ListItem {
    let muted = cx.theme().muted_foreground;
    let loaded_count = entry.children_count;
    let label = entry.label.clone();
    ListItem::new(ix).w_full().py_1p5().px_2().child(
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            // Indent to the folder's child level (same formula as
            // normal rows) so the row visually belongs to its
            // folder — centering it made nested folders' stacked
            // "Load more" rows indistinguishable.
            .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
            // Same as kv-table headers: body foreground + bold mono
            // (no primary accent). System UI fonts often don't
            // paint BOLD without a real family face.
            .child(Icon::new(CustomIconName::ChevronsDown).text_color(cx.theme().foreground))
            .child(
                Label::new(label)
                    .text_sm()
                    .font_family(get_mono_font_family())
                    .font_weight(FontWeight::BOLD)
                    .text_ellipsis(),
            )
            // Loaded-so-far count at the right edge, mirroring the
            // folder rows' count column (bare number, no wording).
            .when(loaded_count > 0, |this| {
                this.child(div().flex_1()).child(
                    Label::new(group_thousands(loaded_count as u64))
                        .text_sm()
                        .text_color(muted),
                )
            }),
    )
}

/// A key's type label, or a folder's icon, open or closed.
fn row_icon(entry: &KeyTreeItem, cx: &App) -> AnyElement {
    // Folder icons use a calm neutral instead of a loud yellow — the folder
    // name (brighter) already carries the emphasis and the design keeps the
    // tree quiet.
    let folder_icon_color = cx.theme().foreground.alpha(0.7);
    if !entry.is_folder {
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
    }
}

/// TTL text — rendered on a leaf row whose key expires. `< 1h` ⇒ warm
/// amber accent, anything calmer ⇒ muted gray. A key without a TTL
/// (`-1`) gets nothing: it used to get "∞", which put the same mark on
/// every row of most databases and buried the few keys that do expire.
/// Missing (`-2`, race between SCAN and TTL) renders nothing either.
/// Gated by the user setting; when off the SCAN loop also skipped the
/// TTL command (see `RedisClient::scan`).
fn ttl_chip(entry: &KeyTreeItem, show_ttl: bool, cx: &App) -> Option<(SharedString, Hsla)> {
    let is_folder = entry.is_folder;
    if is_folder || !show_ttl {
        None
    } else {
        entry.ttl_secs.and_then(|secs| {
            // `ttl_chip_kind` is the "render a chip?" gate (None ⇒ the -2
            // SCAN/TTL race, `Perm` ⇒ no expiry: no chip for either); the
            // colour below is keyed off the raw seconds so the warning
            // window is exactly "< 1h".
            if matches!(ttl_chip_kind(secs)?, TtlChipKind::Perm) {
                return None;
            }
            let label = format_ttl_chip(secs)?;
            // Three-tier TTL colour (design): seconds left (< 1m) ⇒ red
            // (about to vanish), minutes left (< 1h) ⇒ warm amber (#cba26a),
            // anything calmer (≥ 1h) ⇒ muted.
            let amber: Hsla = rgb(0xcba26a).into();
            let color = if (0..60).contains(&secs) {
                cx.theme().red
            } else if (60..3600).contains(&secs) {
                amber
            } else {
                cx.theme().muted_foreground
            };
            Some((label.into(), color))
        })
    }
}

/// Dashed tree connectors: one vertical guide per ancestor level so the
/// children under an expanded folder read as a connected group. Each
/// segment bridges the row's `mb_1` gap (bottom −4px) to meet the row
/// below into a continuous dashed line. Top-level rows (depth 0) have
/// none.
fn depth_guides(depth: usize, guide_color: Hsla) -> Vec<AnyElement> {
    (1..=depth)
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
        .collect()
}

/// A row's name — the last segment of its key or prefix — with the whole
/// path in its tooltip, under it a key's note or a folder's tags; no
/// tooltip while the row's menu is open.
fn row_name(ix: IndexPath, entry: &KeyTreeItem, menu_open_here: bool, cx: &App) -> impl IntoElement {
    let is_folder = entry.is_folder;
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
    // the glyphs and reads as a flicker); the fill marks it.
    // `whitespace_nowrap` is what makes `text_ellipsis` actually truncate:
    // without it a long folder name wraps to a second line instead
    // (same pairing as the editor-header key name).
    let row_label = Label::new(entry.label.clone())
        .text_color(label_color)
        .text_ellipsis()
        .whitespace_nowrap();
    let note = entry.note.clone();
    let has_note = !note.is_empty();
    // A folder's tags as the tooltip lists them ("Red 3 · Blue 1"),
    // worded in the language chosen now.
    let folder_tags = (is_folder && !entry.folder_tag_counts.is_empty()).then(|| {
        let tags = entry
            .folder_tag_counts
            .iter()
            .map(|&(color, count)| format!("{} {count}", i18n_key_tag(cx, tag_color_label_key(color))))
            .collect::<Vec<_>>()
            .join(" · ");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let line = if entry.tag_mixed {
            t!("key_tree.folder_tags_mixed", tags = tags, locale = locale)
        } else {
            t!("key_tree.folder_tags", tags = tags, locale = locale)
        };
        line.to_string()
    });
    div()
        .id(("ktree-label", ix.row))
        .flex_1()
        .min_w_0()
        // Hover tooltip: full key / prefix path
        // (visible label is last segment only),
        // plus leaf note or folder tag aggregate —
        // none while the row's menu is open.
        .when(!menu_open_here, |this| {
            this.tooltip({
                let text: SharedString = if has_note {
                    format!("{}\n{}", entry.id, note).into()
                } else if let Some(tags) = &folder_tags {
                    format!("{}\n{tags}", entry.id).into()
                } else {
                    entry.id.clone()
                };
                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
            })
        })
        .child(row_label)
}

/// A key's TTL, right-aligned; with the inline delete, the delete takes
/// its place while the TTL — not the whole row — is hovered, so the TTL
/// stays readable with the pointer elsewhere on the row.
fn ttl_slot(
    row: usize,
    chip_label: SharedString,
    chip_color: Hsla,
    show_inline_delete: bool,
    del_id: &SharedString,
    delete_tooltip: &SharedString,
) -> Div {
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
            div()
                .when(show_inline_delete, |this| {
                    this.group_hover("ktree-ttl", |s| s.invisible())
                })
                .child(
                    // Plain TTL text (no chip chrome) —
                    // calm and right-aligned, matching
                    // the design.
                    // One line whatever it
                    // says: a label that wraps
                    // makes the row two lines
                    // tall (#190).
                    Label::new(chip_label)
                        .text_size(px(10.))
                        .w(px(TTL_CHIP_WIDTH))
                        .whitespace_nowrap()
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
                    .child(inline_delete_button(row, del_id, tooltip)),
            )
        })
}

/// Row-hover inline delete — used only when the row has
/// no TTL chip (chip rows scope the delete to the chip
/// slot above). Absolutely positioned (out of flow → no
/// reserved width); dispatches the same `DeleteKey`
/// action as the context menu (confirm + PROD escalation).
fn row_hover_delete(row: usize, del_id: SharedString, tooltip: SharedString) -> Div {
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
        .child(inline_delete_button(row, del_id, tooltip))
}

/// A key row's inline delete: the menu's `DeleteKey`, with its
/// confirmation and production escalation.
fn inline_delete_button(row: usize, del_id: SharedString, tooltip: SharedString) -> Button {
    Button::new(("ktree-del", row))
        .ghost()
        .xsmall()
        .icon(CustomIconName::X)
        .tooltip(tooltip)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(KeyTreeAction::DeleteKey(del_id.clone())), cx);
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestEventExt, TestWindowExt};

    fn leaf(id: &str) -> KeyTreeItem {
        KeyTreeItem {
            id: id.into(),
            label: id.into(),
            ..Default::default()
        }
    }

    fn folder(id: &str) -> KeyTreeItem {
        KeyTreeItem {
            id: id.into(),
            label: id.into(),
            is_folder: true,
            ..Default::default()
        }
    }

    fn load_more() -> KeyTreeItem {
        KeyTreeItem {
            id: "more".into(),
            load_more_prefix: Some("user:".into()),
            ..Default::default()
        }
    }

    /// `user/ user:1 user:2 <more> order/ order:1` — the flattened shape a
    /// range has to walk.
    fn rows() -> Vec<KeyTreeItem> {
        vec![
            folder("user"),
            leaf("user:1"),
            leaf("user:2"),
            load_more(),
            folder("order"),
            leaf("order:1"),
        ]
    }

    #[test]
    fn a_rebuild_keeps_the_picks_that_still_have_a_row() {
        let mut picks: AHashSet<SharedString> = ["user:1", "order:1", "gone", "user"]
            .into_iter()
            .map(SharedString::from)
            .collect();
        retain_picks_with_rows(&mut picks, &rows());
        let mut kept: Vec<&str> = picks.iter().map(|id| id.as_ref()).collect();
        kept.sort_unstable();
        // A deleted key and a folder id are not picks a row can carry.
        assert_eq!(kept, vec!["order:1", "user:1"]);
    }

    #[test]
    fn a_range_spans_folders_but_selects_only_keys() {
        let items = rows();
        // Every visible row from `user:1` to `order:1`, which crosses a
        // folder header and the "Load more" row.
        assert_eq!(
            keys_between(&items, 1, 5),
            vec![
                SharedString::from("user:1"),
                SharedString::from("user:2"),
                SharedString::from("order:1")
            ]
        );
        // Dragging upward is the same range.
        assert_eq!(keys_between(&items, 5, 1), keys_between(&items, 1, 5));
        // One row, and a stale index from a rebuilt tree.
        assert_eq!(keys_between(&items, 2, 2), vec![SharedString::from("user:2")]);
        assert_eq!(keys_between(&items, 99, 99), vec![SharedString::from("order:1")]);
        assert!(keys_between(&[], 0, 3).is_empty());
    }

    #[test]
    fn a_second_shift_click_can_shrink_the_range() {
        let items = rows();
        let mut base = AHashSet::new();
        // A key picked before the anchor: it must survive both clicks.
        base.insert(SharedString::from("kept"));

        let wide = range_selection(&base, &items, 1, 5);
        assert!(wide.contains(&SharedString::from("order:1")));
        assert_eq!(wide.len(), 4, "kept + three keys: {wide:?}");

        // Same anchor, a nearer row: the far key is gone, the earlier pick
        // is not.
        let narrow = range_selection(&base, &items, 1, 2);
        assert!(!narrow.contains(&SharedString::from("order:1")));
        assert!(narrow.contains(&SharedString::from("kept")));
        assert!(narrow.contains(&SharedString::from("user:2")));
        assert_eq!(narrow.len(), 3);
    }

    /// Draws the list alone: the rows and their controls are the
    /// delegate's, the tree around it is not needed to click them.
    struct RowsHarness {
        list: Entity<ListState<KeyTreeDelegate>>,
    }

    impl Render for RowsHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex().size_full().child(List::new(&self.list))
        }
    }

    fn picks(list: &Entity<ListState<KeyTreeDelegate>>, cx: &mut TestAppContext) -> Vec<String> {
        list.read_with(cx, |state, _| {
            let mut picks: Vec<String> = state.delegate().selected_items.iter().map(|k| k.to_string()).collect();
            picks.sort();
            picks
        })
    }

    /// A row's checkbox picks its key, and only that: in multi-select mode
    /// the row-hover delete was drawn over it, so ticking a key opened its
    /// delete confirmation (#196); and with picks made by modifier clicks
    /// and the mode off, the checkbox was a picture whose click opened the
    /// key. The clicks are real pointer events, hit-tested.
    #[gpui::test]
    fn a_row_checkbox_picks_its_key(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut list = None;
        let handle = cx.open_window(size(px(400.), px(300.)), |window, cx| {
            let delegate = KeyTreeDelegate {
                items: vec![leaf("a"), leaf("b")],
                enabled_multiple_selection: true,
                selected_items: AHashSet::new(),
                range_anchor: None,
                range_base: AHashSet::new(),
                readonly: false,
                server_state: server_state.clone(),
                menu_row: Default::default(),
            };
            let state = cx.new(|cx| ListState::new(delegate, window, cx));
            list = Some(state.clone());
            let view = cx.new(|_| RowsHarness { list: state });
            Root::new(view, window, cx)
        });
        let list = list.expect("the window built the list");

        // In the mode: once picks it — the row's own click, which also
        // toggles there, would have put it back — and again unpicks it.
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click((ROW_CHECKBOX_ID, 0usize), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(picks(&list, cx), ["a"]);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find((ROW_CHECKBOX_ID, 0usize)).checked(), Some(true));
            window.click((ROW_CHECKBOX_ID, 0usize), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert!(picks(&list, cx).is_empty());

        // Mode off, a pick made by a modifier click: the column is still
        // drawn, and its checkbox adds to the picks.
        list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.enabled_multiple_selection = false;
            delegate.set_pick("b".into(), true, 1);
            cx.notify();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click((ROW_CHECKBOX_ID, 0usize), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(picks(&list, cx), ["a", "b"]);
    }

    type Dispatched = Rc<RefCell<Vec<KeyTreeAction>>>;

    /// `items` drawn alone, and every `KeyTreeAction` dispatched from then
    /// on, in order — what the tree's own handlers would have been asked.
    fn open_rows(
        cx: &mut TestAppContext,
        items: Vec<KeyTreeItem>,
        readonly: bool,
    ) -> (AnyWindowHandle, Entity<ListState<KeyTreeDelegate>>, Dispatched) {
        let dispatched = Dispatched::default();
        cx.update(|cx| {
            let dispatched = dispatched.clone();
            cx.on_action(move |action: &KeyTreeAction, _| dispatched.borrow_mut().push(action.clone()));
        });
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut list = None;
        let handle = cx.open_window(size(px(480.), px(640.)), |window, cx| {
            let delegate = KeyTreeDelegate {
                items,
                enabled_multiple_selection: false,
                selected_items: AHashSet::new(),
                range_anchor: None,
                range_base: AHashSet::new(),
                readonly,
                server_state,
                menu_row: Default::default(),
            };
            let state = cx.new(|cx| ListState::new(delegate, window, cx));
            list = Some(state.clone());
            let view = cx.new(|_| RowsHarness { list: state });
            Root::new(view, window, cx)
        });
        (handle.into(), list.expect("the window built the list"), dispatched)
    }

    /// What the right-click menu of `row` offers, as the actions its items
    /// send, top to bottom: each item clicked in a menu opened afresh.
    fn menu_actions(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        row: usize,
        dispatched: &Dispatched,
    ) -> Vec<KeyTreeAction> {
        dispatched.borrow_mut().clear();
        for item in 0usize..20 {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.right_click(IndexPath::new(row), cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
            let offered = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let offered = window.within("popup-menu").try_find(item).is_some();
                    if offered {
                        window.within("popup-menu").click(item, cx);
                    } else {
                        window.press("escape", cx);
                    }
                    offered
                })
                .expect("the window is open");
            cx.run_until_parked();
            if !offered {
                return dispatched.borrow().clone();
            }
            assert_eq!(
                dispatched.borrow().len(),
                item + 1,
                "item {item} of row {row}'s menu sent one action"
            );
        }
        panic!("row {row}'s menu kept offering items");
    }

    fn pick(list: &Entity<ListState<KeyTreeDelegate>>, ids: &[&str], cx: &mut TestAppContext) {
        list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.clear_selection();
            for (row, id) in ids.iter().enumerate() {
                delegate.set_pick((*id).into(), true, row);
            }
            cx.notify();
        });
    }

    /// What a row's right-click menu offers, by the actions its items send:
    /// a key, a key that never expires, a folder, two keys picked and one,
    /// and the same rows on a read-only connection.
    #[gpui::test]
    fn a_rows_menu_offers_what_the_row_and_the_access_allow(cx: &mut TestAppContext) {
        use KeyTreeAction as A;
        init_ui(cx);
        let rows = || {
            vec![
                leaf("a"),
                KeyTreeItem {
                    ttl_secs: Some(-1),
                    ..leaf("p")
                },
                folder("f"),
                leaf("b"),
            ]
        };
        let id = |id: &str| SharedString::from(id);

        let (handle, list, dispatched) = open_rows(cx, rows(), false);
        assert_eq!(
            menu_actions(cx, handle, 0, &dispatched),
            [
                A::DeleteKey(id("a")),
                A::SetTtlKey(id("a")),
                A::PersistKey(id("a")),
                A::ExportKey(id("a")),
                A::EditKeyTag(id("a")),
                A::CopyKeyName(id("a")),
                A::RenameKey(id("a")),
                A::ToggleFavoriteKey(id("a")),
            ]
        );
        // A key known not to expire has no TTL to remove.
        assert!(!menu_actions(cx, handle, 1, &dispatched).contains(&A::PersistKey(id("p"))));
        assert_eq!(
            menu_actions(cx, handle, 2, &dispatched),
            [
                A::RefreshFolder(id("f")),
                A::DeleteFolder(id("f")),
                A::SetTtlFolder(id("f")),
                A::PersistFolder(id("f")),
                A::ExportFolder(id("f")),
                A::CopyFolderPrefix(id("f")),
            ]
        );
        pick(&list, &["a", "b"], cx);
        assert_eq!(
            menu_actions(cx, handle, 0, &dispatched),
            [
                A::DeleteMultipleKeys,
                A::SetTtlMultipleKeys,
                A::PersistMultipleKeys,
                A::ExportSelectedKeys,
                A::BatchTagSelectedKeys,
            ]
        );
        // One pick: the key's own operations, and the export of the
        // selection rather than of the key.
        pick(&list, &["a"], cx);
        assert_eq!(
            menu_actions(cx, handle, 0, &dispatched),
            [
                A::DeleteKey(id("a")),
                A::SetTtlKey(id("a")),
                A::PersistKey(id("a")),
                A::ExportSelectedKeys,
            ]
        );
        // A row outside the picks has its own menu.
        assert_eq!(menu_actions(cx, handle, 3, &dispatched)[0], A::DeleteKey(id("b")));

        let (handle, _, dispatched) = open_rows(cx, rows(), true);
        assert_eq!(
            menu_actions(cx, handle, 0, &dispatched),
            [
                A::ExportKey(id("a")),
                A::EditKeyTag(id("a")),
                A::CopyKeyName(id("a")),
                A::ToggleFavoriteKey(id("a")),
            ]
        );
        assert_eq!(
            menu_actions(cx, handle, 2, &dispatched),
            [
                A::RefreshFolder(id("f")),
                A::ExportFolder(id("f")),
                A::CopyFolderPrefix(id("f")),
            ]
        );
    }

    /// The inline delete: a writable key row's, shown while the row is
    /// hovered — or, on a row with a TTL, while the TTL is — and sending
    /// the menu's `DeleteKey`. Never a folder's, never on a read-only
    /// connection, never beside the selection checkbox (#196).
    #[gpui::test]
    fn a_key_row_offers_its_delete_on_hover(cx: &mut TestAppContext) {
        init_ui(cx);
        let rows = || {
            vec![
                leaf("a"),
                KeyTreeItem {
                    ttl_secs: Some(30),
                    ..leaf("t")
                },
                folder("f"),
            ]
        };
        let (handle, list, dispatched) = open_rows(cx, rows(), false);
        // The TTL column is a setting the server state resolves on select;
        // an unknown server takes the global one, which is on.
        let server_state = list.read_with(cx, |state, _| state.delegate().server_state.clone());
        server_state.update(cx, |state, cx| state.select("scratch".into(), 0, cx));
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("ktree-del", 2usize)).is_none(), "a folder's");
            window.hover(IndexPath::new(2), cx);
            assert!(!window.find(("ktree-del", 0usize)).visible(), "another row hovered");
            window.hover(IndexPath::new(0), cx);
            assert!(window.find(("ktree-del", 0usize)).visible(), "the row hovered");
            // The TTL row keeps its TTL while the row is hovered and gives
            // the place to the delete only while the TTL is.
            window.hover(IndexPath::new(1), cx);
            assert!(!window.find(("ktree-del", 1usize)).visible(), "the TTL row hovered");
            // The delete is hidden there, so the pointer goes to its place
            // by hand: what is hovered is the TTL under it.
            let slot = window.find(("ktree-del", 1usize)).bounds().center();
            window.pointer_move(slot, None, cx);
            assert!(window.find(("ktree-del", 1usize)).visible(), "the TTL hovered");
            window.click(("ktree-del", 1usize), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(*dispatched.borrow(), [KeyTreeAction::DeleteKey("t".into())]);

        pick(&list, &["a"], cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("ktree-del", 0usize)).is_none(), "beside the checkbox");
        })
        .expect("the window is open");

        let (handle, _, _) = open_rows(cx, rows(), true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("ktree-del", 0usize)).is_none(), "read-only");
        })
        .expect("the window is open");
    }
}
