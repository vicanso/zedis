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
//!
//! The grammar is only half of a language: what the editor does on a
//! typed bracket or quote and on Enter comes from a separate editing
//! config (`gpui_kit::component::input::language_config`), which
//! gpui-component ships for text / json / python only. Everything else
//! gets generic `()[]{}""''` pairing and bracket-only indentation, so a
//! language with keyword blocks registers its rules here too.

use gpui::App;
use gpui_kit::component::highlighter::{GrammarConfig, LanguageRegistry};
use gpui_kit::component::input::language_config::LanguageConfig;
use gpui_kit::component::input::{IndentationRules, set_language_config};
use regex::Regex;

/// Wire up every extra tree-sitter language we want at runtime.
/// Idempotent — `register` overwrites by name, so calling twice is
/// harmless.
pub fn register_extra_languages() {
    register_lua();
}

/// Editing rules for the languages above. Needs the app because the
/// provider the rules hang off is installed by
/// `gpui_kit::component::init`; call it right after that.
pub fn register_editing_rules(cx: &mut App) {
    set_language_config("lua", lua_editing_config(), cx);
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
    let config = GrammarConfig::new(
        "lua",
        tree_sitter::Language::new(tree_sitter_lua::LANGUAGE),
        Vec::new(),
        LUA_HIGHLIGHTS,
        tree_sitter_lua::INJECTIONS_QUERY,
        tree_sitter_lua::LOCALS_QUERY,
    );
    LanguageRegistry::singleton().register("lua", &config);
}

/// A Lua line (up to the cursor, trailing whitespace trimmed) that opens a
/// block: it ends with `then` / `do` / `else` / `repeat`, a function header,
/// or an opening bracket, optionally followed by a `--` comment. The header
/// alternative stops at the parameter list's `)`, so an inline
/// `pcall(function() ... end)` does not count.
const LUA_OPENS_BLOCK: &str = r"^\s*(?:.*\b(?:then|do|else|repeat)|.*\bfunction\b[^)]*\)|.*[{(\[])\s*(?:--.*)?$";

/// The rest of a Lua line (from the cursor on) that closes a block, so the
/// new line it lands on is outdented.
const LUA_CLOSES_BLOCK: &str = r"^\s*(?:(?:end|else|elseif|until)\b|[})\]])";

fn lua_opens_block() -> Regex {
    Regex::new(LUA_OPENS_BLOCK).expect("LUA_OPENS_BLOCK is a valid regex")
}

fn lua_closes_block() -> Regex {
    Regex::new(LUA_CLOSES_BLOCK).expect("LUA_CLOSES_BLOCK is a valid regex")
}

/// The default pairs (brackets and both quote kinds, suppressed inside
/// strings and comments through the grammar registered above) are right
/// for Lua as they are; only the indentation needs the keyword blocks.
fn lua_editing_config() -> LanguageConfig {
    LanguageConfig::default().indentation_rules(IndentationRules::new(lua_opens_block(), lua_closes_block()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opens(line: &str) -> bool {
        lua_opens_block().is_match(line.trim_end())
    }

    fn closes(rest: &str) -> bool {
        lua_closes_block().is_match(rest)
    }

    #[test]
    fn lua_block_openers_indent_the_next_line() {
        for line in [
            "if x then",
            "elseif y then",
            "else",
            "for i = 1, 10 do",
            "while true do",
            "repeat",
            "local function f(a, b)",
            "function M.run()",
            "local f = function()",
            "local t = {",
            "call(",
            "if x then -- why",
            "  if x then  ",
        ] {
            assert!(opens(line), "{line:?} should open a block");
        }
    }

    #[test]
    fn lua_one_liners_and_calls_do_not_indent() {
        for line in [
            "if x then return end",
            "local x = f(y)",
            "pcall(function() return 1 end)",
            "local s = 'then'",
            "return x",
            "",
        ] {
            assert!(!opens(line), "{line:?} should not open a block");
        }
    }

    #[test]
    fn lua_block_closers_outdent_their_line() {
        for rest in ["end", "  end)", "else", "elseif z then", "until done", "}", ")", "]"] {
            assert!(closes(rest), "{rest:?} should close a block");
        }
        for rest in ["ending = 1", "x", "", "  -- end"] {
            assert!(!closes(rest), "{rest:?} should not close a block");
        }
    }
}
