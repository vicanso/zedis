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

//! Tree-sitter language registration for the code editor.
//!
//! gpui-component ships highlight queries for ~30 languages but only
//! behind its `tree-sitter-languages` feature, which pulls in the full
//! parser bundle (8-15MB binary growth). We don't want that for one
//! niche editor view, so we instead register just the parsers we
//! actually need at startup, via the public `LanguageRegistry` API.
//!
//! Today that's only Lua (for the Functions / EVAL editors). Add more
//! by following the same pattern — pull the parser crate, call
//! `register_*`, the rest is free.

use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};

/// Wire up every extra tree-sitter language we want at runtime.
/// Idempotent — `register` overwrites by name, so calling twice is
/// harmless.
pub fn register_extra_languages() {
    register_lua();
}

/// `highlights.scm` for Lua, copied verbatim from gpui-component's
/// curated `crates/ui/src/highlighter/languages/lua/highlights.scm`
/// (Apache-2 licensed). The upstream `tree_sitter_lua::HIGHLIGHTS_QUERY`
/// uses Neovim's richer capture vocabulary (`@conditional`, `@repeat`,
/// `@function.builtin`, etc.) — none of which gpui-component's flat
/// `SyntaxColors` theme maps to a color, so `if` / `else` / `for` /
/// builtin functions would render uncolored. The curated query
/// projects every interesting token onto the small set the renderer
/// actually understands.
const LUA_HIGHLIGHTS: &str = include_str!("lua_highlights.scm");

fn register_lua() {
    // INJECTIONS_QUERY / LOCALS_QUERY come straight from the crate —
    // their capture vocabulary doesn't intersect with theme color
    // mappings (they drive embedded-language detection and scope
    // analysis), so the upstream copies are fine.
    let config = LanguageConfig::new(
        "lua",
        tree_sitter::Language::new(tree_sitter_lua::LANGUAGE),
        Vec::new(),
        LUA_HIGHLIGHTS,
        tree_sitter_lua::INJECTIONS_QUERY,
        tree_sitter_lua::LOCALS_QUERY,
    );
    LanguageRegistry::singleton().register("lua", &config);
}
