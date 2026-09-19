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

//! Cluster mode, the Load tab: per-master load, polled while the tab shows.
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    pub(super) fn ensure_load_poll(&mut self, cx: &mut Context<Self>) {
        // Poll only while the Load tab is actually showing — sampling every
        // master on an interval for a hidden heatmap is wasted traffic. The
        // task is dropped (cancelled) in `set_cluster_tab` when the user
        // switches away, and re-created here on return.
        if self.cluster_tab != ClusterTab::Load {
            return;
        }
        if self.load_poll_task.is_some() {
            return;
        }
        self.load_poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let masters = match this.update(cx, |this, cx| {
                    if this.mode != TopologyMode::Cluster {
                        return None;
                    }
                    let desc = this.server_state.read(cx).nodes_description();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    if server_id.is_empty() {
                        return None;
                    }
                    let masters: Vec<(String, String, u32, usize)> = desc
                        .slot_map
                        .masters
                        .iter()
                        .map(|m| (m.node_id.to_string(), m.addr.to_string(), m.slot_count, m.color_index))
                        .collect();
                    Some((server_id, masters))
                }) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        cx.background_executor().timer(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(_) => break,
                };

                let result = if masters.1.is_empty() {
                    Ok(Vec::new())
                } else {
                    fetch_cluster_node_loads(&masters.0, &masters.1).await
                };

                if this
                    .update(cx, |this, cx| {
                        match result {
                            Ok(loads) => {
                                this.node_loads = loads;
                                this.load_error = None;
                            }
                            Err(e) => {
                                this.load_error = Some(e.to_string().into());
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }

    pub(super) fn render_load_tab(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let metric_row = h_flex()
            .gap_1()
            .child(
                Button::new("load-mem")
                    .when(self.load_metric == LoadMetric::Memory, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Memory, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_mem"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Memory;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("load-ops")
                    .when(self.load_metric == LoadMetric::Ops, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Ops, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_ops"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Ops;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("load-clients")
                    .when(self.load_metric == LoadMetric::Clients, |b| b.primary())
                    .when(self.load_metric != LoadMetric::Clients, |b| b.ghost())
                    .small()
                    .label(i18n_topology(cx, "load_metric_clients"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_metric = LoadMetric::Clients;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("load-to-reshard")
                    .ghost()
                    .small()
                    .label(i18n_topology(cx, "goto_reshard"))
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.set_cluster_tab(ClusterTab::Reshard, cx);
                    })),
            );

        if let Some(err) = &self.load_error {
            return v_flex()
                .gap_2()
                .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
                .child(metric_row)
                .child(Label::new(err.clone()).text_color(cx.theme().danger))
                .into_any_element();
        }

        if self.node_loads.is_empty() {
            return v_flex()
                .gap_2()
                .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
                .child(metric_row)
                .child(Label::new(i18n_topology(cx, "load_refreshing")).text_color(muted))
                .into_any_element();
        }

        let max_val = self
            .node_loads
            .iter()
            .map(|n| match self.load_metric {
                LoadMetric::Memory => n.used_memory,
                LoadMetric::Ops => n.ops_per_sec,
                LoadMetric::Clients => n.connected_clients,
            })
            .max()
            .unwrap_or(1)
            .max(1);

        let mut cards: Vec<gpui::AnyElement> = Vec::new();
        for n in &self.node_loads {
            let value = match self.load_metric {
                LoadMetric::Memory => n.used_memory,
                LoadMetric::Ops => n.ops_per_sec,
                LoadMetric::Clients => n.connected_clients,
            };
            let ratio = value as f32 / max_val as f32;
            let heat = heat_color(ratio);
            let value_label = match self.load_metric {
                LoadMetric::Memory => humansize::format_size(n.used_memory, humansize::DECIMAL),
                LoadMetric::Ops => format!("{} ops/s", n.ops_per_sec),
                LoadMetric::Clients => format!("{} clients", n.connected_clients),
            };
            let short_id = short_node_id(&n.node_id);
            let stripe = master_color(n.color_index);
            let slots_label =
                rust_i18n::t!("topology.slots_count_label", count = n.slot_count, locale = locale).to_string();
            let id_for_fill = n.node_id.clone();
            cards.push(
                v_flex()
                    .id(SharedString::from(format!("load-card-{}", n.node_id)))
                    .w(px(200.))
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .cursor_pointer()
                    .bg({
                        let mut c = heat;
                        c.a = 0.25;
                        c
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Click load card → target for reshard (heavier masters
                        // are the usual source; fill as source when above avg).
                        this.fill_reshard_source(id_for_fill.clone(), window, cx);
                        this.set_cluster_tab(ClusterTab::Reshard, cx);
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(stripe))
                            .child(Label::new(n.addr.clone()).font_semibold()),
                    )
                    .child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                    .child(Label::new(SharedString::from(value_label)).text_sm())
                    .child(Label::new(SharedString::from(slots_label)).text_xs().text_color(muted))
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
            .child(
                Label::new(i18n_topology(cx, "load_click_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(metric_row)
            .child(h_flex().gap_3().flex_wrap().children(cards))
            .into_any_element()
    }
}
