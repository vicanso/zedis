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
//! building, the reply parsing, the views — is the same code the desktop
//! client runs: `zedis_gui::launch` opens the same window the desktop opens.
//! What this crate adds is only the browser's way in: the transport, the
//! server store, the fonts, and the asset source.
//!
//! Build with `make web-bundle` (from the repo root; it runs from this
//! directory so rustup reads the `rust-toolchain.toml` here and selects the
//! nightly the browser backend needs), then serve the bundle and the API from
//! one origin with `make web-serve`.

#[cfg(target_family = "wasm")]
mod assets;
pub mod transport;

#[cfg(target_family = "wasm")]
use gpui::{Application, ApplicationHandle};
#[cfg(target_family = "wasm")]
use gpui_web::{CanvasFontFallback, WebBackendPreference, WebPlatform};
#[cfg(target_family = "wasm")]
use std::borrow::Cow;
#[cfg(target_family = "wasm")]
use std::cell::RefCell;
#[cfg(target_family = "wasm")]
use std::rc::Rc;
#[cfg(target_family = "wasm")]
use std::sync::Arc;
#[cfg(target_family = "wasm")]
use tracing::{error, info};
#[cfg(target_family = "wasm")]
use tracing_subscriber::prelude::*;
#[cfg(target_family = "wasm")]
use transport::HttpBridgeTransport;
#[cfg(target_family = "wasm")]
use zedis_connection::{set_bridge_server_store, set_bridge_transport, set_servers_cache};
#[cfg(target_family = "wasm")]
use zedis_gui::helpers::pacing::set_page_hidden;
#[cfg(target_family = "wasm")]
use zedis_gui::helpers::set_web_command_key;
#[cfg(target_family = "wasm")]
use zedis_gui::states::{GlobalEvent, ZedisAppState, ZedisGlobalStore};

