//! The Zedis desktop application, as a library.
//!
//! The binary next to this file is a five-line shim. The split exists so the
//! view and state layers — 63k lines of them — can be compiled into the
//! browser build as well, which a `[[bin]]`-only crate could never allow
//! (ADR 9).

// `run` is the desktop's `main`: the file logger, the single-instance socket,
// the tray, the menus and the database recovery window. The browser has its
// own entry in `zedis-web` and calls `launch` (ADR 9).
#[cfg(not(target_family = "wasm"))]
use crate::connection::install_crypto_provider;
use crate::connection::{get_server, get_servers};
use crate::db::LuaScriptManager;
#[cfg(not(target_family = "wasm"))]
use crate::db::{ProtoManager, ScriptManager, init_database, open_failure_kind};
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{
    CrashContext, InstanceMessage, InstanceRole, MultiSearchAction, UpdateAction, claim_instance,
    get_or_create_config_dir, init_logger, install_panic_hook, instance_messages, is_app_store_build, logs_dir,
    platform_info, post_instance_message, register_editing_rules, register_extra_languages, release_instance,
    set_configured_proxy, take_instance_server, take_pending_crash,
};
use crate::helpers::{
    MemuAction, PaletteAction, RecentKeysAction, ShortcutsAction, apply_default_ui_font_size, apply_fonts,
    load_keybinding_overrides, new_hot_keys, set_datetime_prefs, take_config_recoveries, with_app_identity,
};
use crate::states::{
    HINT_WELCOME, Route, ServerView, ZedisAppState, ZedisGlobalStore, flush_app_state_on_quit,
    update_app_state_and_save_quiet,
};
use crate::views::open_about_window;
use gpui::{App, WindowBounds, WindowOptions, prelude::*, px, size};
#[cfg(not(target_family = "wasm"))]
use gpui::{Bounds, Menu, MenuItem, OsAction};
#[cfg(not(target_family = "wasm"))]
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
use gpui::TitlebarOptions;
use gpui_kit::component::{Root, Theme};
#[cfg(not(target_family = "wasm"))]
use sys_locale::get_locale;
use tracing::{error, info, warn};

// Pointed at the empty `locales_stub/` so the macro embeds no translations at
// compile time; the lazy backend from `i18n_loader::runtime_backend` inflates
// a locale on its first `t!` lookup (desktop: rust-embed; browser: a fetch
// from `/locales/<lang>.toml`). See `src/i18n_loader.rs`.
rust_i18n::i18n!(
    "locales_stub",
    fallback = "en",
    backend = crate::i18n_loader::runtime_backend()
);

pub mod assets;
pub mod components;
pub mod connection;
pub mod constants;
pub mod db;
pub mod dialogs;
pub mod error;
pub mod helpers;
pub mod i18n_loader;
pub mod root;
pub mod startup;
pub mod states;
// An OS allow-list would follow the build into the browser ("not linux"), so
// the family gate is the outer one — the same shape the `tray-icon`
// dependency carries in Cargo.toml.
#[cfg(all(not(target_family = "wasm"), not(target_os = "linux")))]
pub mod tray;
pub mod views;
#[cfg(target_family = "wasm")]
pub mod web_fetch;
pub mod window_setup;
#[cfg(not(target_family = "wasm"))]
use crate::dialogs::*;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{DiagnosticsAction, WindowAction};
use crate::root::*;
use crate::startup::*;
use crate::window_setup::*;

