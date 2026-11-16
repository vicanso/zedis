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
                    // A background tab or an unattended app skips the round,
                    // like every other poll (ADR 5).
                    if this.mode != TopologyMode::Cluster || this.server_state.read(cx).is_background() {
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
                        cx.background_executor().timer(pacing::CLUSTER_LOAD_POLL_INTERVAL).await;
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
                cx.background_executor().timer(pacing::CLUSTER_LOAD_POLL_INTERVAL).await;
            }
        }));
    }

    /// One metric button of the Load tab: filled while it colours the cards.
    fn load_metric_button(
        &self,
        id: &'static str,
        metric: LoadMetric,
        label: SharedString,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .outline()
            .selected(self.load_metric == metric)
            .small()
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.load_metric = metric;
                cx.notify();
            }))
    }

    pub(super) fn render_load_tab(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let metric_row = h_flex()
            .gap_1()
            .child(self.load_metric_button("load-mem", LoadMetric::Memory, i18n_topology(cx, "load_metric_mem"), cx))
            .child(self.load_metric_button("load-ops", LoadMetric::Ops, i18n_topology(cx, "load_metric_ops"), cx))
            .child(self.load_metric_button("load-keys", LoadMetric::Keys, i18n_topology(cx, "load_metric_keys"), cx))
            .child(self.load_metric_button(
                "load-clients",
                LoadMetric::Clients,
                i18n_topology(cx, "load_metric_clients"),
                cx,
            ))
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

        let metric_of = |n: &ClusterNodeLoad| match self.load_metric {
            LoadMetric::Memory => n.used_memory,
            LoadMetric::Ops => n.ops_per_sec,
            LoadMetric::Clients => n.connected_clients,
            LoadMetric::Keys => n.keys,
        };
        let max_val = self.node_loads.iter().map(metric_of).max().unwrap_or(1).max(1);
        let shares = cluster_shares(&self.node_loads);
        let any_skew = shares
            .iter()
            .any(|s| skewed(s.memory, s.slots) || skewed(s.ops, s.slots) || skewed(s.keys, s.slots));

        let mut cards: Vec<gpui::AnyElement> = Vec::new();
        for (n, share) in self.node_loads.iter().zip(&shares) {
            let ratio = metric_of(n) as f32 / max_val as f32;
            let heat = heat_color(ratio);
            let short_id = short_node_id(&n.node_id);
            let stripe = master_color(n.color_index);
            let id_for_fill = n.node_id.clone();
            let row = |label: SharedString, value: String, part: f32, flagged: bool| {
                let tone = if flagged { warning } else { muted };
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Label::new(label).text_xs().text_color(muted).w(px(56.)))
                    .child(
                        Label::new(value)
                            .text_xs()
                            .font_family(get_mono_font_family())
                            .flex_1()
                            .when(flagged, |l| l.text_color(warning)),
                    )
                    .child(
                        Label::new(format!("{:.0}%", part * 100.0))
                            .text_xs()
                            .font_family(get_mono_font_family())
                            .text_color(tone),
                    )
            };
            cards.push(
                v_flex()
                    .id(SharedString::from(format!("load-card-{}", n.node_id)))
                    .w(px(240.))
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
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
                    .child(
                        v_flex()
                            .pt_1()
                            .gap_0p5()
                            .child(row(
                                i18n_topology(cx, "load_metric_mem"),
                                humansize::format_size(n.used_memory, humansize::DECIMAL),
                                share.memory,
                                skewed(share.memory, share.slots),
                            ))
                            .child(row(
                                i18n_topology(cx, "load_metric_ops"),
                                group_thousands(n.ops_per_sec),
                                share.ops,
                                skewed(share.ops, share.slots),
                            ))
                            .child(row(
                                i18n_topology(cx, "load_metric_keys"),
                                group_thousands(n.keys),
                                share.keys,
                                skewed(share.keys, share.slots),
                            ))
                            .child(row(
                                i18n_topology(cx, "load_slots"),
                                n.slot_count.to_string(),
                                share.slots,
                                false,
                            ))
                            .child(row(
                                i18n_topology(cx, "load_metric_clients"),
                                group_thousands(n.connected_clients),
                                share.clients,
                                false,
                            )),
                    )
                    .into_any_element(),
            );
        }

        let total = rust_i18n::t!(
            "topology.load_total",
            memory = humansize::format_size(
                self.node_loads.iter().map(|n| n.used_memory).sum::<u64>(),
                humansize::DECIMAL
            ),
            ops = group_thousands(self.node_loads.iter().map(|n| n.ops_per_sec).sum()),
            keys = group_thousands(self.node_loads.iter().map(|n| n.keys).sum()),
            locale = locale
        )
        .to_string();

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "load_title")).font_semibold())
            .child(Label::new(total).text_sm())
            .child(
                Label::new(i18n_topology(cx, "load_click_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(metric_row)
            .when(any_skew, |this| {
                this.child(
                    Label::new(i18n_topology(cx, "load_skew_hint"))
                        .text_xs()
                        .text_color(warning),
                )
            })
            .child(h_flex().gap_3().flex_wrap().children(cards))
            .into_any_element()
    }
}

/// One master's share of the cluster, for what the Load cards compare.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shares {
    slots: f32,
    memory: f32,
    ops: f32,
    keys: f32,
    clients: f32,
}

