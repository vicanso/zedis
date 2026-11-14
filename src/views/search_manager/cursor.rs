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

//! Server-side cursor pagination (`FT.AGGREGATE … WITHCURSOR`, `FT.CURSOR READ/DEL`)
//! and aggregate result rendering for [`ZedisSearchManager`].

use super::*;
use crate::connection::{AggregateCursorResult, ft_aggregate_cursor, ft_cursor_del, ft_cursor_read};
use gpui_kit::component::checkbox::Checkbox;

impl ZedisSearchManager {
    /// Release any open server-side `FT.CURSOR` on the background pool.
    pub(super) fn close_active_cursor(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((index, cursor_id)) = self.active_cursor.take() else {
            return;
        };
        if cursor_id == 0 {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        cx.background_spawn(async move {
            let at = ServerDb::new(&server_id, db);
            let _ = ft_cursor_del(&at, index.as_ref(), cursor_id).await;
        })
        .detach();
        cx.notify();
    }

    pub(super) fn run_aggregate_with_cursor(
        &mut self,
        at: ServerDb,
        index: SharedString,
        query: String,
        mut opts: AggregateOptions,
        read_count: u32,
        cx: &mut gpui::Context<Self>,
    ) {
        opts.limit = None;
        let index_for_task = index.clone();
        self._query_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                ft_aggregate_cursor(&at, index_for_task.as_ref(), &query, &opts, read_count.max(1)).await
            });
            let result: Result<AggregateCursorResult> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running_query = false;
                match result {
                    Ok(cursor_res) => {
                        this.active_cursor = (cursor_res.cursor_id != 0).then_some((index, cursor_res.cursor_id));
                        this.last_result = Some(LastResult::Aggregate(cursor_res.result));
                        this.error = None;
                    }
                    Err(e) => {
                        this.active_cursor = None;
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Fetch the next batch from an open `FT.CURSOR` and append its rows.
    pub(super) fn read_next_cursor_page(&mut self, cx: &mut gpui::Context<Self>) {
        if self.running_query {
            return;
        }
        let Some((index, cursor_id)) = self.active_cursor.clone() else {
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let read_count = parse_u32(&self.limit_count_input.read(cx).value())
            .unwrap_or(DEFAULT_LIMIT_COUNT)
            .max(1);
        self.running_query = true;
        cx.notify();

        let index_for_task = index.clone();
        self._query_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let at = ServerDb::new(&*server_id, db);
                ft_cursor_read(&at, index_for_task.as_ref(), cursor_id, read_count).await
            });
            let result: Result<AggregateCursorResult> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running_query = false;
                match result {
                    Ok(next) => {
                        this.active_cursor = (next.cursor_id != 0).then_some((index, next.cursor_id));
                        if let Some(LastResult::Aggregate(existing)) = this.last_result.as_mut() {
                            existing.rows.extend(next.result.rows);
                            if next.result.total > existing.total {
                                existing.total = next.result.total;
                            }
                        } else {
                            this.last_result = Some(LastResult::Aggregate(next.result));
                        }
                        this.error = None;
                    }
                    Err(e) => {
                        this.active_cursor = None;
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn render_cursor_checkbox(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Checkbox::new("search-aggregate-use-cursor")
            .checked(self.use_cursor)
            .label(i18n_search(cx, "use_cursor"))
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.use_cursor = *checked;
                if !this.use_cursor {
                    this.close_active_cursor(cx);
                }
                cx.notify();
            }))
    }

    pub(super) fn render_aggregate_result(&self, r: AggregateResult, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let offset = parse_u32(&self.limit_offset_input.read(cx).value()).unwrap_or(0);
        let count = parse_u32(&self.limit_count_input.read(cx).value())
            .unwrap_or(DEFAULT_LIMIT_COUNT)
            .max(1);
        let can_prev = offset > 0;
        let can_next = r.rows.len() as u32 >= count;
        let server_total = (r.total > r.rows.len() as u64).then_some(r.total);
        let cursor_id = self.active_cursor.as_ref().map(|(_, id)| *id).filter(|id| *id != 0);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(r.rows.len());
        for row in &r.rows {
            let mut cells: Vec<gpui::AnyElement> = Vec::with_capacity(row.len());
            for (k, v) in row {
                cells.push(
                    h_flex()
                        .gap_1()
                        .child(Label::new(k.clone()).text_xs().text_color(muted))
                        .child(Label::new(v.clone()).text_sm())
                        .into_any_element(),
                );
            }
            rows.push(
                h_flex()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(cells)
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .items_center()
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Label::new(format!("{} {}", i18n_search(cx, "rows_label"), r.rows.len()))
                            .text_xs()
                            .text_color(muted),
                    )
                    .when_some(server_total, |this, total| {
                        this.child(
                            Label::new(format!("{} {}", i18n_search(cx, "total_label"), total))
                                .text_xs()
                                .text_color(muted),
                        )
                    })
                    .when_some(cursor_id, |this, cid| {
                        let chip: SharedString = t!("search.cursor_active", id = cid, locale = &locale)
                            .to_string()
                            .into();
                        this.child(Label::new(chip).text_xs().text_color(cx.theme().primary))
                    })
                    .child(div().flex_1())
                    .when(cursor_id.is_some(), |this| {
                        this.child(
                            Button::new("aggregate-cursor-next")
                                .primary()
                                .xsmall()
                                .label(i18n_search(cx, "cursor_next"))
                                .disabled(self.running_query)
                                .on_click(cx.listener(|this, _, _w, cx| this.read_next_cursor_page(cx))),
                        )
                        .child(
                            Button::new("aggregate-cursor-close")
                                .outline()
                                .xsmall()
                                .label(i18n_search(cx, "cursor_close"))
                                .disabled(self.running_query)
                                .on_click(cx.listener(|this, _, _w, cx| this.close_active_cursor(cx))),
                        )
                    })
                    .when(!self.use_cursor && cursor_id.is_none(), |this| {
                        this.child(
                            Button::new("aggregate-page-prev")
                                .outline()
                                .xsmall()
                                .label(i18n_search(cx, "page_prev"))
                                .disabled(!can_prev || self.running_query)
                                .on_click(cx.listener(|this, _, window, cx| this.page_by(-1, window, cx))),
                        )
                        .child(
                            Button::new("aggregate-page-next")
                                .outline()
                                .xsmall()
                                .label(i18n_search(cx, "page_next"))
                                .disabled(!can_next || self.running_query)
                                .on_click(cx.listener(|this, _, window, cx| this.page_by(1, window, cx))),
                        )
                    }),
            )
            .children(rows)
    }
}
