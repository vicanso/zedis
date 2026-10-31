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
use futures::StreamExt as _;
#[cfg(target_family = "wasm")]
use futures::channel::mpsc::UnboundedReceiver;
#[cfg(target_family = "wasm")]
use futures::future::{self, Either};
#[cfg(target_family = "wasm")]
use gpui::{Application, ApplicationHandle, AsyncApp, TextRenderingMode};
#[cfg(target_family = "wasm")]
use gpui_web::{CanvasFontFallback, WebBackendPreference, WebPlatform};
#[cfg(target_family = "wasm")]
use std::borrow::Cow;
#[cfg(target_family = "wasm")]
use std::cell::RefCell;
#[cfg(target_family = "wasm")]
use std::collections::HashSet;
#[cfg(target_family = "wasm")]
use std::rc::Rc;
#[cfg(target_family = "wasm")]
use std::sync::Arc;
#[cfg(target_family = "wasm")]
use std::time::Duration;
#[cfg(target_family = "wasm")]
use tracing::{error, info, warn};
#[cfg(target_family = "wasm")]
use tracing_subscriber::prelude::*;
#[cfg(target_family = "wasm")]
use transport::HttpBridgeTransport;
#[cfg(target_family = "wasm")]
use zedis_connection::{
    RedisServer, bridge_page_data, get_servers, set_bridge_accounts, set_bridge_page_data, set_bridge_server_store,
    set_bridge_storage, set_bridge_transport, set_servers_cache,
};
#[cfg(target_family = "wasm")]
use zedis_gui::helpers::pacing::{PAGE_DATA_REFRESH, PAGE_DATA_RETRY, SERVER_LIST_REFRESH, set_unattended, unattended};
#[cfg(target_family = "wasm")]
use zedis_gui::helpers::{
    DeepLink, PickDone, PickFailure, PickedFile, set_web_command_key, set_web_download, set_web_upload,
};
#[cfg(target_family = "wasm")]
use zedis_gui::states::{GlobalEvent, ZedisAppState, ZedisGlobalStore};

/// Install one `locales/<lang>.toml` fetched by the page. Must run before
/// [`run`]: the first frame already calls `t!`, and that lookup is
/// synchronous. A later language switch fetches through the same URL from
/// inside the app (`i18n_loader::when_locale_ready`).
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn install_locale(name: String, toml: String) {
    zedis_gui::i18n_loader::install_locale_toml(&name, &toml);
}

