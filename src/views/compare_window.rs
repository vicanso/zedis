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

//! Compare the keys under a prefix on this connection with the same prefix
//! on another server / db: what is missing on either side, and what is on
//! both but different. The comparison itself is `compare_prefix`; this
//! window is the form, the progress, the three lists, and the hand-off to
//! the copy window for what the target lacks.

use crate::connection::{
    CompareOptions, CompareProgress, CompareReport, CompareSide, CompareStage, ConflictMode, KeyDifference,
    compare_prefix, get_servers,
};
use crate::error::Error;
use crate::helpers::{get_mono_font_family, with_app_identity};
use crate::states::{ZedisGlobalStore, i18n_common, i18n_compare, i18n_copy};
use crate::views::{CopyPreset, open_migration_copy_window};
use gpui::{
    App, Bounds, ClipboardItem, Entity, FocusHandle, Focusable, KeyDownEvent, SharedString, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Root, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    notification::Notification,
    spinner::Spinner,
    table::{DataTable, TableState},
    v_flex,
};
use rust_i18n::t;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use zedis_ui::{TextColumn, ZedisTextTable};

const WINDOW_WIDTH: f32 = 920.0;
const WINDOW_HEIGHT: f32 = 660.0;
/// Keys scanned per side before the report is marked partial.
const DEFAULT_LIMIT: usize = 10_000;
const KEY_WIDTH: f32 = 430.0;
const TYPE_WIDTH: f32 = 110.0;
const DIFFERENCE_WIDTH: f32 = 280.0;

/// The three lists a comparison produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    OnlySource,
    OnlyTarget,
    Differing,
}

impl ResultTab {
    const ALL: [ResultTab; 3] = [ResultTab::OnlySource, ResultTab::OnlyTarget, ResultTab::Differing];

    fn index(self) -> usize {
        match self {
            ResultTab::OnlySource => 0,
            ResultTab::OnlyTarget => 1,
            ResultTab::Differing => 2,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            ResultTab::OnlySource => "tab_only_source",
            ResultTab::OnlyTarget => "tab_only_target",
            ResultTab::Differing => "tab_differing",
        }
    }
}

#[derive(Default)]
enum RunState {
    #[default]
    Idle,
    Running,
    Done,
    Failed(SharedString),
}

