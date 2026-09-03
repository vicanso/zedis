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

//! HyperLogLog viewer for string keys that hold an HLL sketch.
//!
//! Redis stores a HyperLogLog as a plain string whose `TYPE` is
//! `string`, so — unlike the RedisBloom structures — it cannot be
//! classified at the TYPE layer. Instead [`crate::views::ZedisEditor`]
//! peeks at the already-loaded value bytes: every HLL begins with the
//! 4-byte magic `"HYLL"` (see [`looks_like_hll`]), and when it matches
//! the raw bytes editor is swapped for this dedicated read-only card.
//!
//! The card shows the estimated cardinality (`PFCOUNT`), the internal
//! encoding (dense / sparse, read from the header byte) and the byte
//! size (`STRLEN`). A single `PFADD` input folds new elements in so the
//! user can watch the estimate move; that is the only mutation — the raw
//! sketch bytes are never editable.

use crate::helpers::get_mono_font_family;
use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ZedisServerState, i18n_hll},
};
use gpui::{Context, Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, StyledExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use redis::cmd;
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// HLL header magic. Every Redis HyperLogLog string (dense or sparse)
/// starts with these 4 ASCII bytes followed by a 12-byte header, so a
/// minimum length of 16 guards against a short user string that merely
/// happens to begin with "HYLL".
const HLL_MAGIC: &[u8] = b"HYLL";

/// Whether `bytes` look like a Redis HyperLogLog sketch. Used by the
/// editor dispatch to route an HLL-bearing string key to this card
/// instead of the raw bytes editor.
pub(crate) fn looks_like_hll(bytes: &[u8]) -> bool {
    bytes.len() >= 16 && bytes.starts_with(HLL_MAGIC)
}

/// Decoded HLL stats for display.
#[derive(Clone, Default)]
struct HllData {
    /// `PFCOUNT key` — estimated cardinality.
    cardinality: i64,
    /// Dense vs sparse, from the encoding byte at offset 4 (i18n key).
    encoding: Option<&'static str>,
    /// `STRLEN key` — internal representation size in bytes.
    size: u64,
}

pub struct ZedisHllEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    data: Option<HllData>,
    error: Option<SharedString>,
    loading: bool,
    /// `PFADD` element input.
    add_input: Entity<InputState>,
    /// Inline notice under the add row (e.g. empty input or a failed add).
    add_error: Option<SharedString>,
    /// In-flight fetch; dropped (and thereby cancelled) when the editor
    /// is recreated for a new key.
    load_task: Option<Task<()>>,
    /// In-flight `PFADD`; dropped likewise.
    add_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisHllEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new HLL editor view");
        let placeholder = i18n_hll(cx, "add_placeholder").to_string();
        let add_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));

        let subscriptions = vec![cx.subscribe_in(&add_input, window, |this, _state, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.run_add(cx);
            }
        })];

        let mut this = Self {
            server_state,
            key,
            data: None,
            error: None,
            loading: false,
            add_input,
            add_error: None,
            load_task: None,
            add_task: None,
            _subscriptions: subscriptions,
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
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_hll(server_id, db, key).await;
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

    /// `PFADD key <elems>` from the add input (whitespace-split), then
    /// reload the estimate. Additive and non-destructive, so no confirm.
    fn run_add(&mut self, cx: &mut Context<Self>) {
        let raw = self.add_input.read(cx).value().to_string();
        let elems: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if elems.is_empty() {
            self.add_error = Some(i18n_hll(cx, "add_empty"));
            cx.notify();
            return;
        }
        self.add_error = None;
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        cx.notify();
        self.add_task = Some(cx.spawn(async move |this, cx| {
            let result = pfadd(server_id, db, key, elems).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.add_error = None;
                    // Refresh PFCOUNT / encoding / size.
                    this.load(cx);
                }
                Err(e) => {
                    this.add_error = Some(SharedString::from(e.to_string()));
                    cx.notify();
                }
            });
        }));
    }

    fn stat_row(&self, label: SharedString, value: String, muted: Hsla) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_4()
            .items_baseline()
            .child(
                div()
                    .min_w(px(180.))
                    .child(Label::new(label).text_xs().text_color(muted)),
            )
            .child(Label::new(value).text_xs().font_semibold())
    }
}

