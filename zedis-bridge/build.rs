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

//! A release build embeds `zedis-web/www`, so the page has to be there first.
//!
//! `cargo build --release -p zedis-bridge` on a tree where `make web-release`
//! has not run would otherwise embed a `www/` without its module and ship a
//! bridge whose page cannot start — silently, because rust-embed is happy to
//! embed whatever the directory holds. A debug build reads the directory at
//! run time and is left alone: `make web-serve` may come before `make
//! web-bundle`.

use std::env;
use std::path::Path;

fn main() {
    // The `.gz`, because that is the form the module is embedded in: the raw
    // `.wasm` beside it is a build intermediate the embed leaves out.
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zedis-web/www/wasm/zedis_web_bg.wasm.gz");
    // Re-run when the bundle lands or changes; the derive's own
    // `include_bytes!` tracks the rest of the directory.
    println!("cargo:rerun-if-changed={}", wasm.display());
    // `PROFILE` is `release` for every profile that inherits release.
    if env::var("PROFILE").as_deref() == Ok("release") && !wasm.exists() {
        panic!(
            "zedis-bridge embeds zedis-web/www, and {} is missing: run `make web-release` first \
             (`make web-dist` does), or build the bridge in a debug profile",
            wasm.display()
        );
    }
}
