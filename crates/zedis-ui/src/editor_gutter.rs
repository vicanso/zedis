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

//! A line-number gutter that stays put.
//!
//! gpui-component sizes an `Editor`'s gutter from the document's current
//! line count on every layout — `digits + 1` glyphs of the editor's mono
//! font — and offers no floor. A 20-line value and a 2,000-line value
//! therefore start their text two glyphs apart, so switching between keys
//! (or a value crossing 10 / 100 / 1,000 lines as it grows) shoves the whole
//! text block sideways. [`stable_gutter_padding`] pads the short documents
//! up to [`GUTTER_MIN_DIGITS`] with plain left padding on the editor's
//! wrapper, measured with the same shaping call the gutter uses, so the line
//! numbers' right edge and the text column no longer move.

use gpui::{App, Entity, Pixels, SharedString, font, px};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{EditorState, RopeExt};

/// Line-number digits every gutter reserves room for. Three covers a value
/// of up to 999 lines; past that the gutter grows as it always did.
pub const GUTTER_MIN_DIGITS: u32 = 3;

/// Left padding for a line-numbered `Editor` so its text column sits where a
/// [`GUTTER_MIN_DIGITS`]-digit document's would. Apply it with `.pl(...)`
/// after any other padding call on the editor, so nothing resets it; the
/// multi-line `Input` wrapper has no padding of its own to lose.
///
/// `font_family` is the family the editor renders in (the caller's
/// `.font_family(...)`, or the theme's mono family when it sets none): the
/// gutter is shaped in that font, and the pad has to be measured in it.
pub fn stable_gutter_padding(state: &Entity<EditorState>, font_family: impl Into<SharedString>, cx: &App) -> Pixels {
    let missing = missing_gutter_columns(state.read(cx).text().lines_len());
    if missing == 0 {
        return px(0.);
    }
    // gpui-base's `layout_line_numbers` shapes a run of `+` in the editor's
    // font at the theme's mono size; one glyph advance of that run is the
    // column to pad with. A font without the glyph pads nothing.
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&font(font_family));
    let column = text_system
        .advance(font_id, cx.theme().mono_font_size, '+')
        .map(|advance| advance.width)
        .unwrap_or(px(0.));
    column * missing as f32
}

/// Glyph columns a gutter for `lines` lines is short of the reserved width.
fn missing_gutter_columns(lines: usize) -> usize {
    let digits = lines.max(1).ilog10() + 1;
    GUTTER_MIN_DIGITS.saturating_sub(digits) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_documents_are_padded_up_to_the_reserved_digits() {
        for (lines, missing) in [
            (0, 2),
            (1, 2),
            (9, 2),
            (10, 1),
            (99, 1),
            (100, 0),
            (999, 0),
            (1_000, 0),
            (12_345, 0),
        ] {
            assert_eq!(missing_gutter_columns(lines), missing, "{lines} lines");
        }
    }
}
