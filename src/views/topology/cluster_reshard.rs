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

//! Cluster mode, the Reshard tab: moving slot ranges between masters, and
//! the rebalance plan.
//!
//! Split out of `topology.rs`; the methods are the `ZedisTopology` ones they
//! always were.

use super::*;

impl ZedisTopology {
    pub(super) fn fill_reshard_source(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_error = None;
        self.reshard_source_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    pub(super) fn fill_reshard_target(&mut self, node_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_error = None;
        self.reshard_target_input.update(cx, |input, cx| {
            input.set_value(node_id, window, cx);
        });
        cx.notify();
    }

    pub(super) fn clear_reshard_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reshard_source_input.update(cx, |input, cx| {
            input.set_value(SharedString::default(), window, cx);
        });
        cx.notify();
    }

    pub(super) fn render_reshard_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let can_write = self.can_cluster_write(cx);
        let desc = self.server_state.read(cx).nodes_description();
        let selected_source = self.reshard_source_input.read(cx).value().to_string();
        let selected_target = self.reshard_target_input.read(cx).value().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        // Master pickers: each row is a master with Source / Target buttons.
        // Selected side is highlighted so the free-text fields stay as power-user
        // fallback while the common path is one click.
        let mut master_chips: Vec<gpui::AnyElement> = Vec::new();
        for m in &desc.slot_map.masters {
            let id = m.node_id.clone();
            let id_as_source = id.clone();
            let id_as_target = id.clone();
            let color = master_color(m.color_index);
            let is_src = !selected_source.is_empty() && selected_source == id;
            let is_tgt = !selected_target.is_empty() && selected_target == id;
            let short_id = short_node_id(&m.node_id);
            let slots_label =
                rust_i18n::t!("topology.slots_count_label", count = m.slot_count, locale = locale).to_string();
            master_chips.push(
                h_flex()
                    .id(SharedString::from(format!("reshard-chip-{}", m.node_id)))
                    .gap_1()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if is_src || is_tgt { primary } else { border })
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
                    .child(Label::new(m.addr.clone()).text_xs())
                    .child(
                        Label::new(SharedString::from(short_id))
                            .text_xs()
                            .text_color(muted)
                            .font_family(get_mono_font_family()),
                    )
                    .child(Label::new(SharedString::from(slots_label)).text_xs().text_color(muted))
                    .child(
                        Button::new(SharedString::from(format!("reshard-src-{}", m.node_id)))
                            .when(is_src, |b| b.primary())
                            .when(!is_src, |b| b.ghost())
                            .small()
                            .label(i18n_topology(cx, "reshard_as_source"))
                            .disabled(!can_write || self.reshard_running)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fill_reshard_source(id_as_source.clone().into(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("reshard-tgt-{}", m.node_id)))
                            .when(is_tgt, |b| b.primary())
                            .when(!is_tgt, |b| b.ghost())
                            .small()
                            .label(i18n_topology(cx, "reshard_as_target"))
                            .disabled(!can_write || self.reshard_running)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fill_reshard_target(id_as_target.clone().into(), window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }

        let plan_btn = Button::new("reshard-plan")
            .primary()
            .small()
            .label(i18n_topology(cx, "reshard_plan"))
            .disabled(!can_write || self.reshard_running)
            .on_click(cx.listener(|this, _, _window, cx| {
                this.run_plan(cx);
            }));

        let execute_btn = Button::new("reshard-exec")
            .danger()
            .small()
            .label(if self.reshard_running {
                i18n_topology(cx, "reshard_running")
            } else {
                i18n_topology(cx, "reshard_execute")
            })
            .disabled(!can_write || self.planned_slots.is_empty() || self.reshard_running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_reshard_dialog(window, cx);
            }));

