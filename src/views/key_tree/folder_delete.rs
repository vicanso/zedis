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

//! The folder-delete confirmation, asked with the number in it.
//!
//! The tree holds a sample of the keyspace — the first pages of a scan —
//! while the delete walks the server (`delete_keys_matching`) and removes
//! everything the prefix matches, whether the tree had loaded it or not. A
//! confirmation that names the folder therefore asks the user to agree to a
//! number they cannot see and the tree may understate by orders of
//! magnitude. So the dialog opens at once, counts with the same walk the
//! delete will make, and asks with the count (and a sampled size) once it
//! has it; until then the OK button keeps the dialog open. The pass is
//! bounded like the delete's, and a walk that stopped short says so.

use super::*;
use crate::connection::{PrefixImpact, count_keys_matching};

/// The dialog's body: the count while it is being made, then the question.
pub(super) struct FolderDeletePreview {
    folder: SharedString,
    server_id: String,
    /// `None` while counting; the walk's answer, or why there is none.
    impact: Option<Result<PrefixImpact, String>>,
}

impl FolderDeletePreview {
    fn counted(&self) -> bool {
        self.impact.is_some()
    }

    fn text(&self, cx: &App) -> SharedString {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let folder = self.folder.clone();
        let text = match &self.impact {
            None => t!("key_tree.delete_folder_counting", folder = folder, locale = &locale).to_string(),
            Some(Err(error)) => t!(
                "key_tree.delete_folder_count_failed",
                folder = folder,
                error = error,
                locale = &locale
            )
            .to_string(),
            Some(Ok(impact)) if impact.keys == 0 && impact.complete => {
                t!("key_tree.delete_folder_impact_none", folder = folder, locale = &locale).to_string()
            }
            Some(Ok(impact)) => {
                let count = group_thousands(impact.keys);
                let mut text = if impact.complete {
                    t!(
                        "key_tree.delete_folder_impact",
                        count = count,
                        folder = folder,
                        locale = &locale
                    )
                    .to_string()
                } else {
                    t!(
                        "key_tree.delete_folder_impact_partial",
                        count = count,
                        folder = folder,
                        locale = &locale
                    )
                    .to_string()
                };
                if let Some(bytes) = impact.estimated_bytes() {
                    let size = humansize::format_size(bytes, humansize::DECIMAL);
                    text.push(' ');
                    text.push_str(&t!(
                        "key_tree.delete_folder_impact_memory",
                        size = size,
                        locale = &locale
                    ));
                }
                text
            }
        };
        escalate_dangerous_body(cx, &self.server_id, text)
    }
}

impl Render for FolderDeletePreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The stock message body is bare text; this is the same, with a
        // spinner beside it while the count is out.
        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .when(!self.counted(), |this| this.child(Spinner::new().small()))
            // `min_w_0` so the text may shrink below its own width and wrap;
            // a flex item defaults to the width of its content.
            .child(div().flex_1().min_w_0().child(self.text(cx)))
    }
}

/// Open the confirmation for deleting `folder`, and start the count that
/// fills it in.
pub(super) fn open_folder_delete_dialog(
    tree: &ZedisKeyTree,
    folder: SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let server_state = tree.server_state.clone();
    let (at, pattern, server_id) = {
        let state = server_state.read(cx);
        (state.at(), state.folder_pattern(&folder), state.server_id().to_string())
    };
    let preview = cx.new(|_| FolderDeletePreview {
        folder: folder.clone(),
        server_id,
        impact: None,
    });

    let counting = preview.clone();
    cx.spawn(async move |_, cx| {
        let result = cx
            .background_spawn(async move { count_keys_matching(&at, &pattern).await })
            .await;
        counting.update(cx, |preview, cx| {
            preview.impact = Some(result.map_err(|e| e.to_string()));
            cx.notify();
        });
    })
    .detach();

    let body = preview.clone();
    ZedisDialog::new(i18n_key_tree(cx, "delete_folder_title"))
        .alert()
        .icon(IconName::Info)
        .child(move || body.clone())
        .button_props(dialog_button_props(cx))
        .on_ok(move |_, _, cx| {
            // Not before the number: the number is the question.
            if !preview.read(cx).counted() {
                return false;
            }
            server_state.update(cx, |state, cx| {
                state.delete_folder(folder.clone(), cx);
            });
            true
        })
        .open(window, cx);
}