/// Everything `main` does. Lives here so the views and states above it
/// are reachable from another crate — the browser build needs them, and a
/// binary crate cannot be depended on (ADR 9).
#[cfg(not(target_family = "wasm"))]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Held for the whole run so the non-blocking file logger keeps flushing.
    let _log_guard = init_logger()?;
    // Crash context first, before anything that can panic: release builds
    // abort on panic, so the report the hook writes is the only trace left.
    let info = platform_info();
    let os = info.os_label();
    let arch = info.architecture;
    install_panic_hook(CrashContext {
        version: VERSION,
        git_sha: GIT_SHA,
        os: os.clone(),
        arch: arch.clone(),
    });
    // Before anything can dial: the first TLS connection — a `rediss://`
    // server, the update check — panics without a provider (see the fn).
    install_crypto_provider();
    let config_dir = if let Ok(dir) = get_or_create_config_dir() {
        dir.to_string_lossy().to_string()
    } else {
        "--".to_string()
    };
    info!(
        version = VERSION,
        git_sha = GIT_SHA,
        os,
        arch,
        config_dir,
        is_app_store_build = is_app_store_build(),
        sys_locale = ?get_locale(),
        "zedis launch"
    );
    if is_smoke_test() {
        // Smoke watchdog: in the 0.4.5/0.4.6 Windows failure mode (hidden
        // window never receives WM_PAINT) no frame ever paints, so the
        // success hook below never fires — turn that hang into a distinct
        // failing exit code instead of a stuck CI job.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(30));
            eprintln!("ZEDIS_SMOKE_TIMEOUT: no frame painted within 30s");
            std::process::exit(2);
        });
    }
    // Register tree-sitter languages we want the code editor to
    // highlight beyond the JSON-only default. Today this is just Lua
    // (Functions / EVAL editors); add others by extending the helper.
    register_extra_languages();
    init_embedded_commands();
    let app = gpui_platform::application().with_assets(assets::Assets);
    // redis:// links: macOS delivers them as Apple Events through the
    // registered scheme (`osx_url_schemes`) — possibly before the window
    // exists, so they queue for `launch`. The other platforms start a second
    // process with the link as an argument, which hands it off the same way.
    app.on_open_urls(|urls| post_instance_message(InstanceMessage { urls }));
    let app_state = ZedisAppState::try_new().unwrap_or_else(|e| {
        error!(error = %e, "zedis.toml could not be loaded; starting with defaults");
        ZedisAppState::new()
    });
    if let Err(e) = get_servers() {
        error!(error = %e, "get servers fail",);
    }
    // One Zedis per profile: a second launch hands its arguments (a
    // redis:// link, or nothing — "bring the window up") to the running
    // instance and exits, instead of tripping over the database lock.
    if claim_instance(&InstanceMessage { urls: cli_redis_urls() }) == InstanceRole::Forwarded {
        return Ok(());
    }
    if let Err(e) = init_database() {
        let failure = open_failure_kind(&e);
        error!(error = %e, failure = ?failure, "init database failed; showing the recovery window");
        // Don't start a half-broken instance (the DB is required for tags /
        // history / proto / script / Lua). The window explains the cause and,
        // for a damaged or too-new file, offers to move it aside and rebuild.
        app.run(move |cx| {
            gpui_kit::component::init(cx);
            // Match the user's chosen mode, or the OS appearance, so the error
            // window isn't a jarring light flash on a dark system.
            let mode = match app_state.theme() {
                Some(m) => m,
                None => theme_mode_for_appearance(cx.window_appearance()),
            };
            Theme::change(mode, None, cx);
            apply_default_ui_font_size(cx);
            cx.activate(true);
            let bounds = Bounds::centered(None, size(px(540.), px(300.)), cx);
            let opened = cx.open_window(
                with_app_identity(WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(440.), px(240.))),
                    ..Default::default()
                }),
                |window, cx| {
                    window.on_window_should_close(cx, |_window, cx| {
                        cx.quit();
                        true
                    });
                    let view = cx.new(|_| DatabaseErrorView::new(failure, app_state));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            if opened.is_err() {
                cx.quit();
            }
        });
        return Ok(());
    }
    init_caches();
    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_kit::component::init(cx);
        // Editing rules (auto-close pairs, smart indent) for the languages
        // registered above; `init` installs the provider they hang off.
        register_editing_rules(cx);
        launch(cx, app_state);
    });
    Ok(())
}

/// Hand the embedded command metadata to the connection crate — it has no
/// access to the app's asset bundle (see command.rs). Both entry points call
/// this before anything can build a command. In the browser the file is not
/// embedded; [`fetch_commands_json`] loads it after the first frame.
pub fn init_embedded_commands() {
    if let Some(file) = assets::Assets::get("commands.json") {
        crate::connection::init_commands_json(file.data.to_vec());
    }
}

#[cfg(not(target_family = "wasm"))]
fn register_embedded_mono_fonts(cx: &mut App) {
    let fonts = ["fonts/JetBrainsMono-Regular.ttf", "fonts/JetBrainsMono-Bold.ttf"]
        .into_iter()
        .filter_map(|p| assets::Assets::get(p).map(|f| f.data))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        error!(error = %e, "failed to register bundled fonts");
    }
}

