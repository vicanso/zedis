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

//! RedisBloom probabilistic-structure viewer.
//!
//! Rendered by [`crate::views::ZedisEditor`] for the five RedisBloom
//! module types — Bloom filter (`MBbloom--`), Cuckoo filter
//! (`MBbloomCF`), Count-Min Sketch (`CMSk-TYPE`), Top-K (`TopK-TYPE`)
//! and t-digest (`TDIS-TYPE`) — which previously fell back to the raw
//! bytes editor. Like the time series viewer it self-gates by the key's
//! TYPE (the module must be loaded for a key to resolve to one of these)
//! and owns its own fetch.
//!
//! Every kind renders its `*.INFO` reply as a stat table. Top-K also
//! lists its current top items (`TOPK.LIST … WITHCOUNT`) and t-digest
//! adds min / max / p50 / p90 / p99 (`TDIGEST.MIN` / `MAX` /
//! `QUANTILE`). The viewer is read-only.

use crate::helpers::get_mono_font_family;
use crate::{
    connection::{Capability, ProbInfo, ProbeOutcome, ServerDb, prob_info, prob_probe},
    states::{ProbKind, ZedisGlobalStore, ZedisServerState, i18n_probabilistic},
};
use gpui::{App, Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, StyledExt,
    button::Button,
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use rust_i18n::t;
use tracing::info;

/// i18n sub-key for the kind's display name.
fn kind_i18n_key(kind: ProbKind) -> &'static str {
    match kind {
        ProbKind::Bloom => "bloom",
        ProbKind::Cuckoo => "cuckoo",
        ProbKind::CountMinSketch => "count_min",
        ProbKind::TopK => "topk",
        ProbKind::TDigest => "tdigest",
    }
}

pub struct ZedisProbabilisticEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    kind: ProbKind,
    data: Option<ProbInfo>,
    error: Option<SharedString>,
    loading: bool,
    /// In-flight fetch; dropped (and thereby cancelled) when the editor
    /// is recreated for a new key.
    load_task: Option<Task<()>>,
    /// The probe box: a member name (Bloom/Cuckoo/CMS/Top-K) or a
    /// numeric sample value (t-digest).
    probe_input: Entity<InputState>,
    /// Last probe's localized answer; red when `probe_failed`.
    probe_result: Option<SharedString>,
    probe_failed: bool,
    probing: bool,
    probe_task: Option<Task<()>>,
}

impl ZedisProbabilisticEditor {
    pub fn new(
        server_state: Entity<ZedisServerState>,
        kind: ProbKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new probabilistic editor view");
        let placeholder_key = if kind == ProbKind::TDigest {
            "probe_placeholder_value"
        } else {
            "probe_placeholder_item"
        };
        let probe_input = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_probabilistic(cx, placeholder_key)));
        let mut this = Self {
            server_state,
            key,
            kind,
            data: None,
            error: None,
            loading: false,
            load_task: None,
            probe_input,
            probe_result: None,
            probe_failed: false,
            probing: false,
            probe_task: None,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        if key.is_empty() {
            return;
        }
        let kind = self.kind;
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = prob_info(&ServerDb::new(server_id, db), &key, kind).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(data) => {
                        this.data = Some(data);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Run the probe: `add == false` answers the structure's defining
    /// question (membership / count / CDF), `add == true` inserts the
    /// item. A successful add also reloads the stats above.
    fn run_probe(&mut self, add: bool, cx: &mut Context<Self>) {
        if self.probing {
            return;
        }
        let item = self.probe_input.read(cx).value().trim().to_string();
        if item.is_empty() {
            return;
        }
        // t-digest probes are numeric — validate before touching Redis so
        // the error is instant and precise.
        if self.kind == ProbKind::TDigest && item.parse::<f64>().is_err() {
            self.probe_result = Some(i18n_probabilistic(cx, "probe_need_number"));
            self.probe_failed = true;
            cx.notify();
            return;
        }
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        let kind = self.kind;
        self.probing = true;
        self.probe_failed = false;
        cx.notify();
        self.probe_task = Some(cx.spawn(async move |this, cx| {
            let result = prob_probe(&ServerDb::new(server_id, db), &key, kind, &item, add).await;
            let _ = this.update(cx, |this, cx| {
                this.probing = false;
                match result {
                    Ok(outcome) => {
                        this.probe_result = Some(probe_outcome_label(&outcome, cx));
                        this.probe_failed = false;
                        if add {
                            // The insert changed the structure's stats.
                            this.load(cx);
                        }
                    }
                    Err(e) => {
                        this.probe_result = Some(SharedString::from(e.to_string()));
                        this.probe_failed = true;
                    }
                }
                cx.notify();
            });
        }));
    }

    /// The probe box: input + Query (+ Add unless read-only) + the last
    /// answer. This is the panel's reason to exist — a probabilistic
    /// structure's whole point is answering "is X in it / how often".
    fn render_probe(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_add = self.server_state.read(cx).can(Capability::MutateContainer);
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Input::new(&self.probe_input).small().flex_1())
            .child(
                Button::new("prob-probe-query")
                    .small()
                    .outline()
                    .label(i18n_probabilistic(cx, "probe_query"))
                    .disabled(self.probing)
                    .on_click(cx.listener(|this, _, _w, cx| this.run_probe(false, cx))),
            );
        if can_add {
            row = row.child(
                Button::new("prob-probe-add")
                    .small()
                    .outline()
                    .label(i18n_probabilistic(cx, "probe_add"))
                    .disabled(self.probing)
                    .on_click(cx.listener(|this, _, _w, cx| this.run_probe(true, cx))),
            );
        }
        let mut col = v_flex().w_full().gap_1().child(row);
        if let Some(result) = self.probe_result.clone() {
            let color = if self.probe_failed { cx.theme().danger } else { muted };
            col = col.child(Label::new(result).text_xs().text_color(color).whitespace_normal());
        }
        col
    }

    fn render_info(&self, data: &ProbInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows = v_flex().w_full().gap_1();
        for (k, v) in data.info.iter() {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_baseline()
                    .child(
                        div()
                            .min_w(px(180.))
                            .child(Label::new(k.clone()).text_xs().text_color(muted)),
                    )
                    .child(Label::new(v.clone()).text_xs().font_semibold()),
            );
        }
        rows
    }

    fn render_top_items(&self, data: &ProbInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows = v_flex().w_full().gap_1().child(
            Label::new(i18n_probabilistic(cx, "top_items"))
                .text_sm()
                .font_semibold(),
        );
        for (rank, (item, count)) in data.top_items.iter().enumerate() {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_baseline()
                    .child(
                        div()
                            .min_w(px(28.))
                            .child(Label::new(format!("{}.", rank + 1)).text_xs().text_color(muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(item.clone()).text_xs().font_semibold()),
                    )
                    .child(Label::new(count.to_string()).text_xs().text_color(muted)),
            );
        }
        rows
    }

    fn render_quantiles(&self, data: &ProbInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut row = h_flex().w_full().flex_wrap().gap_x_4().gap_y_1().items_center().child(
            Label::new(i18n_probabilistic(cx, "quantiles"))
                .text_sm()
                .font_semibold(),
        );
        for (label, value) in data.quantiles.iter() {
            row = row.child(
                h_flex()
                    .gap_1()
                    .items_baseline()
                    .child(Label::new(*label).text_xs().text_color(muted))
                    .child(Label::new(format!("{value:.4}")).text_xs().font_semibold()),
            );
        }
        row
    }
}

