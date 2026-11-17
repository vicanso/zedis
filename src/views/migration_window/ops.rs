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

use super::*;

impl ZedisMigrationWindow {
    pub(super) fn spawn_follow_stream(
        &mut self,
        source_id: SharedString,
        source_db: usize,
        target_id: SharedString,
        target_db: usize,
        patterns: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.following = true;
        self.follow_synced_count = 0;
        self.follow_last_key = None;
        self.follow_disconnected = false;
        cx.notify();

        let source_at = ServerDb::new(source_id.as_str(), source_db);
        let target_at = ServerDb::new(target_id.as_str(), target_db);
        let psub_patterns: Vec<String> = patterns
            .iter()
            .map(|p| format!("__keyspace@{source_db}__:{p}"))
            .collect();
        let prefix_to_strip = format!("__keyspace@{source_db}__:");

        self.follow_task = Some(cx.spawn(async move |this, cx| {
            let sub = cx
                .background_spawn({
                    let source_at = source_at.clone();
                    let psub_patterns = psub_patterns.clone();
                    async move {
                        let refs: Vec<&str> = psub_patterns.iter().map(String::as_str).collect();
                        ChannelSubscription::open(&source_at, SubscribeKind::KeyspaceEvents, &refs).await
                    }
                })
                .await;
            let Ok(mut pubsub) = sub else {
                let _ = this.update(cx, |view, cx| {
                    view.following = false;
                    view.follow_disconnected = true;
                    cx.notify();
                });
                return;
            };

            loop {
                let (next_msg, returned_pubsub) = cx
                    .background_spawn(async move {
                        let msg = pubsub.next_message().await;
                        (msg, pubsub)
                    })
                    .await;
                pubsub = returned_pubsub;
                let Some(msg) = next_msg else {
                    let _ = this.update(cx, |view, cx| {
                        view.following = false;
                        view.follow_disconnected = true;
                        cx.notify();
                    });
                    break;
                };
                let Some(key) = msg.channel.strip_prefix(&prefix_to_strip).map(str::to_string) else {
                    continue;
                };
                let event = String::from_utf8_lossy(&msg.payload).trim().to_string();
                let source_at_op = source_at.clone();
                let target_at_op = target_at.clone();
                let key_op = key.clone();
                let event_op = event.clone();
                let _ = cx
                    .background_spawn(async move {
                        if matches!(event_op.as_str(), "del" | "expired" | "evicted") {
                            let _ = delete_keys(&target_at_op, vec![key_op]).await;
                        } else {
                            let _ = copy_keys_batch(
                                &source_at_op,
                                &target_at_op,
                                std::slice::from_ref(&key_op),
                                ConflictMode::Overwrite,
                            )
                            .await;
                        }
                    })
                    .await;
                let alive = this.update(cx, |view, cx| {
                    view.follow_synced_count += 1;
                    view.follow_last_key = Some(format!("{key} ({event})").into());
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    pub(super) fn stop_follow(&mut self, cx: &mut Context<Self>) {
        self.follow_task = None;
        if self.following {
            self.following = false;
            cx.notify();
        }
    }

    /// Opens the Compare window pre-populated with `key` so the user can
    /// inspect a verification mismatch in one click.
    pub(super) fn open_key_in_compare(&self, key: SharedString, cx: &mut App) {
        let MigrationWindowMode::Export {
            server_id,
            server_name,
            db,
            ..
        } = &self.mode
        else {
            return;
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        open_compare_for_key(
            server_id.clone(),
            server_name.clone(),
            *db,
            target_id,
            self.target_db(cx),
            key,
            cx,
        );
    }

    /// Post-copy verification: checks that each copied key exists on the
    /// target with the same type and value.
    pub(super) fn verify_copy(&mut self, cx: &mut Context<Self>) {
        let MigrationWindowMode::Export { server_id, db, .. } = &self.mode else {
            return;
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.verify_running || self.target_is_source(cx) {
            return;
        }
        let keys: Vec<String> = match self.source_mode {
            KeySourceMode::LoadedKeys => self
                .filtered_export_keys(cx)
                .iter()
                .map(|key| key.to_string())
                .collect(),
            KeySourceMode::ScanPatterns => self
                .state
                .read(cx)
                .copied_keys()
                .iter()
                .map(|key| key.to_string())
                .collect(),
        };
        if keys.is_empty() {
            return;
        }
        let source_at = ServerDb::new(server_id.as_str(), *db);
        let target_at = ServerDb::new(target_id.as_str(), self.target_db(cx));
        self.verify_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.verify_cancel.clone();
        let superseded = self.verify_cancel.clone();
        self.verify_running = true;
        self.verify_error = None;
        self.verify_report = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { verify_copied_keys(&source_at, &target_at, &keys, &cancel).await })
                .await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) {
                    return;
                }
                view.verify_running = false;
                match result {
                    Ok(report) => {
                        view.verify_report = Some(report);
                        view.verify_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "copy verification failed");
                        view.verify_report = None;
                        view.verify_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Dry run of a copy: `EXISTS` for the filtered keys on the target.
    pub(super) fn preview_copy(&mut self, cx: &mut Context<Self>) {
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.preview_running || self.target_is_source(cx) {
            return;
        }
        let keys: Vec<String> = self
            .filtered_export_keys(cx)
            .iter()
            .map(|key| key.to_string())
            .collect();
        let target_id = target_id.to_string();
        let target_db = self.target_db(cx);
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        let superseded = self.preview_cancel.clone();
        self.preview_running = true;
        self.preview_error = None;
        self.preview = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    preview_key_conflicts(&target_id, target_db, &keys, PREVIEW_SAMPLE_LIMIT, &cancel).await
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) {
                    return;
                }
                view.preview_running = false;
                match result {
                    Ok(preview) => {
                        view.preview = Some(preview);
                        view.preview_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "copy conflict preview failed");
                        view.preview = None;
                        view.preview_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