/// Pull both JetBrains faces, then register them together so Regular/Bold
/// land in the same `add_fonts` call. A missing file is skipped; an empty
/// result leaves the fallback family in place.
#[cfg(target_family = "wasm")]
fn fetch_mono_fonts(cx: &App) {
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};

    let collected = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let pending = Arc::new(Mutex::new(2_u8));
    for name in ["JetBrainsMono-Regular.ttf", "JetBrainsMono-Bold.ttf"] {
        let collected = collected.clone();
        let pending = pending.clone();
        web_fetch::get_bytes(cx, &format!("fonts/{name}"), move |cx, bytes| {
            if let Some(bytes) = bytes {
                collected.lock().unwrap_or_else(|p| p.into_inner()).push(bytes);
            } else {
                warn!(name, "JetBrains Mono face could not be fetched");
            }
            let left = {
                let mut n = pending.lock().unwrap_or_else(|p| p.into_inner());
                *n = n.saturating_sub(1);
                *n
            };
            if left > 0 {
                return;
            }
            let fonts = std::mem::take(&mut *collected.lock().unwrap_or_else(|p| p.into_inner()));
            if fonts.is_empty() {
                return;
            }
            let cows: Vec<Cow<'static, [u8]>> = fonts.into_iter().map(Cow::Owned).collect();
            if let Err(e) = cx.text_system().add_fonts(cows) {
                error!(error = %e, "failed to register fetched mono fonts");
            }
            cx.refresh_windows();
        });
    }
}

#[cfg(target_family = "wasm")]
fn fetch_commands_json(cx: &App) {
    web_fetch::get_bytes(cx, "assets/commands.json", |cx, bytes| match bytes {
        Some(bytes) => {
            crate::connection::init_commands_json(bytes);
            cx.refresh_windows();
        }
        None => warn!("commands.json could not be fetched"),
    });
}

/// Fills the in-memory proto / script / Lua caches from the local database.
/// Runs synchronously before the window opens — they back the proto/script/
/// Lua editor tables, and loading them in a post-window background task (the
/// old placement) raced: an editor restored as the startup route read an
/// empty cache and showed no rows until the view was recreated.
/// A second launch reached the running instance: bring the window up and
/// open its link, if it carried one.
#[cfg(not(target_family = "wasm"))]
fn activate_from_instance(message: InstanceMessage, cx: &mut App) {
    cx.activate(true);
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
    if !message.urls.is_empty() {
        open_redis_urls(message.urls, cx);
    }
}

pub fn init_caches() {
    // Proto descriptors and script viewers are read off disk and run as
    // processes — the browser build has neither (ADR 9).
    #[cfg(not(target_family = "wasm"))]
    if let Err(e) = ProtoManager::init() {
        error!(error = %e, "init protos fail");
    }
    #[cfg(not(target_family = "wasm"))]
    if let Err(e) = ScriptManager::init() {
        error!(error = %e, "init script viewers fail");
    }
    if let Err(e) = LuaScriptManager::init() {
        error!(error = %e, "init lua scripts fail");
    }
}