impl Render for ZedisProbabilisticEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = i18n_probabilistic(cx, kind_i18n_key(self.kind));
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Label::new(title).font_semibold())
            .child(
                div()
                    .px_1p5()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .child(Label::new(self.kind.prefix()).text_xs().text_color(muted)),
            );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if let Some(data) = self.data.as_ref() {
            if data.info.is_empty() && data.top_items.is_empty() && data.quantiles.is_empty() {
                div()
                    .p_4()
                    .child(
                        Label::new(i18n_probabilistic(cx, "no_data"))
                            .text_sm()
                            .text_color(muted),
                    )
                    .into_any_element()
            } else {
                let mut col = v_flex().w_full().gap_4();
                col = col.child(self.render_info(data, cx));
                if !data.top_items.is_empty() {
                    col = col.child(self.render_top_items(data, cx));
                }
                if !data.quantiles.is_empty() {
                    col = col.child(self.render_quantiles(data, cx));
                }
                col.into_any_element()
            }
        } else {
            div()
                .p_4()
                .child(
                    Label::new(i18n_probabilistic(cx, "loading"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .gap_3()
            .p_3()
            .child(header)
            .child(self.render_probe(cx))
            .child(body)
    }
}

/// Localize a probe outcome — wording stays honest about the
/// probabilistic semantics (a Bloom/Cuckoo "yes" is only a maybe, a CMS
/// count only over-estimates).
fn probe_outcome_label(outcome: &ProbeOutcome, cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    match outcome {
        ProbeOutcome::MaybeExists => i18n_probabilistic(cx, "probe_maybe_exists"),
        ProbeOutcome::DefinitelyNot => i18n_probabilistic(cx, "probe_definitely_not"),
        ProbeOutcome::Count(count) => t!("probabilistic.probe_count", count = count, locale = locale)
            .to_string()
            .into(),
        ProbeOutcome::InTopK => i18n_probabilistic(cx, "probe_in_topk"),
        ProbeOutcome::NotInTopK => i18n_probabilistic(cx, "probe_not_in_topk"),
        ProbeOutcome::Cdf(fraction) => {
            let pct = (fraction * 100.0).clamp(0.0, 100.0);
            t!("probabilistic.probe_cdf", pct = format!("{pct:.1}"), locale = locale)
                .to_string()
                .into()
        }
        ProbeOutcome::Added => i18n_probabilistic(cx, "probe_added"),
        ProbeOutcome::AlreadyMaybe => i18n_probabilistic(cx, "probe_already"),
        ProbeOutcome::TopkDropped(item) => t!("probabilistic.probe_topk_dropped", item = item, locale = locale)
            .to_string()
            .into(),
    }
}