impl Render for ZedisHllEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let chip_bg = cx.theme().muted;

        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Label::new(i18n_hll(cx, "title")).font_semibold())
            .child(
                div()
                    .px_1p5()
                    .rounded_full()
                    .bg(chip_bg)
                    .child(Label::new("HLL").text_xs().text_color(muted)),
            );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(danger))
                .into_any_element()
        } else if let Some(data) = self.data.as_ref() {
            let mut rows = v_flex().w_full().gap_1().child(self.stat_row(
                i18n_hll(cx, "estimated"),
                data.cardinality.to_string(),
                muted,
            ));
            if let Some(enc) = data.encoding {
                rows = rows.child(self.stat_row(i18n_hll(cx, "encoding"), i18n_hll(cx, enc).to_string(), muted));
            }
            rows = rows.child(self.stat_row(i18n_hll(cx, "size"), format_size(data.size, DECIMAL), muted));
            rows.into_any_element()
        } else {
            div()
                .p_4()
                .child(Label::new(i18n_hll(cx, "loading")).text_sm().text_color(muted))
                .into_any_element()
        };

        let add_row = v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.add_input).appearance(true)))
                    .child(
                        Button::new("hll-pfadd")
                            .label(i18n_hll(cx, "add"))
                            .on_click(cx.listener(|this, _, _window, cx| this.run_add(cx))),
                    ),
            )
            .when_some(self.add_error.clone(), |this, err| {
                this.child(Label::new(err).text_xs().text_color(danger))
            });

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .gap_3()
            .p_3()
            .child(header)
            .child(body)
            .child(add_row)
    }
}

/// Fetch the estimated cardinality (`PFCOUNT`), the internal encoding
/// (dense / sparse, from the header byte at offset 4) and the byte size
/// (`STRLEN`). Only `PFCOUNT` is fatal on error; the extras best-effort.
async fn fetch_hll(server_id: String, db: usize, key: String) -> Result<HllData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let cardinality: i64 = cmd("PFCOUNT").arg(&key).query_async(&mut conn).await?;
    let size: i64 = cmd("STRLEN").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    // Header byte at offset 4: 0 = dense, 1 = sparse.
    let encoding = match cmd("GETRANGE")
        .arg(&key)
        .arg(4)
        .arg(4)
        .query_async::<Vec<u8>>(&mut conn)
        .await
        .ok()
        .and_then(|b| b.first().copied())
    {
        Some(0) => Some("dense"),
        Some(1) => Some("sparse"),
        _ => None,
    };

    Ok(HllData {
        cardinality,
        encoding,
        size: size.max(0) as u64,
    })
}

/// `PFADD key <elems>` — fold new elements into the sketch.
async fn pfadd(server_id: String, db: usize, key: String, elems: Vec<String>) -> Result<()> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    let mut command = cmd("PFADD");
    command.arg(&key);
    for elem in &elems {
        command.arg(elem);
    }
    let _: i64 = command.query_async(&mut conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::looks_like_hll;

    #[test]
    fn detects_hll_magic() {
        // A 16-byte buffer starting with the HYLL magic is an HLL.
        let mut buf = b"HYLL".to_vec();
        buf.extend_from_slice(&[0u8; 12]);
        assert!(looks_like_hll(&buf));
    }

    #[test]
    fn rejects_plain_strings() {
        assert!(!looks_like_hll(b"hello world this is plain"));
    }

    #[test]
    fn rejects_short_magic() {
        // Right prefix but too short to be a real header.
        assert!(!looks_like_hll(b"HYLL"));
    }
}
