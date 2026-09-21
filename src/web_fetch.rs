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

//! Same-origin GET of static files the wasm does not embed.
//!
//! Locales, JetBrains Mono, `commands.json` and CONFIG help stay out of
//! `zedis_web_bg.wasm` (no zstd inflater on this target) and live next to
//! the kit icons on the bridge. [`set_origin`] is called once from the web
//! entry; everything else is a relative path from that origin.

use futures::AsyncReadExt as _;
use gpui::App;
use gpui::http_client::{AsyncBody, HttpClient, http::Request};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static ORIGIN: OnceLock<String> = OnceLock::new();
static REVS: OnceLock<HashMap<String, String>> = OnceLock::new();

/// `{origin}` with no trailing slash. The same value the kit uses for icons.
pub fn set_origin(origin: impl Into<String>) {
    let origin = origin.into().trim_end_matches('/').to_string();
    let _ = ORIGIN.set(origin);
}

/// Content hashes inlined in `index.html` (`path` → md5 prefix). `get_bytes`
/// appends `?v=` so a rebuilt file is a new URL and the old one can stay
/// cached (`Cache-Control: immutable` on the bridge).
pub fn set_revs(json: &str) {
    let Ok(map) = serde_json::from_str::<HashMap<String, String>>(json) else {
        tracing::warn!("asset revs could not be parsed; fetches go without ?v=");
        return;
    };
    let _ = REVS.set(map);
}

fn url_for(origin: &str, path: &str) -> String {
    match REVS.get().and_then(|revs| revs.get(path)) {
        Some(rev) => format!("{origin}/{path}?v={rev}"),
        None => format!("{origin}/{path}"),
    }
}

/// GET `{origin}/{path}` and hand the bytes to `on_done` on the main thread.
///
/// `None` if the origin was never set, the request failed, or the status was
/// not success. The caller decides whether that is worth retrying.
pub fn get_bytes(cx: &App, path: &str, on_done: impl FnOnce(&mut App, Option<Vec<u8>>) + Send + 'static) {
    let url = match ORIGIN.get() {
        Some(origin) => url_for(origin, path),
        None => {
            tracing::warn!(path, "static-file origin was not set");
            cx.spawn(async move |cx| {
                let _ = cx.update(|cx| on_done(cx, None));
            })
            .detach();
            return;
        }
    };
    let client = cx.http_client();
    cx.spawn(async move |cx| {
        let bytes = get_bytes_async(client, url).await;
        let _ = cx.update(|cx| on_done(cx, bytes));
    })
    .detach();
}

pub(crate) async fn get_bytes_async(client: Arc<dyn HttpClient>, url: String) -> Option<Vec<u8>> {
    let request = Request::builder()
        .method("GET")
        .uri(&url)
        .body(AsyncBody::from(String::new()))
        .ok()?;
    let mut response = client.send(request).await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    response.body_mut().read_to_end(&mut bytes).await.ok()?;
    Some(bytes)
}