/// The browser entry point.
///
/// `origin` is `window.location.origin`: the bridge serving this page is the
/// bridge the page talks to. `ui_font` is the bytes of the one family the web
/// text system resolves `.SystemUIFont` to (IBM Plex Sans): that system starts
/// from an empty font database, so the page fetches the file and hands it in,
/// and it is registered here before the kit initialises and before anything
/// can measure a glyph. Fetching it from inside would have put a frame
/// between "started" and "has a font". `apple_keyboard` is the page's reading
/// of `navigator`: one wasm module serves a Mac and a Windows laptop alike,
/// so which key the shortcuts are *drawn* with is a run-time fact here, not
/// the compile-time one it is on the desktop.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn run(origin: String, ui_font: Vec<u8>, apple_keyboard: bool) -> Result<(), wasm_bindgen::JsValue> {
    // A panic in wasm is otherwise an unexplained `unreachable`.
    console_error_panic_hook::set_once();
    // Straight to `set_global_default`, not `init()`: `init` first installs
    // the `log` → `tracing` bridge, and `web_init` above has already claimed
    // the `log` logger for the platform's console writer — so `init` returned
    // an error and, being `init`, panicked on it before the first frame. This
    // way the platform keeps `log` and Zedis's own `tracing` reaches the
    // console beside it.
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(tracing_web::MakeWebConsoleWriter::new()),
    );
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        error!("a tracing subscriber was already installed; Zedis's logs go to that one");
    }

    gpui_kit::platform::web_init();
    // `gpui_platform::single_threaded_web()`, spelled out for the one argument
    // it does not take: the canvas font fallback. The web text system shapes
    // with the fonts it was handed — IBM Plex Sans and JetBrains Mono, neither
    // of which has a CJK glyph — and a page cannot read the system's fonts to
    // add one. `EmojiAndCjk` lets the browser draw what the loaded fonts lack
    // with its own `sans-serif` (PingFang, YaHei, Noto…), so Chinese, Japanese
    // and Korean text renders without a 10 MB font in the download. The policy
    // is fixed at construction, which is why it cannot be set afterwards.
    //
    // Single-threaded (`false`): browser threads need cross-origin isolation
    // (`SharedArrayBuffer`), which the bridge does not promise to serve.
    let platform = Rc::new(WebPlatform::new_with_backend_and_font_fallback(
        false,
        WebBackendPreference::Auto,
        CanvasFontFallback::EmojiAndCjk,
    ));
    let http_client = Arc::new(platform.fetch_http_client());
    let app = Application::with_platform(platform).with_http_client(http_client);
    // The kit fetches its icons from `{origin}/assets/icons/…`: it needs an
    // absolute base, not a relative one.
    let web_assets = assets::WebAssets::new(&origin);
    let icon_watch = web_assets.take_watch();
    let app = app.with_assets(web_assets);
    // `run_embedded`, not `run`. On the desktop `Platform::run` blocks for the
    // life of the app and `Application::run`'s stack frame owns the app state.
    // The browser's run loop belongs to the browser: its `run` schedules the
    // launch and returns at once, so the closure below is the last thing
    // holding the app — and the first fetch to complete afterwards found it
    // gone ("app was released before async operation completed"). The handle
    // this returns is the owner instead, kept for the life of the page.
    let handle = app.run_embedded(move |cx| {
        if let Err(e) = cx.text_system().add_fonts(vec![Cow::Owned(ui_font)]) {
            error!(error = %e, "the UI font could not be registered");
        }
        gpui_kit::component::init(cx);
        // The kit fetches its icons and tells nobody when they arrive.
        if let Some(icon_watch) = icon_watch {
            assets::repaint_when_icons_land(icon_watch, cx);
        }

        // The bridge is both where commands go and where the server list
        // lives. One transport serves both roles; no credential, because the page
        // logged in for a cookie and the fetch carries it.
        let transport = Arc::new(HttpBridgeTransport::new(cx.http_client(), origin));
        set_bridge_transport(transport.clone());
        set_bridge_server_store(transport.clone());

        // Before `launch`, which binds the keys and draws their labels.
        set_web_command_key(apple_keyboard);
        zedis_gui::init_embedded_commands();
        if let Err(e) = zedis_gui::db::init_database() {
            error!(error = %e, "the in-memory local store could not be created");
        }
        zedis_gui::init_caches();

        // Launch synchronously, here in the run closure — exactly where the
        // desktop's `main` calls it. Doing it from inside a spawned
        // `cx.update` instead nested launch's own window-open spawn under a
        // borrow that the render loop then could not take, and every frame
        // logged "already borrowed" while the page stayed blank.
        let app_state = ZedisAppState::try_new().unwrap_or_else(|e| {
            error!(error = %e, "app state could not be loaded; starting with defaults");
            ZedisAppState::new()
        });
        zedis_gui::launch(cx, app_state);

        // The server list follows, the way it does on the desktop: the window
        // is already up, and the sidebar refreshes when the list lands. No
        // credential — the login cookie rides along on a same-origin fetch.
        cx.spawn(async move |cx| {
            let servers = transport.fetch_servers().await;
            let _ = cx.update(|cx| match servers {
                Ok(list) => {
                    info!(count = list.len(), "server list from the bridge");
                    set_servers_cache(list);
                    // Tell the sidebar to re-read the (now filled) list.
                    let store = cx.global::<ZedisGlobalStore>().clone();
                    let state = store.state();
                    state.update(cx, |_state, cx| cx.emit(GlobalEvent::ServerListUpdated));
                }
                Err(e) => error!(error = %e, "the server list could not be fetched"),
            });
        })
        .detach();
    });
    APP.with(|slot| *slot.borrow_mut() = Some(handle));
    Ok(())
}

/// The page's `visibilitychange`, from `index.html`. A hidden page polls the
/// bridge at the pace of a background workspace tab instead of the heartbeat
/// (`zedis_gui::helpers::pacing`): a browser tab is left open for days in a
/// way a desktop window is not, and each of its beats is work for a bridge
/// and a Redis that everybody shares. Nothing is kicked when the page comes
/// back — the next heartbeat tick, at most one interval away, goes through.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_page_visible(visible: bool) {
    set_page_hidden(!visible);
}

// The page is the process: it lives until the tab closes, and so does this.
#[cfg(target_family = "wasm")]
thread_local! {
    static APP: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}
