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

//! Zedis in a browser tab.
//!
//! The Redis traffic leaves through `zedis-bridge` over HTTP, because a page
//! has no socket to open (ADR 9). Everything above that — the command
//! building, the reply parsing, the value editors — is the same code the
//! desktop client runs.
//!
//! Build it from *this* directory, so rustup reads the `rust-toolchain.toml`
//! next to this file and selects the nightly the browser backend needs:
//!
//! ```sh
//! cd zedis-web
//! cargo build --target wasm32-unknown-unknown --release
//! wasm-bindgen ../../target/wasm32-unknown-unknown/release/zedis_web.wasm \
//!     --out-dir www/wasm --target web --no-typescript
//! ```
//!
//! Then serve the bundle and the API from one origin:
//! `zedis-bridge --static <dir>`.

pub mod transport;

/// The browser entry point.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn run() -> Result<(), wasm_bindgen::JsValue> {
    // A panic in wasm is otherwise an unexplained `unreachable`.
    console_error_panic_hook::set_once();

    gpui_kit::platform::web_init();
    // Single-threaded: browser threads need cross-origin isolation
    // (`SharedArrayBuffer`), which the bridge does not promise to serve.
    let app = gpui_kit::platform::single_threaded_web();

    // Icons are fetched from this origin rather than embedded, which is why
    // the bridge serving the page is also what serves them.
    let app = app.with_assets(gpui_kit::assets::Assets::new(""));
    app.run(|cx| {
        gpui_kit::component::init(cx);
        // TODO: fonts must be registered before the first frame. The web
        // platform ships none, and GPUI's `.SystemUIFont` alias resolves to a
        // family that has to exist or the text system panics while measuring
        // text for the first frame.
        let _ = cx;
    });
    Ok(())
}