fn share(part: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { part as f32 / total as f32 }
}

/// Each master's share of the slots, the memory, the commands, the keys and
/// the clients.
fn cluster_shares(loads: &[ClusterNodeLoad]) -> Vec<Shares> {
    let slots = loads.iter().map(|n| u64::from(n.slot_count)).sum();
    let memory = loads.iter().map(|n| n.used_memory).sum();
    let ops = loads.iter().map(|n| n.ops_per_sec).sum();
    let keys = loads.iter().map(|n| n.keys).sum();
    let clients = loads.iter().map(|n| n.connected_clients).sum();
    loads
        .iter()
        .map(|n| Shares {
            slots: share(u64::from(n.slot_count), slots),
            memory: share(n.used_memory, memory),
            ops: share(n.ops_per_sec, ops),
            keys: share(n.keys, keys),
            clients: share(n.connected_clients, clients),
        })
        .collect()
}

/// A share well above the master's share of the slots — what a big key, or
/// a hash tag that puts many keys on one slot, looks like: the slots are
/// spread one way and the data another. Half as much again and ten points
/// at least, so a few keys either way on a quiet cluster are not news.
fn skewed(share: f32, slot_share: f32) -> bool {
    share >= slot_share * 1.5 && share - slot_share >= 0.10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(slot_count: u32, used_memory: u64, ops_per_sec: u64, keys: u64) -> ClusterNodeLoad {
        ClusterNodeLoad {
            node_id: "n".into(),
            addr: "a".into(),
            used_memory,
            ops_per_sec,
            connected_clients: 1,
            keys,
            slot_count,
            color_index: 0,
        }
    }

    /// Three masters with a third of the slots each: the one holding most
    /// of the keys and the traffic stands out, the even ones do not.
    #[test]
    fn a_master_holding_more_than_its_slots_is_skewed() {
        let loads = [
            master(5461, 1_000, 10, 100),
            master(5462, 1_000, 10, 100),
            master(5461, 1_000, 980, 800),
        ];
        let shares = cluster_shares(&loads);
        assert!((shares[0].slots - 1.0 / 3.0).abs() < 0.001);
        assert!(!skewed(shares[0].keys, shares[0].slots));
        assert!(skewed(shares[2].keys, shares[2].slots), "{:?}", shares[2]);
        assert!(skewed(shares[2].ops, shares[2].slots), "{:?}", shares[2]);
        assert!(!skewed(shares[2].memory, shares[2].slots), "memory is even");
    }

    /// A quiet cluster is no news, and neither is one master on its own.
    #[test]
    fn nothing_is_skewed_with_no_traffic_or_a_single_master() {
        let quiet = cluster_shares(&[master(8192, 1_000, 0, 0), master(8192, 1_000, 0, 0)]);
        assert!(
            quiet
                .iter()
                .all(|s| !skewed(s.ops, s.slots) && !skewed(s.keys, s.slots))
        );
        let alone = cluster_shares(&[master(16384, 5_000, 300, 40)]);
        assert!(!skewed(alone[0].keys, alone[0].slots));
        // Slots moved off a master mid-reshard: its data share follows, and a
        // small difference is not flagged.
        assert!(!skewed(0.36, 0.30));
    }
}