/// The normal startup once the local database is usable: theme, fonts, global
/// store, menus, hot keys, and the main window. Called from `main` directly,
/// or from the database recovery window after a successful rebuild — so it
/// must not assume anything about which windows already exist.
pub fn launch(cx: &mut App, app_state: ZedisAppState) {
    // Register the bundled JetBrains Mono faces (assets/fonts/*.ttf) so the
    // monospace family (`get_mono_font_family()`) renders real Regular / Bold
    // weights on every platform instead of leaning on whatever the OS ships
    // — see the "Bold needs a concrete font family" gotcha in CLAUDE.md.
    // The browser fetches those files after the first frame: they are ~0.5 MiB
    // uncompressed and would sit in the wasm (no zstd inflater there).
    #[cfg(not(target_family = "wasm"))]
    register_embedded_mono_fonts(cx);
    #[cfg(target_family = "wasm")]
    {
        fetch_mono_fonts(cx);
        fetch_commands_json(cx);
    }
    // Register the embedded color themes so they appear in the theme menu.
    assets::register_themes(cx);

    cx.activate(true);
    let (window_bounds, maximized) = resolve_window_bounds(&app_state, cx);
    info!(bounds = ?window_bounds, maximized, "resolved window bounds");
    let app_state = cx.new(|_| app_state);
    let app_store = ZedisGlobalStore::new(app_state);
    // Fonts first: `apply_fonts` records the families and writes them into
    // the light / dark theme slots as well as the live theme, so the theme
    // application below keeps them (a named theme puts them back itself).
    {
        let (ui_font, mono_font) = {
            let store = app_store.read(cx);
            (store.ui_font_family(), store.mono_font_family())
        };
        apply_fonts(cx, ui_font.as_deref(), mono_font.as_deref());
    }
    // A saved named theme wins; otherwise fall back to the Light/Dark/System
    // mode (resolved against the OS appearance by the renderer).
    let saved_theme_name = app_store.read(cx).theme_name();
    let saved_mode = app_store.read(cx).theme();
    let applied = match saved_theme_name {
        Some(name) => apply_named_theme(&name, cx),
        None => false,
    };
    if !applied {
        // Resolve System mode (no saved name/mode) against the OS appearance
        // *before* the window opens, so the very first painted frame already
        // uses the right light/dark theme. Otherwise the default theme shows
        // for a frame and flashes (e.g. white before a dark theme settles).
        let mode = match saved_mode {
            Some(m) => m,
            None => theme_mode_for_appearance(cx.window_appearance()),
        };
        Theme::change(mode, None, cx);
    }
    // Theme::change / apply_named_theme reset font_size to stock 16; pin
    // the app rem base before the first frame (Root reads theme.font_size).
    apply_default_ui_font_size(cx);
    cx.set_global(app_store);
    // From here on every exit path flushes the state on the way out; nothing
    // else needs to remember to.
    flush_app_state_on_quit(cx);
    // Mirror the persisted proxy setting into helpers::proxy before the
    // startup update check fires — its HTTP runs on background threads
    // that can't read the store.
    {
        #[cfg(not(target_family = "wasm"))]
        {
            let proxy = cx.global::<ZedisGlobalStore>().read(cx).http_proxy();
            set_configured_proxy(&proxy);
        }
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        set_datetime_prefs(store.time_zone(), &store.date_format());
    }
    #[cfg(all(not(target_family = "wasm"), not(target_os = "linux")))]
    {
        let tray_enabled = cx.global::<ZedisGlobalStore>().read(cx).tray_enabled();
        if tray_enabled {
            tray::init_tray(cx);
        }
    }
    // The user's `keybindings.toml` overrides must be in place before the
    // keymap is bound — there is no rebinding later.
    let overridden = load_keybinding_overrides();
    if overridden > 0 {
        info!(overridden, "keybinding overrides loaded");
    }
    cx.bind_keys(new_hot_keys());
    cx.on_action(|e: &MemuAction, cx: &mut App| match e {
        MemuAction::Quit => {
            cx.quit();
        }
        MemuAction::About => {
            open_about_window(cx);
        }
        MemuAction::Close => {
            // ⌘W / Ctrl+W mirrors the red close button: on macOS it hides
            // the app (see on_window_should_close); elsewhere it closes the
            // active window (which quits the app when it's the last one).
            #[cfg(target_os = "macos")]
            cx.hide();
            #[cfg(not(target_os = "macos"))]
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, _cx| window.remove_window());
            }
        }
        MemuAction::OpenLogs => {
            #[cfg(not(target_family = "wasm"))]
            match logs_dir() {
                // `logs_dir` creates the directory, so it exists even before any
                // log line has been written.
                Some(logs) => cx.open_with_system(&logs),
                None => error!("failed to resolve logs directory"),
            }
            #[cfg(target_family = "wasm")]
            warn!("there is no logs folder in the browser build");
        }
    });
    #[cfg(not(target_family = "wasm"))]
    {
        let mut menu_items = vec![MenuItem::action("About Zedis", MemuAction::About)];
        // App Store builds update via the App Store — hide the manual check.
        if !is_app_store_build() {
            menu_items.push(MenuItem::action("Check for Updates", UpdateAction::Check));
        }
        menu_items.extend([
            MenuItem::action("Open Logs Folder", MemuAction::OpenLogs),
            MenuItem::action("Export Diagnostics…", DiagnosticsAction::Export),
            MenuItem::action("Close Window", MemuAction::Close),
            MenuItem::action("Quit", MemuAction::Quit),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Zedis".into(),
                items: menu_items,
                disabled: false,
            },
            // The standard Edit menu: macOS routes these to the focused text
            // field through the OS actions, and the inputs' own bindings answer
            // the shortcuts the menu shows.
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::os_action("Undo", Undo, OsAction::Undo),
                    MenuItem::os_action("Redo", Redo, OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("Cut", Cut, OsAction::Cut),
                    MenuItem::os_action("Copy", Copy, OsAction::Copy),
                    MenuItem::os_action("Paste", Paste, OsAction::Paste),
                    MenuItem::separator(),
                    MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                ],
                disabled: false,
            },
            Menu {
                name: "Window".into(),
                items: vec![
                    MenuItem::action("Minimize", WindowAction::Minimize),
                    MenuItem::action("Zoom", WindowAction::Zoom),
                    MenuItem::action("Toggle Full Screen", WindowAction::ToggleFullscreen),
                    MenuItem::separator(),
                    MenuItem::action("Close Window", MemuAction::Close),
                ],
                disabled: false,
            },
        ]);
    }

    #[cfg(not(target_family = "wasm"))]
    install_host_key_prompt(cx);

    // Hand-offs from a second launch and redis:// links from the OS both
    // queue in the instance inbox; drain it here, on the foreground.
    #[cfg(not(target_family = "wasm"))]
    {
        if let Some(server) = take_instance_server() {
            server.serve(post_instance_message);
        }
        let inbox = instance_messages();
        cx.spawn(async move |cx| {
            while let Ok(message) = inbox.recv().await {
                cx.update(|cx| activate_from_instance(message, cx));
            }
        })
        .detach();
        cx.on_app_quit(|_cx| async { release_instance() }).detach();
    }

    cx.spawn(async move |cx| {
        cx.open_window(
            with_app_identity(WindowOptions {
                window_bounds: Some(if maximized {
                    WindowBounds::Maximized(window_bounds)
                } else {
                    WindowBounds::Windowed(window_bounds)
                }),
                // macOS / Windows: custom-drawn title bar (transparent OS chrome).
                // Linux: server-side decorations show the title from
                // `with_app_identity` ("Zedis") — see issue #106.
                #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
                }),
                // macOS only: create the window hidden and reveal it after
                // the first themed frame (see on_next_frame below) so
                // there's no white flash before the theme paints. Windows
                // must be shown immediately — its frames are driven by
                // WM_PAINT, which hidden windows never receive, so the
                // "reveal on first frame" deadlocks and the window never
                // appears (the 0.4.5/0.4.6 auto-hide bug). Its backing is
                // a black brush, so there's no white flash to hide anyway.
                // Linux too — Wayland can't reliably reveal a window that
                // was never mapped.
                show: cfg!(not(target_os = "macos")),
                window_min_size: Some(size(px(600.), px(400.))),
                ..Default::default()
            }),
            |window, cx| {
                #[cfg(target_os = "macos")]
                window.on_window_should_close(cx, move |_window, cx| {
                    cx.hide();
                    false
                });
                // Reveal the (hidden) window only after the first frame has
                // painted, so the user never sees the default white backing
                // before the themed background. Pairs with `show: false`.
                // macOS only: it paints hidden windows; Windows/Linux never
                // deliver a frame to an unmapped window, so this callback
                // would never fire there (see the `show:` comment above).
                #[cfg(target_os = "macos")]
                window.on_next_frame(|window, _cx| window.activate_window());
                // CI smoke mode: a painted first frame is exactly the
                // signal the 0.4.5/0.4.6 Windows regression killed
                // (hidden windows never get WM_PAINT, so no frame ever
                // comes). Success → exit 0; the watchdog in `main`
                // turns "no frame" into exit 2.
                if is_smoke_test() {
                    // Stage 1: the window exists. Printed on every platform
                    // (useful in the log either way); with the `window` gate
                    // it is also the success signal, after a grace period
                    // long enough for a startup panic to surface.
                    println!("ZEDIS_SMOKE_WINDOW");
                    if smoke_gate_is_window() {
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_secs(5));
                            println!("ZEDIS_SMOKE_OK (window gate)");
                            std::process::exit(0);
                        });
                    }
                    // Stage 2: a painted frame — the real signal wherever a
                    // display exists.
                    window.on_next_frame(|_window, _cx| {
                        println!("ZEDIS_SMOKE_OK");
                        std::process::exit(0);
                    });
                    // Note for CI: under Xvfb + llvmpipe this hook has
                    // never fired — the frame-present signal doesn't
                    // arrive in that environment even with redraws
                    // forced every second (verified 2026-07-17), so the
                    // Linux smoke step runs best-effort until that's
                    // understood upstream. Real displays (macOS /
                    // Windows runners, actual Linux desktops) fire it
                    // normally.
                }
                let zedis_view = cx.new(|cx| Zedis::new(window, cx));
                // Activate the target connection + view now that the views
                // are subscribed. Deep-link launch args (`--server
                // <id|name>`, `--db <n>`, `--route <view>`) override the
                // remembered state piecewise; with no args this restores
                // the last session. The re-select makes the emitted
                // ServerSelected load the connection (content) and
                // highlight its sidebar row.
                {
                    let store = cx.global::<ZedisGlobalStore>().clone();
                    store.update(cx, |state, cx| {
                        let cli_server = cli_server_override();
                        let cli_db = cli_db_override();
                        // Target connection: the CLI server wins; else the
                        // remembered one, validated (it may have been
                        // deleted since the last run).
                        let target: Option<(String, usize)> = match &cli_server {
                            Some(id) => Some((id.clone(), cli_db.unwrap_or_else(|| state.open_db_for(id)))),
                            None => state
                                .selected_server()
                                .cloned()
                                .filter(|(id, _)| get_server(id).is_ok())
                                // A pinned DB wins over the remembered one
                                // here too, so a restart lands where a
                                // sidebar click would; `--db` still wins
                                // over both.
                                .map(|(id, db)| {
                                    let db = cli_db.unwrap_or_else(|| state.open_db_from(&id, db));
                                    (id, db)
                                }),
                        };
                        // Target view: explicit --route wins; a bare
                        // --server implies the editor (a deep link should
                        // land on that server, not whatever page was
                        // persisted); otherwise the restored route's view.
                        let view = match (cli_route_override(), cli_server.is_some()) {
                            (Some(CliRoute::App(route)), _) => {
                                state.activate(route, cx);
                                return;
                            }
                            (Some(CliRoute::View(view)), _) => Some(view),
                            (None, true) => Some(ServerView::Editor),
                            (None, false) => state.route().server_view(),
                        };
                        let route = match (view, target) {
                            (Some(view), Some((id, db))) => Route::Server {
                                id: id.into(),
                                db,
                                view,
                            },
                            (Some(_), None) => {
                                warn!("server view requested but no valid server available; opening Home");
                                Route::Home
                            }
                            // Restored app-level route (or Home).
                            (None, _) => state.route(),
                        };
                        state.activate(route, cx);
                    });
                }
                // A redis:// link on the command line lands on that server,
                // over the restored / deep-linked route above.
                open_redis_urls(cli_redis_urls(), cx);
                // Global (focus-independent) ⌘K handler — element
                // `.on_action` is focus-routed and dies when the
                // palette closes and orphans its focus handle.
                let weak_zedis = zedis_view.downgrade();
                cx.on_action(move |_: &PaletteAction, cx: &mut App| {
                    if let Some(view) = weak_zedis.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_command_palette(cx));
                    }
                });
                // Global (focus-independent) ⌘P — recent keys Quick Open.
                let weak_zedis_recent = zedis_view.downgrade();
                cx.on_action(move |_: &RecentKeysAction, cx: &mut App| {
                    if let Some(view) = weak_zedis_recent.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_recent_keys_palette(cx));
                    }
                });
                // Global (focus-independent) ⌘⇧F — multi-database search.
                #[cfg(not(target_family = "wasm"))]
                {
                    let weak_zedis_multi_search = zedis_view.downgrade();
                    cx.on_action(move |_: &MultiSearchAction, cx: &mut App| {
                        if let Some(view) = weak_zedis_multi_search.upgrade() {
                            view.update(cx, |zedis, cx| zedis.toggle_multi_search(cx));
                        }
                    });
                }
                // Global (focus-independent) ⌘/ handler — same
                // rationale as the ⌘K handler above; also the target
                // of the command palette's "Keyboard Shortcuts" row.
                let weak_zedis_shortcuts = zedis_view.downgrade();
                cx.on_action(move |_: &ShortcutsAction, cx: &mut App| {
                    if let Some(view) = weak_zedis_shortcuts.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_shortcuts(cx));
                    }
                });
                #[cfg(not(target_family = "wasm"))]
                {
                    // Manual "Check for Updates" (app menu) — focus-independent
                    // like the handlers above so it works regardless of focus.
                    let weak_zedis_update = zedis_view.downgrade();
                    cx.on_action(move |e: &UpdateAction, cx: &mut App| {
                        let Some(view) = weak_zedis_update.upgrade() else {
                            return;
                        };
                        match e {
                            UpdateAction::Check => {
                                view.update(cx, |zedis, cx| zedis.check_for_updates(true, false, cx));
                            }
                            // Title-bar chip click: open the prompt straight from
                            // the store. The check that lit the chip already
                            // fetched everything the dialog shows — version,
                            // changelog, the per-arch asset — so re-fetching here
                            // only made the chip spin for a beat before anything
                            // appeared. The fallback fetch is for the (unreachable)
                            // case of a chip with no cached update behind it.
                            UpdateAction::OpenPrompt => {
                                let cached = cx.global::<ZedisGlobalStore>().read(cx).available_update();
                                view.update(cx, |zedis, cx| match cached {
                                    Some(info) => {
                                        zedis.desktop.pending_update = Some(info);
                                        cx.notify();
                                    }
                                    None => zedis.check_for_updates(true, true, cx),
                                });
                            }
                        }
                    });
                    // Silent startup check: at most one per `UPDATE_CHECK_INTERVAL`,
                    // skippable per-version, only if enabled. The update chip lives
                    // in the always-visible title bar, so this can run on any route.
                    let auto_due = {
                        let store = cx.global::<ZedisGlobalStore>().read(cx);
                        store.auto_update_check() && store.update_check_due()
                    };
                    if auto_due {
                        zedis_view.update(cx, |zedis, cx| zedis.check_for_updates(false, false, cx));
                    }
                }
                // Config files damaged at startup (`zedis.toml` /
                // `redis-servers.toml`) were quarantined before the window
                // existed; surface the outcome now that there is one.
                zedis_view.update(cx, |zedis, _| {
                    zedis.pending_config_recoveries = take_config_recoveries();
                    #[cfg(not(target_family = "wasm"))]
                    {
                        zedis.desktop.pending_crash = take_pending_crash();
                    }
                });
                // One-shot welcome card, and only for a truly fresh start
                // (no server configured — an upgrading user needs no tour).
                // Dismissed on this first evaluation either way, so it can
                // never pop up later (e.g. after deleting every server).
                if !cx.global::<ZedisGlobalStore>().read(cx).hint_dismissed(HINT_WELCOME) {
                    update_app_state_and_save_quiet(cx, "dismiss_hint_welcome", |state, _| {
                        state.dismiss_hint(HINT_WELCOME)
                    });
                    // An unreadable config counts as configured: don't greet
                    // an existing user just because the file failed to parse.
                    if get_servers().map(|servers| servers.is_empty()).unwrap_or(false) {
                        zedis_view.update(cx, |zedis, _| zedis.pending_welcome = true);
                    }
                }
                cx.new(|cx| Root::new(zedis_view, window, cx))
            },
        )?;

        Ok::<_, anyhow::Error>(())
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use crate::connection::install_crypto_provider;

    /// Both rustls backends are in this binary's build — ring through the
    /// Redis stack, aws-lc-rs through gpui-kit's HTTP client — and rustls
    /// panics on `ClientConfig::builder()` until one is picked. This crate
    /// is where they meet, so this is where it is checked.
    #[test]
    fn rustls_has_a_process_level_crypto_provider() {
        install_crypto_provider();
        // Panics ("Could not automatically determine the process-level
        // CryptoProvider") without the install above.
        let _ = rustls::ClientConfig::builder();
    }
}
