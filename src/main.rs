#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The desktop entry point. Everything it does lives in the library beside
//! it, so the same code can be built for the browser.

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    zedis_gui::run()
}

/// The browser has no `main`: its entry point is `zedis-web`, which calls
/// into this crate as a library (ADR 9). This exists only so the workspace's
/// bin target parses under a wasm check.
#[cfg(target_family = "wasm")]
fn main() {}