pub struct ZedisCompareWindow {
    focus_handle: FocusHandle,
    source_id: SharedString,
    source_name: SharedString,
    source_db: usize,
    /// `(id, name)` of every configured server — the targets.
    servers: Vec<(SharedString, SharedString)>,
    target_server_id: Option<SharedString>,
    target_db_input: Entity<InputState>,
    prefix_input: Entity<InputState>,
    limit_input: Entity<InputState>,
    keyword_input: Entity<InputState>,
    tab: ResultTab,
    state: RunState,
    progress: CompareProgress,
    report: Option<CompareReport>,
    cancel: Arc<AtomicBool>,
    /// One table per list, refilled by every run.
    tables: [Entity<TableState<ZedisTextTable>>; 3],
    _run_task: Option<Task<()>>,
    _progress_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl ZedisCompareWindow {
    pub fn new(
        source_id: SharedString,
        source_name: SharedString,
        source_db: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id.into(), s.name.into()))
            .collect();
        let target_server_id = servers
            .iter()
            .find(|(id, _)| *id != source_id)
            .or_else(|| servers.first())
            .map(|(id, _)| id.clone());
        let target_db_input = cx.new(|cx| InputState::new(window, cx).default_value(source_db.to_string()));
        let prefix_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_compare(cx, "prefix_placeholder"))
        });
        let limit_input = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_LIMIT.to_string()));
        let keyword_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_compare(cx, "filter_placeholder"))
        });
        let key_title = i18n_compare(cx, "col_key");
        let type_title = i18n_compare(cx, "col_type");
        let difference_title = i18n_compare(cx, "col_difference");
        let copied = i18n_common(cx, "copied_to_clipboard");
        let copy_tooltip = i18n_common(cx, "copy_cell_tooltip");
        let pair_columns = || {
            vec![
                TextColumn::new("key", key_title.clone(), KEY_WIDTH).sortable(),
                TextColumn::new("type", type_title.clone(), TYPE_WIDTH).sortable(),
            ]
        };
        let mut differing_columns = pair_columns();
        differing_columns.push(TextColumn::new("difference", difference_title, DIFFERENCE_WIDTH).sortable());
        let tables = [
            new_table(pair_columns(), &copied, &copy_tooltip, window, cx),
            new_table(pair_columns(), &copied, &copy_tooltip, window, cx),
            new_table(differing_columns, &copied, &copy_tooltip, window, cx),
        ];
        let mut subs = Vec::new();
        for input in [&target_db_input, &prefix_input, &limit_input] {
            subs.push(cx.subscribe_in(input, window, |_view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }));
        }
        subs.push(
            cx.subscribe_in(&keyword_input, window, |view, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    let keyword = state.read(cx).value().to_string();
                    for table in &view.tables {
                        table.update(cx, |state, _| state.delegate_mut().set_filter(&keyword));
                    }
                    cx.notify();
                }
            }),
        );
        Self {
            focus_handle,
            source_id,
            source_name,
            source_db,
            servers,
            target_server_id,
            target_db_input,
            prefix_input,
            limit_input,
            keyword_input,
            tab: ResultTab::OnlySource,
            state: RunState::Idle,
            progress: CompareProgress::default(),
            report: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tables,
            _run_task: None,
            _progress_task: None,
            _subs: subs,
        }
    }

    fn target_db(&self, cx: &App) -> usize {
        self.target_db_input.read(cx).value().trim().parse().unwrap_or(0)
    }

    fn target_is_source(&self, cx: &App) -> bool {
        self.target_server_id.as_ref() == Some(&self.source_id) && self.target_db(cx) == self.source_db
    }

    fn is_running(&self) -> bool {
        matches!(self.state, RunState::Running)
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let Some(target_id) = self.target_server_id.clone() else {
            self.state = RunState::Failed(i18n_compare(cx, "no_target"));
            cx.notify();
            return;
        };
        if self.target_is_source(cx) {
            self.state = RunState::Failed(i18n_compare(cx, "same_target"));
            cx.notify();
            return;
        }
        let prefix = self.prefix_input.read(cx).value().trim().to_string();
        let limit: usize = self
            .limit_input
            .read(cx)
            .value()
            .trim()
            .parse()
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_LIMIT);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.report = None;
        self.progress = CompareProgress::default();
        self.state = RunState::Running;
        for table in &self.tables {
            table.update(cx, |state, _| state.delegate_mut().clear());
        }
        cx.notify();

        let source = CompareSide {
            server_id: self.source_id.to_string(),
            db: self.source_db,
        };
        let target = CompareSide {
            server_id: target_id.to_string(),
            db: self.target_db(cx),
        };
        let options = CompareOptions { prefix, limit };
        // Progress crosses from the background task on a channel; a
        // foreground task folds it into the view.
        let (tx, rx) = smol::channel::unbounded::<CompareProgress>();
        let task = cx.background_spawn(async move {
            compare_prefix(&source, &target, &options, &cancel, move |progress| {
                let _ = tx.try_send(progress);
            })
            .await
        });
        self._progress_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(progress) = rx.recv().await {
                let alive = this.update(cx, |view, cx| {
                    view.progress = progress;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        self._run_task = Some(cx.spawn(async move |this, cx| {
            let result: Result<CompareReport, Error> = task.await.map_err(Into::into);
            let _ = this.update(cx, |view, cx| view.finish(result, cx));
        }));
    }

    fn finish(&mut self, result: Result<CompareReport, Error>, cx: &mut Context<Self>) {
        match result {
            Ok(report) => {
                self.fill_tables(&report, cx);
                self.report = Some(report);
                self.state = RunState::Done;
            }
            Err(e) => {
                self.state = RunState::Failed(e.to_string().into());
            }
        }
        cx.notify();
    }

    fn fill_tables(&mut self, report: &CompareReport, cx: &mut Context<Self>) {
        let keyword = self.keyword_input.read(cx).value().to_string();
        let pair_rows = |pairs: &[(String, String)]| -> Vec<Vec<SharedString>> {
            pairs
                .iter()
                .map(|(key, key_type)| vec![key.clone().into(), key_type.clone().into()])
                .collect()
        };
        let differing_rows: Vec<Vec<SharedString>> = report
            .differing
            .iter()
            .map(|entry| {
                let difference: SharedString = match &entry.difference {
                    KeyDifference::Value => i18n_compare(cx, "diff_value"),
                    KeyDifference::Type { source, target } => {
                        format!("{}: {source} → {target}", i18n_compare(cx, "diff_type")).into()
                    }
                    KeyDifference::Unchecked => i18n_compare(cx, "diff_unchecked"),
                };
                vec![entry.key.clone().into(), entry.key_type.clone().into(), difference]
            })
            .collect();
        let rows = [
            pair_rows(&report.only_source),
            pair_rows(&report.only_target),
            differing_rows,
        ];
        for (table, rows) in self.tables.iter().zip(rows) {
            table.update(cx, |state, _| {
                let delegate = state.delegate_mut();
                delegate.set_rows(rows);
                delegate.set_filter(&keyword);
            });
        }
    }

    /// The keys the target lacks or holds differently — what a copy would
    /// fix. Keys that could not be compared are left alone.
    fn keys_for_target(&self) -> Vec<SharedString> {
        let Some(report) = &self.report else {
            return Vec::new();
        };
        report
            .only_source
            .iter()
            .map(|(key, _)| SharedString::from(key.clone()))
            .chain(
                report
                    .differing
                    .iter()
                    .filter(|entry| !matches!(entry.difference, KeyDifference::Unchecked))
                    .map(|entry| SharedString::from(entry.key.clone())),
            )
            .collect()
    }

    fn open_copy(&mut self, cx: &mut Context<Self>) {
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        let keys = self.keys_for_target();
        if keys.is_empty() {
            return;
        }
        open_migration_copy_window(
            self.source_id.clone(),
            self.source_name.clone(),
            self.source_db,
            keys,
            CopyPreset {
                target_id,
                target_db: self.target_db(cx),
                // The differing keys exist on the target: a copy that skipped
                // them would change nothing.
                conflict: ConflictMode::Overwrite,
            },
            cx,
        );
    }

    /// The visible keys of the open list, one per line.
    fn copy_key_names(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let table = &self.tables[self.tab.index()];
        let names: Vec<String> = table
            .read(cx)
            .delegate()
            .visible_rows()
            .into_iter()
            .filter_map(|row| row.first().map(|key| key.to_string()))
            .collect();
        if names.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(names.join("\n")));
        window.push_notification(Notification::success(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    fn status_text(&self, cx: &App) -> SharedString {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        match &self.state {
            RunState::Idle => i18n_compare(cx, "idle_hint"),
            RunState::Running => match self.progress.stage {
                CompareStage::ScanningSource => t!(
                    "compare.stage_scan_source",
                    count = self.progress.source_keys,
                    locale = &locale
                )
                .to_string()
                .into(),
                CompareStage::ScanningTarget => t!(
                    "compare.stage_scan_target",
                    count = self.progress.target_keys,
                    locale = &locale
                )
                .to_string()
                .into(),
                CompareStage::Comparing => t!(
                    "compare.stage_comparing",
                    done = self.progress.compared,
                    total = self.progress.to_compare,
                    locale = &locale
                )
                .to_string()
                .into(),
            },
            RunState::Done => {
                let Some(report) = &self.report else {
                    return SharedString::default();
                };
                let mut text = t!(
                    "compare.summary",
                    same = report.same,
                    only_source = report.only_source.len(),
                    only_target = report.only_target.len(),
                    differing = report.differing.len(),
                    locale = &locale
                )
                .to_string();
                if report.cancelled {
                    text.push_str(" · ");
                    text.push_str(&i18n_compare(cx, "cancelled"));
                }
                text.into()
            }
            RunState::Failed(message) => format!("{}: {message}", i18n_compare(cx, "failed")).into(),
        }
    }

    /// The scan hit the limit on a side: say which, so the lists are read as
    /// partial.
    fn capped_note(&self, cx: &App) -> Option<SharedString> {
        let report = self.report.as_ref()?;
        let side = match (report.source_capped, report.target_capped) {
            (false, false) => return None,
            (true, false) => i18n_compare(cx, "side_source"),
            (false, true) => i18n_compare(cx, "side_target"),
            (true, true) => i18n_compare(cx, "side_both"),
        };
        Some(i18n_compare(cx, "capped_note").replace("{side}", &side).into())
    }

    fn handle_close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }
}

impl Focusable for ZedisCompareWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZedisCompareWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(font_size);
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let is_running = self.is_running();
        let target_is_source = self.target_is_source(cx);
        let can_run = !is_running && self.target_server_id.is_some() && !target_is_source;
        let counts = self.report.as_ref().map(|report| {
            [
                report.only_source.len(),
                report.only_target.len(),
                report.differing.len(),
            ]
        });
        let copy_keys = self.keys_for_target().len();

        let source_line: SharedString = i18n_compare(cx, "source_summary")
            .replace("{server}", &self.source_name)
            .replace("{db}", &self.source_db.to_string())
            .into();
        let header = div()
            .px_6()
            .pt_6()
            .child(
                Label::new(i18n_compare(cx, "title"))
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD),
            )
            .child(div().pt_1().child(Label::new(source_line).text_sm().text_color(muted)));

        let selected = self.target_server_id.clone();
        let mut server_row = h_flex().gap_2().flex_wrap();
        for (id, name) in &self.servers {
            let is_selected = selected.as_ref() == Some(id);
            let id_click = id.clone();
            let button = Button::new(SharedString::from(format!("compare-target-{id}")))
                .small()
                .label(name.clone())
                .disabled(is_running);
            let button = if is_selected {
                button.primary()
            } else {
                button.outline()
            };
            server_row = server_row.child(button.on_click(cx.listener(move |this, _, _window, cx| {
                this.target_server_id = Some(id_click.clone());
                cx.notify();
            })));
        }
        let form = v_flex()
            .px_6()
            .pt_4()
            .gap_2()
            .child(Label::new(i18n_copy(cx, "target_server")).text_sm())
            .child(server_row)
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(i18n_copy(cx, "target_db")).text_sm())
                            .child(
                                Input::new(&self.target_db_input)
                                    .small()
                                    .w(px(80.))
                                    .disabled(is_running),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(i18n_compare(cx, "prefix_label")).text_sm())
                            .child(Input::new(&self.prefix_input).small().w(px(260.)).disabled(is_running)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(i18n_compare(cx, "limit_label")).text_sm())
                            .child(Input::new(&self.limit_input).small().w(px(100.)).disabled(is_running)),
                    )
                    .child(
                        Button::new("compare-run")
                            .primary()
                            .small()
                            .disabled(!can_run)
                            .label(i18n_compare(cx, "run"))
                            .on_click(cx.listener(|this, _, _window, cx| this.run(cx))),
                    ),
            )
            .when(target_is_source, |this| {
                this.child(
                    Label::new(i18n_compare(cx, "same_target"))
                        .text_xs()
                        .text_color(theme.yellow),
                )
            });

        let status = h_flex()
            .px_6()
            .pt_3()
            .gap_2()
            .items_center()
            .when(is_running, |this| {
                this.child(Spinner::new().with_size(px(14.)).color(muted))
            })
            .child(Label::new(self.status_text(cx)).text_sm().text_color(muted));
        let capped = self.capped_note(cx).map(|note| {
            div()
                .px_6()
                .pt_1()
                .child(Label::new(note).text_xs().text_color(theme.yellow))
        });

        let tabs = h_flex()
            .px_6()
            .pt_3()
            .gap_3()
            .items_center()
            .child(
                ButtonGroup::new("compare-tabs")
                    .compact()
                    .small()
                    .outline()
                    .children(ResultTab::ALL.into_iter().map(|tab| {
                        let count = counts.map(|counts| counts[tab.index()]);
                        let label = match count {
                            Some(count) => format!("{} ({count})", i18n_compare(cx, tab.label_key())),
                            None => i18n_compare(cx, tab.label_key()).to_string(),
                        };
                        Button::new(("compare-tab", tab.index()))
                            .label(label)
                            .selected(self.tab == tab)
                    }))
                    .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                        if let Some(tab) = clicks.first().and_then(|ix| ResultTab::ALL.get(*ix)) {
                            this.tab = *tab;
                            cx.notify();
                        }
                    })),
            )
            .child(Input::new(&self.keyword_input).small().w(px(220.)));

        let table = self.tables[self.tab.index()].clone();
        let list = v_flex()
            .px_6()
            .pt_2()
            .flex_1()
            .min_h_0()
            .w_full()
            .font_family(get_mono_font_family())
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(4.))
                    .child(DataTable::new(&table).bordered(false)),
            );

        let footer = h_flex()
            .gap_2()
            .justify_end()
            .px_6()
            .py_4()
            .child(
                Button::new("compare-close")
                    .ghost()
                    .label(i18n_common(cx, "close"))
                    .on_click(cx.listener(|this, _, window, cx| this.handle_close(window, cx))),
            )
            .when(is_running, |this| {
                this.child(
                    Button::new("compare-cancel")
                        .danger()
                        .label(i18n_common(cx, "cancel"))
                        .on_click(cx.listener(|this, _, _window, _cx| {
                            this.cancel.store(true, Ordering::Release);
                        })),
                )
            })
            .when(!is_running && self.report.is_some(), |this| {
                this.child(
                    Button::new("compare-copy-names")
                        .outline()
                        .label(i18n_compare(cx, "copy_names"))
                        .on_click(cx.listener(|this, _, window, cx| this.copy_key_names(window, cx))),
                )
                .child(
                    Button::new("compare-copy-to-target")
                        .primary()
                        .disabled(copy_keys == 0)
                        .label(format!("{} ({copy_keys})", i18n_compare(cx, "copy_to_target")))
                        .tooltip(i18n_compare(cx, "copy_to_target_hint"))
                        .on_click(cx.listener(|this, _, _window, cx| this.open_copy(cx))),
                )
            });

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|_this, event: &KeyDownEvent, window, _cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(form)
                    .child(status)
                    .children(capped)
                    .child(tabs)
                    .child(list)
                    .child(footer),
            )
    }
}

fn new_table(
    columns: Vec<TextColumn>,
    copied: &SharedString,
    copy_tooltip: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisCompareWindow>,
) -> Entity<TableState<ZedisTextTable>> {
    let table = ZedisTextTable::new(columns, copied.clone())
        .copy_tooltip(copy_tooltip.clone())
        .filter_columns(&["key"]);
    cx.new(|cx| TableState::new(table, window, cx))
}

/// Opens the compare window with this server / db as the source.
pub fn open_compare_window(server_id: SharedString, server_name: SharedString, db: usize, cx: &mut App) {
    let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
    let title = i18n_compare(cx, "title");
    let options = with_app_identity(WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..Default::default()
        }),
        is_resizable: true,
        focus: true,
        ..Default::default()
    });
    let _ = cx.open_window(options, move |window, cx| {
        let view = cx.new(|cx| ZedisCompareWindow::new(server_id.clone(), server_name.clone(), db, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
}