/// The content hashes the bridge served the page with, so later `web_fetch`
/// GETs carry `?v=<hash>` and the browser can keep those files forever.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_asset_revs(json: String) {
    zedis_gui::web_fetch::set_revs(&json);
}

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
/// the compile-time one it is on the desktop. `page_data` is the bridge's
/// answer to `GET /v1/page-data` (ADR 17): what this account keeps — its
/// favorites, recent keys and search history, the key tags and notes, the
/// saved scripts — loaded before anything reads it; empty when the page
/// could not get it, and then nothing is sent back either.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn run(
    origin: String,
    ui_font: Vec<u8>,
    apple_keyboard: bool,
    page_data: String,
) -> Result<(), wasm_bindgen::JsValue> {
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
    let page_seed = match page_data.as_str() {
        "" => {
            error!("no page data from the bridge; this page keeps its changes to itself");
            None
        }
        json => transport::page_seed(json)
            .map_err(|e| error!(error = %e, "the bridge's page data could not be read"))
            .ok(),
    };
    let http_client = Arc::new(platform.fetch_http_client());
    let app = Application::with_platform(platform).with_http_client(http_client);
    // Locales, JetBrains Mono, commands.json and CONFIG help are fetched
    // from this origin (same as the kit icons). The page preloads the
    // active locale before `run`; the rest land after the first frame.
    zedis_gui::web_fetch::set_origin(origin.clone());
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

        // The bridge is where commands go, where the server list lives,
        // where the recycle bin and the Metrics history are kept, and what
        // the account page asks. One
        // transport serves every role; no credential, because the page
        // logged in for a cookie and the fetch carries it.
        let transport = Arc::new(HttpBridgeTransport::new(cx.http_client(), origin));
        set_bridge_transport(transport.clone());
        set_bridge_server_store(transport.clone());
        set_bridge_storage(transport.clone());
        set_bridge_accounts(transport.clone());
        // Only with what the bridge holds loaded: a change made on top of
        // nothing would replace the account's lists with this page's.
        let page_data_loaded = page_seed.is_some();
        if page_data_loaded {
            set_bridge_page_data(transport.clone());
        }

        // Before `launch`, which binds the keys and draws their labels.
        set_web_command_key(apple_keyboard);
        set_web_download(download_file);
        set_web_upload(pick_file);
        // Grayscale antialiasing for every glyph, before the first frame. The
        // web text system asks for subpixel (RGB) antialiasing for the fonts
        // it was handed and grayscale for what the browser draws, so in a
        // Chinese line the Latin words and digits had colour fringes the
        // characters beside them did not, and read as another colour. A page
        // does not know the screen's subpixel order anyway, and the macOS
        // desktop draws grayscale too.
        cx.set_text_rendering_mode(TextRenderingMode::Grayscale);
        zedis_gui::init_embedded_commands();
        if let Err(e) = zedis_gui::db::init_database() {
            error!(error = %e, "the in-memory local store could not be created");
        }
        if let Some(seed) = page_seed {
            zedis_gui::states::seed_page_data(seed);
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

        if page_data_loaded && let Some(requests) = transport.take_refresh_requests() {
            follow_page_data(transport.clone(), requests, cx);
        }

        // The server list follows, the way it does on the desktop: the window
        // is already up, and the sidebar refreshes when the list lands. No
        // credential — the login cookie rides along on a same-origin fetch.
        //
        // Then it keeps following. The list is shared with every other
        // account, tab and script on this bridge, and this page has no other
        // way to learn that one of them changed it — the desktop owns its
        // file, the browser only borrows the bridge's. So the page asks again
        // every `SERVER_LIST_REFRESH`, skipping the rounds nobody would see,
        // and redraws only when the answer differs from what it holds. A
        // round that lands between this page's own save and that save's
        // re-list can carry a list a moment stale; the re-list corrects it
        // on the same path, so the worst case is one redraw too many. And it
        // asks at once when the page has just changed whose entries are whose
        // (an account deleted, what one left behind settled).
        if let Some(requests) = transport.take_server_list_requests() {
            follow_server_list(transport, requests, cx);
        }
    });
    APP.with(|slot| *slot.borrow_mut() = Some(handle));
    Ok(())
}

/// Wait for a loop's next round: `wait` gone by, or an ask on `requests` —
/// asks that came together are one round. Answers whether it was asked
/// for, or `None` once nothing can ask any more (the transport holds the
/// sender for the life of the page, so that is the page going).
#[cfg(target_family = "wasm")]
async fn next_round(cx: &AsyncApp, wait: Duration, requests: &mut UnboundedReceiver<()>) -> Option<bool> {
    let timer = cx.background_executor().timer(wait);
    let asked = match future::select(timer, requests.next()).await {
        Either::Left(_) => false,
        Either::Right((Some(()), _)) => true,
        Either::Right((None, _)) => return None,
    };
    while requests.try_recv().is_ok() {}
    Some(asked)
}

/// Fetch the server list, then keep it current: every `SERVER_LIST_REFRESH`
/// while someone is looking, and at once when asked.
#[cfg(target_family = "wasm")]
fn follow_server_list(transport: Arc<HttpBridgeTransport>, mut requests: UnboundedReceiver<()>, cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        let servers = transport.fetch_servers().await;
        let _ = cx.update(|cx| match servers {
            Ok(list) => {
                info!(count = list.len(), "server list from the bridge");
                apply_server_list(list, cx);
                apply_initial_web_deep_link(cx);
            }
            Err(e) => error!(error = %e, "the server list could not be fetched"),
        });
        while let Some(asked) = next_round(cx, SERVER_LIST_REFRESH, &mut requests).await {
            if !asked && unattended() {
                continue;
            }
            match transport.fetch_servers().await {
                Ok(list) => {
                    let _ = cx.update(|cx| apply_server_list(list, cx));
                }
                // A missed round is a missed round; the next one is a
                // minute away and the page keeps the list it has.
                Err(e) => info!(error = %e, "server list refresh skipped"),
            }
        }
    })
    .detach();
}

