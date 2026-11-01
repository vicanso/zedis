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

//! AI report analysis, CSV exports (prefixes / top keys), and Data Dictionary
//! exports / dialog for [`ZedisMemoryAnalysis`].

use super::*;

impl ZedisMemoryAnalysis {
    /// Drop the AI result panel and cancel any in-flight request.
    pub(super) fn clear_ai_result(&mut self, cx: &mut gpui::Context<Self>) {
        self.ai_task.take();
        self.ai_status = AiStatus::Idle;
        self.ai_output = None;
        cx.notify();
    }

    /// Send the current analysis report to the configured AI endpoint
    /// and render its advice. No-op (with a notification) when the
    /// endpoint is not configured or there is nothing to analyze.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn start_ai_analysis(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.ai_status == AiStatus::Running {
            return;
        }

        let store = cx.global::<ZedisGlobalStore>().read(cx);
        if !store.ai_configured() {
            window.push_notification(Notification::warning(i18n_memory_analysis(cx, "ai_not_configured")), cx);
            return;
        }
        let endpoint = AiEndpoint {
            base_url: store.ai_base_url(),
            api_key: store.ai_api_key(),
            model: store.ai_model(),
        };
        // Ask the model to reply in the app's current UI language.
        let locale = store.locale().to_string();

        // Build the Markdown report from the freshly computed rows. Only
        // key names/sizes/TTLs are sent — never key values.
        let prefix_rows = self.prefix_rows.clone();
        let single_rows = self.single_rows.clone();
        if prefix_rows.is_empty() && single_rows.is_empty() && self.ttl_histogram.total() == 0 {
            window.push_notification(Notification::warning(i18n_memory_analysis(cx, "ai_no_data")), cx);
            return;
        }
        // Offline RDB results: the live server's dbsize / sample ratio
        // don't describe the file — report exact (unsampled) file data.
        let (report_dbsize, report_ratio) = if self.rdb_file.is_some() {
            (None, 1.0)
        } else {
            (self.dbsize, self.row_ratio())
        };
        let report = build_markdown_report(
            report_dbsize,
            &self.policy,
            report_ratio,
            &prefix_rows,
            &single_rows,
            &self.ttl_histogram,
            &self.type_rows,
        );

        self.ai_status = AiStatus::Running;
        self.ai_output = None;
        cx.notify();

        self.ai_task = Some(cx.spawn(async move |handle, cx| {
            // `analyze_report` is blocking (ureq) — keep it on the
            // background pool so the UI thread stays responsive.
            let result = cx
                .background_spawn(async move { analyze_report(&endpoint, &report, &locale) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(markdown) => {
                        this.ai_status = AiStatus::Done;
                        this.ai_output = Some(markdown.into());
                    }
                    Err(e) => {
                        error!(error = %e, "AI memory analysis failed");
                        this.ai_status = AiStatus::Error;
                        this.ai_output = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// No HTTP client in the browser build; the button is not offered.
    #[cfg(target_family = "wasm")]
    pub(super) fn start_ai_analysis(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let _ = window;
        self.ai_status = AiStatus::Error;
        self.ai_output = Some("The AI assistant is not available in the browser build.".into());
        cx.notify();
    }

    /// Export the prefix-group table as CSV. Raw byte / count figures are
    /// included alongside the display strings so the file sorts and
    /// aggregates cleanly in a spreadsheet.
    pub(super) fn export_prefixes_csv(&mut self, cx: &mut gpui::Context<Self>) {
        // In the table's current order, raw numbers from the payload cells.
        let rows = self.prefix_table.read(cx).delegate().visible_rows();
        if rows.is_empty() {
            return;
        }
        let cell = |cells: &[SharedString], ix: usize| cells.get(ix).map(|c| c.to_string()).unwrap_or_default();
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|c| {
                vec![
                    cell(c, 0),
                    cell(c, PREFIX_CELL_KEY_COUNT),
                    cell(c, PREFIX_CELL_MEMORY_BYTES),
                    cell(c, 2),
                    cell(c, 5),
                    cell(c, 3),
                    cell(c, PREFIX_CELL_PERM_COUNT),
                ]
            })
            .collect();
        let csv = build_csv(
            &[
                "prefix",
                "key_count",
                "memory_bytes",
                "memory",
                "types",
                "avg_ttl",
                "perm_keys",
            ],
            &data,
        );
        self.save_export(csv, "memory-prefixes.csv", cx);
    }

    /// Export the single-key Top-N table (in its current ranking) as CSV.
    pub(super) fn export_keys_csv(&mut self, cx: &mut gpui::Context<Self>) {
        let rows = self.single_table.read(cx).delegate().visible_rows();
        if rows.is_empty() {
            return;
        }
        let cell = |cells: &[SharedString], ix: usize| cells.get(ix).map(|c| c.to_string()).unwrap_or_default();
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|c| {
                vec![
                    cell(c, 0),
                    cell(c, 3),
                    cell(c, KEY_CELL_MEMORY_BYTES),
                    cell(c, 1),
                    cell(c, 2),
                    cell(c, 4),
                ]
            })
            .collect();
        let csv = build_csv(&["key", "type", "memory_bytes", "memory", "ttl", "heat"], &data);
        self.save_export(csv, "memory-top-keys.csv", cx);
    }

    fn save_export(&mut self, csv: String, suggested: &'static str, cx: &mut gpui::Context<Self>) {
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            suggested,
            success,
            error,
        );
    }

    pub(super) fn has_schema_data(&self) -> bool {
        !self.schema.is_empty()
    }

    pub(super) fn schema_context_label(&self, cx: &gpui::App) -> String {
        let state = self.server_state.read(cx);
        if let Some(rdb) = &self.rdb_file {
            rdb.to_string()
        } else {
            let id = state.server_id();
            let name = crate::connection::get_server(id)
                .map(|s| s.name)
                .unwrap_or_else(|_| id.to_string());
            format!("{name} (db {})", state.db())
        }
    }

    pub(super) fn build_data_dictionary(&self) -> DataDictionary {
        self.schema.finish(self.row_ratio(), 50)
    }

    pub(super) fn export_data_dictionary_md(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.has_schema_data() {
            return;
        }
        let label = self.schema_context_label(cx);
        let md = self.build_data_dictionary().to_markdown(&label);
        self.save_export(md, "data-dictionary.md", cx);
    }

    pub(super) fn export_data_dictionary_json(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.has_schema_data() {
            return;
        }
        let label = self.schema_context_label(cx);
        let json = self.build_data_dictionary().to_json_pretty(&label);
        self.save_export(json, "data-dictionary.json", cx);
    }

    pub(super) fn open_schema_dialog(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if !self.has_schema_data() {
            return;
        }
        let label = self.schema_context_label(cx);
        let dict = self.build_data_dictionary();
        open_data_dictionary_dialog(dict, label, self.server_state.clone(), window, cx);
    }
}