        let clear_src_btn = Button::new("reshard-clear-src")
            .ghost()
            .small()
            .label(i18n_topology(cx, "reshard_clear_source"))
            .disabled(selected_source.trim().is_empty() || self.reshard_running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_reshard_source(window, cx);
            }));

        let preview: gpui::AnyElement = if self.reshard_running {
            // Live progress: "moving… N/M" plus a bar fed by the per-slot
            // ticks the reshard task reports through the server state.
            let progress = self.server_state.read(cx).reshard_progress();
            let (processed, total) = progress.unwrap_or((0, 0));
            let pct = if total > 0 {
                processed as f32 * 100.0 / total as f32
            } else {
                0.0
            };
            v_flex()
                .gap_1()
                .w(px(320.))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Label::new(i18n_topology(cx, "reshard_progress"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .when(total > 0, |this| {
                            this.child(Label::new(format!("{processed}/{total}")).text_xs().text_color(muted))
                        }),
                )
                .child(Progress::new("topology-reshard-progress").value(pct))
                .into_any_element()
        } else if let Some(err) = &self.plan_error {
            Label::new(err.clone()).text_color(cx.theme().danger).into_any_element()
        } else if self.planned_slots.is_empty() {
            Label::new(i18n_topology(cx, "reshard_pick_target"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            let n = self.planned_slots.len();
            let preview_slots = if n > 24 {
                let head: Vec<String> = self.planned_slots.iter().take(20).map(|s| s.to_string()).collect();
                format!("{} … (+{} more)", head.join(", "), n - 20)
            } else {
                self.planned_slots
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let title: SharedString = rust_i18n::t!("topology.reshard_will_move", count = n, locale = locale)
                .to_string()
                .into();
            v_flex()
                .gap_1()
                .child(Label::new(title).font_semibold())
                .child(
                    Label::new(SharedString::from(preview_slots))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element()
        };

        v_flex()
            .gap_3()
            .child(Label::new(i18n_topology(cx, "reshard_title")).font_semibold())
            .child(
                Label::new(i18n_topology(
                    cx,
                    if self.atomic_migration_supported(cx) {
                        "reshard_atomic_hint"
                    } else {
                        "reshard_hint"
                    },
                ))
                .text_xs()
                .text_color(muted)
                .whitespace_normal(),
            )
            .children(self.render_slot_migrations(cx))
            .child(self.render_rebalance_section(cx))
            .child(
                Label::new(i18n_topology(cx, "reshard_pick_masters"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(v_flex().gap_1().children(master_chips))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                Label::new(i18n_topology(cx, "reshard_source_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_source_input)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                Label::new(i18n_topology(cx, "reshard_target_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_target_input)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(120.))
                            .child(
                                Label::new(i18n_topology(cx, "reshard_count_field"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.reshard_count_input)),
                    ),
            )
            .child(h_flex().gap_2().child(plan_btn).child(execute_btn).child(clear_src_btn))
            .child(preview)
            .into_any_element()
    }

    /// Masters as `(node_id, ranges)` — what both planners take.
    pub(super) fn master_ranges(&self, cx: &Context<Self>) -> Vec<(String, Vec<(u16, u16)>)> {
        let desc = self.server_state.read(cx).nodes_description();
        let mut by_id: std::collections::HashMap<String, Vec<(u16, u16)>> = std::collections::HashMap::new();
        // Every master, including one holding no slots — a node that just
        // joined is exactly who a rebalance is for.
        for master in &desc.slot_map.masters {
            by_id.entry(master.node_id.to_string()).or_default();
        }
        for owner in &desc.slot_map.owners {
            by_id
                .entry(owner.node_id.to_string())
                .or_default()
                .push((owner.start, owner.end));
        }
        by_id.into_iter().collect()
    }

    pub(super) fn run_rebalance_plan(&mut self, cx: &mut Context<Self>) {
        self.rebalance_planned = true;
        match plan_cluster_rebalance_moves(&self.master_ranges(cx)) {
            Ok(moves) => {
                self.rebalance_plan = moves;
                self.plan_error = None;
            }
            Err(e) => {
                self.rebalance_plan.clear();
                self.plan_error = Some(SharedString::from(e));
            }
        }
        cx.notify();
    }

    /// Resolve the plan's node ids to addresses and run it.
    pub(super) fn open_rebalance_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rebalance_plan.is_empty() || self.reshard_running || !self.can_cluster_write(cx) {
            return;
        }
        let desc = self.server_state.read(cx).nodes_description();
        let addr_of = |node_id: &str| {
            desc.slot_map
                .masters
                .iter()
                .find(|master| master.node_id == node_id)
                .map(|master| master.addr.to_string())
        };
        let mut legs: Vec<RebalanceLeg> = Vec::new();
        for step in &self.rebalance_plan {
            let (Some(source_addr), Some(target_addr)) = (addr_of(&step.source_id), addr_of(&step.target_id)) else {
                self.plan_error = Some(i18n_topology(cx, "err_reshard_target_missing"));
                cx.notify();
                return;
            };
            legs.push(RebalanceLeg {
                source_addr,
                source_id: step.source_id.clone(),
                target_addr,
                target_id: step.target_id.clone(),
                slots: step.slots.clone(),
            });
        }
        let slots: usize = legs.iter().map(|leg| leg.slots.len()).sum();
        let atomic = self.atomic_migration_supported(cx);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body = rust_i18n::t!(
            "topology.rebalance_confirm_body",
            moves = legs.len(),
            slots = slots,
            locale = &locale
        )
        .to_string();
        let title = i18n_topology(cx, "rebalance_confirm_title");
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let entity = cx.entity().downgrade();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
            .on_ok(move |_, window, cx| {
                let legs = legs.clone();
                server_state.update(cx, |state, cx| state.cluster_rebalance(legs, atomic, cx));
                if let Some(this) = entity.upgrade() {
                    this.update(cx, |this, cx| {
                        this.reshard_running = !atomic;
                        this.rebalance_plan.clear();
                        this.rebalance_planned = false;
                        cx.notify();
                    });
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Even out the slots across every master in one step — the planner
    /// `redis-cli --cluster rebalance` implements, with the same 2%
    /// threshold, executed over whichever migration path this server has.
    pub(super) fn render_rebalance_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let (muted, border, radius) = (theme.muted_foreground, theme.border, theme.radius);
        let can_write = self.can_cluster_write(cx);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let slots: usize = self.rebalance_plan.iter().map(|step| step.slots.len()).sum();

        let preview: gpui::AnyElement = if !self.rebalance_planned {
            Label::new(i18n_topology(cx, "rebalance_hint"))
                .text_xs()
                .text_color(muted)
                .whitespace_normal()
                .into_any_element()
        } else if self.rebalance_plan.is_empty() {
            Label::new(i18n_topology(cx, "rebalance_balanced"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            let rows: Vec<gpui::AnyElement> = self
                .rebalance_plan
                .iter()
                .map(|step| {
                    let line = rust_i18n::t!(
                        "topology.rebalance_row",
                        source = short_node_id(&step.source_id),
                        target = short_node_id(&step.target_id),
                        count = step.slots.len(),
                        locale = &locale
                    )
                    .to_string();
                    Label::new(SharedString::from(line))
                        .text_xs()
                        .font_family(get_mono_font_family())
                        .into_any_element()
                })
                .collect();
            let summary: SharedString = rust_i18n::t!(
                "topology.rebalance_will_move",
                moves = self.rebalance_plan.len(),
                slots = slots,
                locale = &locale
            )
            .to_string()
            .into();
            v_flex()
                .gap_0p5()
                .child(Label::new(summary).text_xs())
                .children(rows)
                .into_any_element()
        };

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(i18n_topology(cx, "rebalance_title")).font_semibold())
                    .child(div().flex_1())
                    .child(
                        Button::new("topo-rebalance-plan")
                            .outline()
                            .small()
                            .label(i18n_topology(cx, "rebalance_plan"))
                            .disabled(!can_write || self.reshard_running)
                            .on_click(cx.listener(|this, _, _window, cx| this.run_rebalance_plan(cx))),
                    )
                    .child(
                        Button::new("topo-rebalance-exec")
                            .danger()
                            .small()
                            .label(i18n_topology(cx, "rebalance_execute"))
                            .disabled(!can_write || self.rebalance_plan.is_empty() || self.reshard_running)
                            .on_click(cx.listener(|this, _, window, cx| this.open_rebalance_dialog(window, cx))),
                    ),
            )
            .child(preview)
            .into_any_element()
    }

    pub(super) fn run_plan(&mut self, cx: &mut Context<Self>) {
        let desc = self.server_state.read(cx).nodes_description();
        let source_raw = self.reshard_source_input.read(cx).value().to_string();
        let target_raw = self.reshard_target_input.read(cx).value().to_string();
        let count_raw = self.reshard_count_input.read(cx).value().to_string();

        let source_id = {
            let t = source_raw.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        let target_id = target_raw.trim().to_string();
        if target_id.is_empty() {
            self.plan_error = Some(i18n_topology(cx, "err_reshard_target"));
            self.planned_slots.clear();
            cx.notify();
            return;
        }
        let count: u32 = match count_raw.trim().parse() {
            Ok(n) if n > 0 => n,
            _ => {
                self.plan_error = Some(i18n_topology(cx, "err_reshard_count"));
                self.planned_slots.clear();
                cx.notify();
                return;
            }
        };

        // Expand slot_map masters into (id, ranges) for the planner.
        // Ranges come from owners filtered by node_id.
        let mut by_id: std::collections::HashMap<String, Vec<(u16, u16)>> = std::collections::HashMap::new();
        for o in &desc.slot_map.owners {
            by_id.entry(o.node_id.to_string()).or_default().push((o.start, o.end));
        }
        let masters: Vec<(String, Vec<(u16, u16)>)> = by_id.into_iter().collect();

        match plan_cluster_reshard(&masters, source_id.as_deref(), &target_id, count) {
            Ok(slots) => {
                self.planned_slots = slots;
                self.plan_error = None;
            }
            Err(e) => {
                self.planned_slots.clear();
                // Map known English planner errors to i18n keys.
                let key = match e.as_str() {
                    "no source slots available" => "err_reshard_no_source",
                    other if other.contains("target") => "err_reshard_target",
                    _ => "err_reshard_plan",
                };
                let mapped = if key == "err_reshard_plan" {
                    SharedString::from(e)
                } else {
                    i18n_topology(cx, key)
                };
                self.plan_error = Some(mapped);
            }
        }
        cx.notify();
    }

    pub(super) fn open_reshard_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.planned_slots.is_empty() || self.reshard_running {
            return;
        }
        if !self.can_cluster_write(cx) {
            return;
        }
        let desc = self.server_state.read(cx).nodes_description();
        let target_id = self.reshard_target_input.read(cx).value().trim().to_string();
        let target_addr = desc
            .slot_map
            .masters
            .iter()
            .find(|m| m.node_id.as_str() == target_id)
            .map(|m| m.addr.to_string())
            .unwrap_or_default();
        if target_addr.is_empty() {
            self.plan_error = Some(i18n_topology(cx, "err_reshard_target_missing"));
            cx.notify();
            return;
        }

        // Build owner list for source mapping.
        let mut masters_with_addr: Vec<ClusterMasterRanges> = Vec::new();
        for m in &desc.slot_map.masters {
            let ranges: Vec<(u16, u16)> = desc
                .slot_map
                .owners
                .iter()
                .filter(|o| o.node_id == m.node_id)
                .map(|o| (o.start, o.end))
                .collect();
            masters_with_addr.push(ClusterMasterRanges {
                node_id: m.node_id.to_string(),
                addr: m.addr.to_string(),
                ranges,
            });
        }
        let source_by_slot = match source_owners_for_slots(&masters_with_addr, &self.planned_slots) {
            Ok(v) => v,
            Err(e) => {
                self.plan_error = Some(e.into());
                cx.notify();
                return;
            }
        };

        // Valkey 9 hands the whole job to the source nodes; everything
        // else walks the slots here. The wording differs because the
        // promise does: an atomic migration survives closing the app.
        let atomic = self.atomic_migration_supported(cx);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let title = i18n_topology(
            cx,
            if atomic {
                "reshard_atomic_confirm_title"
            } else {
                "reshard_confirm_title"
            },
        );
        let body = rust_i18n::t!(
            if atomic {
                "topology.reshard_atomic_confirm_body"
            } else {
                "topology.reshard_confirm_body"
            },
            count = self.planned_slots.len(),
            target_id = target_id.as_str(),
            target_addr = target_addr.as_str(),
            locale = locale
        )
        .to_string();
        // One MIGRATESLOTS per source node, each carrying its own ranges.
        let atomic_jobs: Vec<(SharedString, Vec<(u16, u16)>)> = if atomic {
            let mut by_source: std::collections::HashMap<String, Vec<u16>> = std::collections::HashMap::new();
            for (slot, source_addr, _) in &source_by_slot {
                by_source.entry(source_addr.clone()).or_default().push(*slot);
            }
            let mut jobs: Vec<(SharedString, Vec<(u16, u16)>)> = by_source
                .into_iter()
                .map(|(addr, slots)| (SharedString::from(addr), group_slot_ranges(&slots)))
                .collect();
            jobs.sort_by(|a, b| a.0.cmp(&b.0));
            jobs
        } else {
            Vec::new()
        };
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let slots = self.planned_slots.clone();
        let target_addr_s: SharedString = target_addr.into();
        let target_id_s: SharedString = target_id.into();
        let entity = cx.entity().downgrade();
        ZedisDialog::new_alert(title, escalate_dangerous_body(cx, &server_id, body))
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let slots = slots.clone();
                let source_by_slot = source_by_slot.clone();
                let t_addr = target_addr_s.clone();
                let t_id = target_id_s.clone();
                let jobs = atomic_jobs.clone();
                server_state.update(cx, |state, cx| {
                    if atomic {
                        state.cluster_migrate_slots_atomic(jobs, t_id, cx);
                    } else {
                        state.cluster_reshard(t_addr, t_id, slots, source_by_slot, cx);
                    }
                });
                if let Some(this) = entity.upgrade() {
                    this.update(cx, |this, cx| {
                        // The atomic path returns as soon as the servers
                        // accepted the job — the progress bar belongs to the
                        // loop this process drives, not to them.
                        this.reshard_running = !atomic;
                        this.planned_slots.clear();
                        cx.notify();
                    });
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}