/// Keep the page data current with what other accounts change — the key
/// tags and notes, the saved scripts — the way the server list is kept
/// (ADR 17): every `PAGE_DATA_REFRESH` while someone is looking, and at once
/// when the page asks (`refresh_soon`: a server or the script library
/// opened, the page looked at again). The tag is kept only for an answer
/// that was taken in: one refused because this page changed something on
/// its way is asked for again, whole, soon after.
#[cfg(target_family = "wasm")]
fn follow_page_data(transport: Arc<HttpBridgeTransport>, mut requests: UnboundedReceiver<()>, cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        let mut etag: Option<String> = None;
        let mut wait = PAGE_DATA_REFRESH;
        while let Some(asked) = next_round(cx, wait, &mut requests).await {
            wait = PAGE_DATA_REFRESH;
            if !asked && unattended() {
                continue;
            }
            let generation = zedis_gui::states::page_data_generation();
            let answer = match transport.fetch_page_data(etag.clone()).await {
                Ok(Some(answer)) => answer,
                Ok(None) => continue,
                Err(e) => {
                    info!(error = %e, "page data refresh skipped");
                    continue;
                }
            };
            let seed = match transport::page_seed(&answer.json) {
                Ok(seed) => seed,
                Err(e) => {
                    error!(error = %e, "the bridge's page data could not be read");
                    continue;
                }
            };
            let taken = cx.update(|cx| zedis_gui::states::refresh_page_data(seed, generation, cx));
            if taken {
                etag = answer.etag;
            } else {
                wait = PAGE_DATA_RETRY;
            }
        }
    })
    .detach();
}

/// Take a server list from the bridge, and tell the views only if it differs
/// from the one they are drawing.
///
/// Compared as sets: the bridge answers in its own order and the cache hands
/// the list out sorted, so an order-sensitive comparison would call every
/// round a change and rebuild the sidebar once a minute for nothing. Both
/// sides came through the same transport, so secrets are the same
/// placeholders on both and the read-only marking is the same too.
#[cfg(target_family = "wasm")]
fn apply_server_list(list: Vec<RedisServer>, cx: &mut gpui::App) {
    let before: HashSet<RedisServer> = get_servers().unwrap_or_default().into_iter().collect();
    let after: HashSet<RedisServer> = list.iter().cloned().collect();
    if before == after {
        return;
    }
    set_servers_cache(list);
    // Tell the sidebar and the connection page to re-read the list.
    let store = cx.global::<ZedisGlobalStore>().clone();
    let state = store.state();
    state.update(cx, |_state, cx| cx.emit(GlobalEvent::ServerListUpdated));
}

/// If the page URL query string carries a deep link (`?server=...&db=...&key=...`
/// or `?zedis=...`), open that target once the initial server list has arrived.
#[cfg(target_family = "wasm")]
fn apply_initial_web_deep_link(cx: &mut gpui::App) {
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return;
    };
    match DeepLink::from_query_str(&search) {
        Ok(Some(link)) => {
            info!(server = %link.server, db = ?link.db, "opening web query deep link");
            let store = cx.global::<ZedisGlobalStore>().clone();
            store.update(cx, |state, cx| state.open_deep_link(link, cx));
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = %e, "web query deep link rejected");
        }
    }
}

/// The page's `visibilitychange`, from `index.html`. A hidden page polls the
/// bridge at the pace of a background workspace tab instead of the heartbeat
/// (`zedis_gui::helpers::pacing`): a browser tab is left open for days in a
/// way a desktop window is not, and each of its beats is work for a bridge
/// and a Redis that everybody shares. The heartbeat is not kicked when the
/// page comes back — the next tick, at most one interval away, goes through
/// — but the page data is asked for at once.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_page_visible(visible: bool) {
    set_unattended(!visible);
    // What other accounts changed while nobody looked.
    if visible && let Some(page_data) = bridge_page_data() {
        page_data.refresh_soon();
    }
}

