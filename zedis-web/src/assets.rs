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

//! Two asset sources behind one door.
//!
//! The application's own files — its icons, themes, fonts and `commands.json`
//! — are embedded in the wasm and answer at once. The component kit's icons
//! are not: on the desktop the kit embeds them, but its browser build fetches
//! them from `/assets/icons/…` on demand, answering with an error until the
//! download lands. GPUI takes one `AssetSource`, so this is the one: the
//! embedded set first, the fetched set behind it (ADR 9).
//!
//! **And nothing tells the window when an icon lands.** The kit's source puts
//! the bytes in its cache and stops there, so the icon appears only if
//! something else happens to repaint afterwards. A first visit hides that —
//! the welcome dialog opens, the server list arrives — but a quiet screen
//! keeps its holes: a reload, once revalidation made icons come back in a
//! millisecond, drew a home page with no plus sign on its button. So this
//! source remembers what it was asked for and could not give, and
//! [`repaint_when_icons_land`] watches that list and refreshes the windows
//! when an entry starts answering.

use gpui::{App, AssetSource, Result, SharedString};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Paths asked for and not yet answered, with how many times each has been
/// looked at since.
pub type Awaited = Arc<Mutex<HashMap<String, u32>>>;

/// How often the watcher looks while something is awaited, and while nothing is.
const WATCH_BUSY: Duration = Duration::from_millis(60);
const WATCH_IDLE: Duration = Duration::from_millis(500);

/// After this many looks an icon is given up on. Looking is not free — the
/// kit's source *starts a fetch* whenever it is asked for something it has
/// neither got nor is getting, so an icon that 404s would otherwise be
/// requested again on every look, for as long as the page is open.
const WATCH_LOOKS: u32 = 150;

pub struct WebAssets {
    app: zedis_gui::assets::Assets,
    kit: gpui_kit::assets::Assets,
    awaited: Awaited,
}

impl WebAssets {
    /// `endpoint` is the origin the kit fetches from; empty means this one.
    pub fn new(endpoint: &str) -> Self {
        Self {
            app: zedis_gui::assets::Assets,
            kit: gpui_kit::assets::Assets::new(endpoint.to_string()),
            awaited: Awaited::default(),
        }
    }

    /// The list [`repaint_when_icons_land`] watches.
    pub fn awaited(&self) -> Awaited {
        self.awaited.clone()
    }
}

/// Refresh every window when an icon that was asked for arrives.
pub fn repaint_when_icons_land(awaited: Awaited, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let busy = awaited.lock().map(|list| !list.is_empty()).unwrap_or(false);
            cx.background_executor()
                .timer(if busy { WATCH_BUSY } else { WATCH_IDLE })
                .await;
            let paths: Vec<String> = match awaited.lock() {
                Ok(list) => list.keys().cloned().collect(),
                Err(_) => return,
            };
            if paths.is_empty() {
                continue;
            }
            let awaited = awaited.clone();
            cx.update(move |cx| {
                let source = cx.asset_source().clone();
                // Asking again is what finds out; it also re-lists a path
                // that is still not there, which is why the list is edited
                // only after every question has been asked.
                let landed: Vec<&String> = paths
                    .iter()
                    .filter(|path| matches!(source.load(path), Ok(Some(_))))
                    .collect();
                if let Ok(mut list) = awaited.lock() {
                    for path in &landed {
                        list.remove(*path);
                    }
                    list.retain(|_, looks| {
                        *looks += 1;
                        *looks < WATCH_LOOKS
                    });
                }
                if !landed.is_empty() {
                    cx.refresh_windows();
                }
            });
        }
    })
    .detach();
}

impl AssetSource for WebAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // The app's source reports a miss as an error; either way the kit's
        // source gets the question next.
        match self.app.load(path) {
            Ok(Some(data)) => Ok(Some(data)),
            _ => {
                let answer = self.kit.load(path);
                if !matches!(answer, Ok(Some(_)))
                    && let Ok(mut list) = self.awaited.lock()
                {
                    list.entry(path.to_string()).or_insert(0);
                }
                // The kit answers an icon it is still fetching with an
                // *error* ("Wasm assets loading, will be available soon..."),
                // and GPUI's `Svg::paint` ends in `.log_err()`: one ERROR
                // line per pending icon per frame, on every page load, for
                // something that is not a failure. "Not there" is `Ok(None)`
                // — GPUI paints nothing and asks again on the next paint (no
                // atlas caches a `None`), and the watcher above supplies that
                // paint. A fetch that really fails is still reported, once,
                // by the kit's own `warn!` with the path and the status.
                answer.or(Ok(None))
            }
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut files = self.app.list(path)?;
        files.extend(self.kit.list(path)?);
        Ok(files)
    }
}
