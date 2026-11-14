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

//! One-click remediation actions on Memory Analysis recommendations and
//! prefix-group rows (Pattern Bulk Ops, Config jump, and `MEMORY PURGE`).

use super::*;
use crate::connection::{Capability, DangerKind, get_server};
use crate::views::{confirm_dangerous_command, open_pattern_ops_dialog_with_pattern};

pub(super) fn normalize_prefix_glob(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        "*".to_string()
    } else if trimmed.contains('*') || trimmed.contains('?') {
        trimmed.to_string()
    } else {
        format!("{trimmed}*")
    }
}

pub(super) fn confirm_and_purge_memory(
    server_state: Entity<ZedisServerState>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let (server_id, db, allowed) = {
        let state = server_state.read(cx);
        (
            state.server_id().to_string(),
            state.db(),
            state.can(Capability::PersistenceWrite),
        )
    };
    if !allowed {
        return;
    }
    let Ok(server) = get_server(&server_id) else {
        return;
    };
    confirm_dangerous_command(
        &server,
        db,
        &DangerKind::MemoryPurge,
        Some("MEMORY PURGE"),
        window,
        cx,
        move |_window, cx| {
            server_state.update(cx, |state, cx| state.memory_purge(cx));
        },
    );
}

impl ZedisMemoryAnalysis {
    pub(super) fn render_reco_action(
        &self,
        ix: usize,
        reco: &Recommendation,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.rdb_file.is_some() {
            return None;
        }
        let state = self.server_state.read(cx);
        let can_pattern_ops = state.can(Capability::DeleteKeys) || state.can(Capability::SetTtl);
        let can_purge = state.can(Capability::PersistenceWrite);
        let id = ("reco-remediate", ix);

        match &reco.kind {
            RecoKind::BigKey { key, .. } => {
                let key = key.clone();
                let server_state = self.server_state.clone();
                Some(
                    Button::new(id)
                        .xsmall()
                        .outline()
                        .icon(IconName::Search)
                        .label(i18n_common(cx, "open_key_tooltip"))
                        .on_click(move |_, _window, cx| {
                            open_key_in_editor(&server_state, key.clone(), cx);
                        })
                        .into_any_element(),
                )
            }
            RecoKind::UnevictableKeys { .. } | RecoKind::NoEvictionPolicy => {
                if !ServerView::Config.in_this_build() {
                    return None;
                }
                Some(
                    Button::new(id)
                        .xsmall()
                        .outline()
                        .icon(IconName::Settings)
                        .label(i18n_memory_analysis(cx, "remediate_open_config"))
                        .on_click(|_, _window, cx| {
                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                store.update(cx, |s, cx| s.go_to_view(ServerView::Config, cx));
                            });
                        })
                        .into_any_element(),
                )
            }
            RecoKind::HighFragmentation { .. } => {
                if !can_purge {
                    return None;
                }
                let server_state = self.server_state.clone();
                Some(
                    Button::new(id)
                        .xsmall()
                        .outline()
                        .icon(CustomIconName::Zap)
                        .label(i18n_memory_analysis(cx, "remediate_memory_purge"))
                        .on_click(move |_, window, cx| {
                            confirm_and_purge_memory(server_state.clone(), window, cx);
                        })
                        .into_any_element(),
                )
            }
            RecoKind::ManySmallStrings { prefix, .. } | RecoKind::DominantPrefix { prefix, .. } => {
                if !can_pattern_ops {
                    return None;
                }
                let pattern = normalize_prefix_glob(prefix.as_ref());
                let server_state = self.server_state.clone();
                Some(
                    Button::new(id)
                        .xsmall()
                        .outline()
                        .icon(CustomIconName::Zap)
                        .label(i18n_memory_analysis(cx, "remediate_pattern_ops"))
                        .on_click(move |_, window, cx| {
                            open_pattern_ops_dialog_with_pattern(server_state.clone(), pattern.clone(), window, cx);
                        })
                        .into_any_element(),
                )
            }
        }
    }
}