// The page is the process: it lives until the tab closes, and so does this.
#[cfg(target_family = "wasm")]
thread_local! {
    static APP: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

/// How long the browser is given to start reading a download before its
/// object URL is let go of: revoked in the same turn as the click, some
/// browsers cancel it.
#[cfg(target_family = "wasm")]
const DOWNLOAD_URL_LIFETIME_MS: i32 = 60_000;

/// Hand the browser a file to save: a page has no save dialog, so it is the
/// browser's own download — a `Blob`'s object URL clicked through an
/// `<a download>`. Installed as `set_web_download`, behind every Export.
#[cfg(target_family = "wasm")]
fn download_file(name: &str, media_type: &str, contents: &[u8]) -> Result<(), String> {
    start_download(name, media_type, contents).map_err(|e| {
        error!(error = ?e, "the browser did not start the download");
        js_error_text(&e)
    })
}

#[cfg(target_family = "wasm")]
fn start_download(name: &str, media_type: &str, contents: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;
    use web_sys::js_sys::{Array, Uint8Array};
    use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

    let (window, document, body) = page()?;
    let options = BlobPropertyBag::new();
    options.set_type(media_type);
    let parts = Array::new();
    parts.push(&Uint8Array::from(contents));
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
    let url = Url::create_object_url_with_blob(&blob)?;
    let anchor: HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    anchor.set_href(&url);
    anchor.set_download(name);
    // In the document while it is clicked: a detached link downloads
    // nothing in some browsers.
    body.append_child(&anchor)?;
    anchor.click();
    anchor.remove();
    let revoke = Closure::once_into_js(move || {
        let _ = Url::revoke_object_url(&url);
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), DOWNLOAD_URL_LIFETIME_MS)?;
    Ok(())
}

/// Let the person pick a file: a hidden `<input type="file">`, clicked, and
/// the chosen file read whole — unless it is over `max_bytes`, which is
/// refused before anything is read. A picker closed without a choice
/// answers nothing (`cancel`), which the app reads as cancelled. Installed
/// as `set_web_upload`, behind every Import.
#[cfg(target_family = "wasm")]
fn pick_file(accept: &str, max_bytes: u64, done: PickDone) {
    let done = Rc::new(RefCell::new(Some(done)));
    if let Err(e) = start_pick(accept, max_bytes, done.clone()) {
        error!(error = ?e, "the browser did not open its file picker");
        if let Some(done) = done.borrow_mut().take() {
            done(Err(PickFailure::Unreadable(js_error_text(&e))));
        }
    }
}

#[cfg(target_family = "wasm")]
fn start_pick(accept: &str, max_bytes: u64, done: Rc<RefCell<Option<PickDone>>>) -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::HtmlInputElement;
    use web_sys::js_sys::Uint8Array;

    let (_, document, body) = page()?;
    let input: HtmlInputElement = document.create_element("input")?.dyn_into()?;
    input.set_type("file");
    input.set_accept(accept);
    input.style().set_property("display", "none")?;
    body.append_child(&input)?;

    let on_change = {
        let input = input.clone();
        let done = done.clone();
        Closure::once_into_js(move || {
            let file = input.files().and_then(|files| files.get(0));
            input.remove();
            let Some(done) = done.borrow_mut().take() else {
                return;
            };
            let Some(file) = file else {
                // Nothing chosen after all: as cancelled.
                return;
            };
            let size = file.size() as u64;
            if size > max_bytes {
                done(Err(PickFailure::TooLarge { size }));
                return;
            }
            wasm_bindgen_futures::spawn_local(async move {
                let read = JsFuture::from(file.array_buffer()).await;
                done(match read {
                    Ok(buffer) => Ok(PickedFile {
                        name: file.name(),
                        bytes: Uint8Array::new(&buffer).to_vec(),
                    }),
                    Err(e) => Err(PickFailure::Unreadable(js_error_text(&e))),
                });
            });
        })
    };
    let on_cancel = {
        let input = input.clone();
        Closure::once_into_js(move || {
            input.remove();
            // Dropping the answer is the cancel.
            done.borrow_mut().take();
        })
    };
    input.add_event_listener_with_callback("change", on_change.unchecked_ref())?;
    input.add_event_listener_with_callback("cancel", on_cancel.unchecked_ref())?;
    input.click();
    Ok(())
}

/// The page's window, document and body, for the two above.
#[cfg(target_family = "wasm")]
fn page() -> Result<(web_sys::Window, web_sys::Document, web_sys::HtmlElement), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsValue;
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window.document().ok_or_else(|| JsValue::from_str("no document"))?;
    let body = document.body().ok_or_else(|| JsValue::from_str("no body"))?;
    Ok((window, document, body))
}

/// A thrown JavaScript value as words: an `Error`'s message, else whatever
/// it prints as.
#[cfg(target_family = "wasm")]
fn js_error_text(value: &wasm_bindgen::JsValue) -> String {
    use wasm_bindgen::JsCast as _;
    value
        .dyn_ref::<web_sys::js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
