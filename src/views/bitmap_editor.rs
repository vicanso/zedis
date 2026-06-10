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

//! Bitmap / Bitfield viewer for string keys.
//!
//! A Redis bitmap is just a `string` — every `SETBIT` / `BITCOUNT` works
//! on any string and `TYPE` is always `string`, so (unlike a HyperLogLog)
//! there is no way to auto-detect one. This view is therefore reached via
//! an explicit Bitmap toggle on the string editor (mirroring the GEO
//! map's Table/Map toggle), not by classification.
//!
//! It paints the value's bits on a tile-less, GPU-rendered grid — each
//! cell is one bit, set bits lit, in Redis bit order (bit `i` is the
//! `7 - i%8` bit of byte `i/8`, MSB first). Hover shows the bit offset;
//! clicking a cell flips it with `SETBIT`. A stat row surfaces `BITCOUNT`
//! / `BITPOS`, and a thin `BITFIELD` box runs raw sub-commands. The grid
//! is capped at [`CAP_BITS`]; `BITCOUNT` / `BITPOS` stay whole-key.

use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ZedisServerState, i18n_bitmap},
};
use gpui::{
    BorderStyle, Bounds, Context, Entity, EventEmitter, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, SharedString, Subscription, Task, Window, bounds, canvas, div, fill, point, prelude::*, px, quad, rgb, size,
    transparent_black,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex,
};
use redis::cmd;
use std::cell::Cell;
use std::rc::Rc;
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Max bits painted in the grid. The value can be far bigger — this is a
/// debug visualiser, not a renderer for million-bit bitmaps — so beyond
/// this we draw the first window and flag truncation. `BITCOUNT` /
/// `BITPOS` always run on the whole key regardless.
const CAP_BITS: usize = 4096;

/// Strings at least this large are never auto-classified as bitmaps: a big
/// opaque blob is far more likely to be serialized data or media than a
/// hand-built bitset. The manual toggle still covers those.
const BITMAP_AUTO_MAX_BYTES: usize = 4 * 1024;

/// Returned to the parent editor when the user leaves the bitmap view.
pub enum BitmapEvent {
    ExitToRaw,
}

/// Whether bit `offset` is set, in Redis bit order (MSB-first within each
/// byte). Out-of-range offsets read as `0`.
fn bit_at(bytes: &[u8], offset: usize) -> bool {
    let shift = 7 - (offset % 8);
    bytes.get(offset / 8).is_some_and(|b| (b >> shift) & 1 == 1)
}

/// Whether the Bitmap toggle should be offered for this value at all:
/// opaque binary that `infer` cannot identify as any known file type and
/// that doesn't read as text. Text / JSON and recognised formats (images
/// incl. BMP / TIFF / ICO / AVIF, archives, …) render as meaningless bit
/// noise, so they get no toggle.
pub(crate) fn bitmap_eligible(bytes: &[u8]) -> bool {
    !bytes.is_empty() && infer::get(bytes).is_none() && !is_probably_text(bytes)
}

/// Heuristic for *auto-opening* the bitmap view: an eligible blob that is
/// also small (< [`BITMAP_AUTO_MAX_BYTES`]) — i.e. the typical `SETBIT`
/// bitmap. Bigger eligible blobs keep the manual toggle only; the only
/// false positives are small encrypted / custom-serialized blobs, which
/// one click returns to the raw view.
pub(crate) fn looks_like_bitmap(bytes: &[u8]) -> bool {
    bytes.len() < BITMAP_AUTO_MAX_BYTES && bitmap_eligible(bytes)
}

/// Whether `bytes` read as human-readable text. A bitmap is dominated by
/// `0x00` with isolated set bits, so a *sparse* one (only low offsets set)
/// can be entirely valid UTF-8 — a pure UTF-8 check would misread it as
/// text. NUL bytes are the tell: real text never contains them, bitmaps
/// almost always do.
fn is_probably_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// Grid geometry for `n` bits inside a `vw × vh` canvas: a byte-aligned
/// column count, the resulting cell size (fit so the whole capped grid is
/// always visible — no scroll), and the centring offset. Shared by the
/// painter and the hit-tester so a click lands on the bit it looks like.
struct GridLayout {
    cols: usize,
    cell: f32,
    ox: f32,
    oy: f32,
}

fn grid_layout(n: usize, vw: f32, vh: f32) -> GridLayout {
    let n = n.max(1);
    // Roughly square, rounded up to a multiple of 8 so byte boundaries
    // line up on column edges, clamped to a sane range.
    let cols = ((n as f64).sqrt().ceil() as usize).div_ceil(8).max(1) * 8;
    let cols = cols.clamp(8, 128);
    let rows = n.div_ceil(cols);
    let cell = (vw / cols as f32).min(vh / rows as f32).max(2.0);
    let gw = cols as f32 * cell;
    let gh = rows as f32 * cell;
    GridLayout {
        cols,
        cell,
        ox: (vw - gw) / 2.0,
        oy: (vh - gh) / 2.0,
    }
}

/// Bitmap stats + the rendered (capped) byte window.
#[derive(Clone, Default)]
struct BitmapData {
    /// First [`CAP_BITS`] bits' worth of bytes, painted in the grid.
    bytes: Vec<u8>,
    /// `STRLEN key * 8` — full bit length.
    total_bits: u64,
    /// `BITCOUNT key` — whole-key set-bit count.
    set_bits: i64,
    /// `BITPOS key 1` — first set bit, or -1.
    first_set: i64,
    /// `BITPOS key 0` — first clear bit, or -1.
    first_clear: i64,
    /// `total_bits` exceeds the painted window.
    truncated: bool,
}

impl BitmapData {
    fn rendered_bits(&self) -> usize {
        self.bytes.len() * 8
    }
}

pub struct ZedisBitmapEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    readonly: bool,
    data: Option<BitmapData>,
    error: Option<SharedString>,
    loading: bool,
    /// Bit currently under the cursor (grid index), for hover + click.
    hovered: Option<usize>,
    /// Painted canvas bounds, handed from the paint phase to the mouse
    /// handlers so window coordinates can be made canvas-local.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Raw `BITFIELD` sub-command input (e.g. `GET u8 0`).
    bitfield_input: Entity<InputState>,
    /// Last `BITFIELD` result / error shown under the input.
    bitfield_result: Option<SharedString>,
    bitfield_error: Option<SharedString>,
    /// In-flight fetch / mutation; dropped (cancelled) on recreate.
    load_task: Option<Task<()>>,
    write_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BitmapEvent> for ZedisBitmapEditor {}

impl ZedisBitmapEditor {
    pub fn new(
        server_state: Entity<ZedisServerState>,
        readonly: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new bitmap editor view");
        let placeholder = i18n_bitmap(cx, "bitfield_placeholder").to_string();
        let bitfield_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));

        let subscriptions = vec![
            cx.subscribe_in(&bitfield_input, window, |this, _state, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.run_bitfield(cx);
                }
            }),
        ];

        let mut this = Self {
            server_state,
            key,
            readonly,
            data: None,
            error: None,
            loading: false,
            hovered: None,
            viewport: Rc::new(Cell::new(None)),
            bitfield_input,
            bitfield_result: None,
            bitfield_error: None,
            load_task: None,
            write_task: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        if key.is_empty() {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_bitmap(server_id, db, key).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(data) => {
                        this.data = Some(data);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Local cursor position (canvas-relative pixels) for a window event.
    fn local(&self, position: Point<Pixels>) -> Option<Point<f32>> {
        let vp = self.viewport.get()?;
        Some(point(
            (position.x - vp.origin.x).as_f32(),
            (position.y - vp.origin.y).as_f32(),
        ))
    }

    /// Grid bit index under the local cursor, if any.
    fn hit_test(&self, local: Point<f32>) -> Option<usize> {
        let vp = self.viewport.get()?;
        let n = self.data.as_ref()?.rendered_bits();
        let gl = grid_layout(n, vp.size.width.as_f32(), vp.size.height.as_f32());
        let lx = local.x - gl.ox;
        let ly = local.y - gl.oy;
        if lx < 0.0 || ly < 0.0 {
            return None;
        }
        let col = (lx / gl.cell) as usize;
        let row = (ly / gl.cell) as usize;
        if col >= gl.cols {
            return None;
        }
        let k = row * gl.cols + col;
        (k < n).then_some(k)
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(local) = self.local(event.position) else {
            return;
        };
        let hovered = self.hit_test(local);
        if hovered != self.hovered {
            self.hovered = hovered;
            cx.notify();
        }
    }

    /// Click a cell to flip its bit with `SETBIT`. No-op in read-only mode.
    fn on_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        let Some(local) = self.local(event.position) else {
            return;
        };
        let Some(offset) = self.hit_test(local) else {
            return;
        };
        let current = self.data.as_ref().map(|d| bit_at(&d.bytes, offset)).unwrap_or(false);
        let new_bit = i64::from(!current);
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.write_task = Some(cx.spawn(async move |this, cx| {
            let result = setbit(server_id, db, key, offset as i64, new_bit).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.load(cx),
                Err(e) => {
                    this.error = Some(SharedString::from(e.to_string()));
                    cx.notify();
                }
            });
        }));
    }

    /// Run the raw `BITFIELD key <args>` sub-command from the input.
    fn run_bitfield(&mut self, cx: &mut Context<Self>) {
        let raw = self.bitfield_input.read(cx).value().to_string();
        let args: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if args.is_empty() {
            self.bitfield_error = Some(i18n_bitmap(cx, "bitfield_empty"));
            self.bitfield_result = None;
            cx.notify();
            return;
        }
        self.bitfield_error = None;
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        cx.notify();
        self.write_task = Some(cx.spawn(async move |this, cx| {
            let result = bitfield(server_id, db, key, args).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(values) => {
                        let joined = values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
                        this.bitfield_result = Some(SharedString::from(joined));
                        this.bitfield_error = None;
                        // A BITFIELD SET/INCRBY may have mutated the value.
                        this.load(cx);
                    }
                    Err(e) => {
                        this.bitfield_error = Some(SharedString::from(e.to_string()));
                        this.bitfield_result = None;
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn stat(&self, label: SharedString, value: String, muted: Hsla) -> impl IntoElement {
        h_flex()
            .gap_1()
            .items_baseline()
            .child(Label::new(label).text_xs().text_color(muted))
            .child(Label::new(value).text_xs().font_semibold())
    }

    fn render_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg: Hsla = if theme.is_dark() {
            rgb(0x1a1d21).into()
        } else {
            theme.muted
        };
        let accent: Hsla = rgb(0x60a5fa).into();
        let sep = theme.border.opacity(0.4);
        let hover_color = theme.foreground;

        let data = self.data.clone();
        let hovered = self.hovered;
        let viewport = self.viewport.clone();

        canvas(
            move |b, _window, _cx| {
                viewport.set(Some(b));
            },
            move |b, _state, window, _cx| {
                let vw = b.size.width.as_f32();
                let vh = b.size.height.as_f32();
                window.paint_quad(fill(b, bg));

                let Some(data) = data.as_ref() else {
                    return;
                };
                let n = data.rendered_bits();
                if n == 0 {
                    return;
                }
                let gl = grid_layout(n, vw, vh);
                let ox = b.origin.x.as_f32() + gl.ox;
                let oy = b.origin.y.as_f32() + gl.oy;
                let rows = n.div_ceil(gl.cols);
                let gh = rows as f32 * gl.cell;

                // Faint byte-group separators every 8 columns.
                let mut c = 8;
                while c < gl.cols {
                    let x = ox + c as f32 * gl.cell;
                    window.paint_quad(fill(bounds(point(px(x), px(oy)), size(px(1.), px(gh))), sep));
                    c += 8;
                }

                // Set bits.
                for k in 0..n {
                    if !bit_at(&data.bytes, k) {
                        continue;
                    }
                    let col = k % gl.cols;
                    let row = k / gl.cols;
                    let x = ox + col as f32 * gl.cell;
                    let y = oy + row as f32 * gl.cell;
                    window.paint_quad(fill(
                        bounds(
                            point(px(x + 0.5), px(y + 0.5)),
                            size(px(gl.cell - 1.0), px(gl.cell - 1.0)),
                        ),
                        accent,
                    ));
                }

                // Hover outline (shows the bit a click would flip).
                if let Some(h) = hovered.filter(|h| *h < n) {
                    let col = h % gl.cols;
                    let row = h / gl.cols;
                    let x = ox + col as f32 * gl.cell;
                    let y = oy + row as f32 * gl.cell;
                    window.paint_quad(quad(
                        bounds(point(px(x), px(y)), size(px(gl.cell), px(gl.cell))),
                        px(2.),
                        transparent_black(),
                        px(1.5),
                        hover_color,
                        BorderStyle::Solid,
                    ));
                }
            },
        )
        .size_full()
    }

    fn render_tooltip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let h = self.hovered?;
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let n = data.rendered_bits();
        let gl = grid_layout(n, vp.size.width.as_f32(), vp.size.height.as_f32());
        let col = h % gl.cols;
        let row = h / gl.cols;
        let x = gl.ox + col as f32 * gl.cell;
        let y = gl.oy + row as f32 * gl.cell;
        let value = i64::from(bit_at(&data.bytes, h));
        let muted = cx.theme().muted_foreground;
        Some(
            div()
                .absolute()
                .left(px(x + gl.cell + 6.0))
                .top(px(y))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .child(Label::new(format!("bit {h}")).text_xs().font_semibold())
                        .child(Label::new(value.to_string()).text_xs().text_color(muted)),
                ),
        )
    }

    fn render_bitfield_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Label::new("BITFIELD").text_xs().text_color(muted))
                    .child(div().flex_1().child(Input::new(&self.bitfield_input).small()))
                    .child(
                        Button::new("bitfield-run")
                            .small()
                            .primary()
                            .label(i18n_bitmap(cx, "run"))
                            .on_click(cx.listener(|this, _, _window, cx| this.run_bitfield(cx))),
                    ),
            )
            .when_some(self.bitfield_result.clone(), |this, r| {
                this.child(Label::new(r).text_xs().font_semibold())
            })
            .when_some(self.bitfield_error.clone(), |this, e| {
                this.child(Label::new(e).text_xs().text_color(cx.theme().danger))
            })
    }
}

impl Render for ZedisBitmapEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let mut header = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(Label::new(i18n_bitmap(cx, "title")).font_semibold())
            .child(
                Button::new("bitmap-view-raw")
                    .small()
                    .ghost()
                    .icon(IconName::Menu)
                    .tooltip(i18n_bitmap(cx, "raw"))
                    .on_click(cx.listener(|_this, _, _window, cx| cx.emit(BitmapEvent::ExitToRaw))),
            );
        if let Some(data) = self.data.as_ref() {
            header = header
                .child(self.stat(i18n_bitmap(cx, "bits"), data.total_bits.to_string(), muted))
                .child(self.stat(i18n_bitmap(cx, "set_bits"), data.set_bits.to_string(), muted))
                .child(self.stat(i18n_bitmap(cx, "first_set"), data.first_set.to_string(), muted))
                .child(self.stat(i18n_bitmap(cx, "first_clear"), data.first_clear.to_string(), muted));
            if data.truncated {
                header = header.child(
                    Label::new(i18n_bitmap(cx, "cap_notice"))
                        .text_xs()
                        .text_color(cx.theme().yellow),
                );
            }
        }

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if self.loading && self.data.is_none() {
            div()
                .p_4()
                .child(Label::new(i18n_bitmap(cx, "loading")).text_sm().text_color(muted))
                .into_any_element()
        } else if self.data.as_ref().is_some_and(|d| d.rendered_bits() == 0) {
            div()
                .p_4()
                .child(Label::new(i18n_bitmap(cx, "empty")).text_sm().text_color(muted))
                .into_any_element()
        } else {
            let mut grid = div()
                .id("bitmap-canvas")
                .relative()
                .flex_1()
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(self.render_canvas(cx))
                .when_some(self.render_tooltip(cx), |this, tip| this.child(tip))
                .on_mouse_move(cx.listener(|this, event, _window, cx| this.on_move(event, cx)));
            if !self.readonly {
                grid = grid.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event, _window, cx| this.on_down(event, cx)),
                );
            }
            grid.into_any_element()
        };

        v_flex()
            .size_full()
            .gap_3()
            .p_3()
            .child(header)
            .child(self.render_bitfield_bar(cx))
            .child(body)
    }
}

/// Fetch the rendered byte window plus whole-key `BITCOUNT` / `BITPOS`.
/// Only `STRLEN` is fatal; the stats are best-effort.
async fn fetch_bitmap(server_id: String, db: usize, key: String) -> Result<BitmapData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let len: i64 = cmd("STRLEN").arg(&key).query_async(&mut conn).await?;
    let cap_bytes = (CAP_BITS / 8) as i64;
    let bytes: Vec<u8> = if len <= 0 {
        vec![]
    } else {
        let end = len.min(cap_bytes) - 1;
        cmd("GETRANGE")
            .arg(&key)
            .arg(0)
            .arg(end)
            .query_async(&mut conn)
            .await
            .unwrap_or_default()
    };
    let set_bits: i64 = cmd("BITCOUNT").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let first_set: i64 = cmd("BITPOS")
        .arg(&key)
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap_or(-1);
    let first_clear: i64 = cmd("BITPOS")
        .arg(&key)
        .arg(0)
        .query_async(&mut conn)
        .await
        .unwrap_or(-1);

    let total_bits = (len.max(0) as u64) * 8;
    let rendered = (bytes.len() * 8) as u64;
    Ok(BitmapData {
        bytes,
        total_bits,
        set_bits,
        first_set,
        first_clear,
        truncated: total_bits > rendered,
    })
}

/// `SETBIT key offset value` — flip a single bit.
async fn setbit(server_id: String, db: usize, key: String, offset: i64, value: i64) -> Result<()> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    let _: i64 = cmd("SETBIT")
        .arg(&key)
        .arg(offset)
        .arg(value)
        .query_async(&mut conn)
        .await?;
    Ok(())
}

/// `BITFIELD key <args>` — run the user's raw sub-command, returning the
/// integer reply array.
async fn bitfield(server_id: String, db: usize, key: String, args: Vec<String>) -> Result<Vec<i64>> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    let mut command = cmd("BITFIELD");
    command.arg(&key);
    for arg in &args {
        command.arg(arg);
    }
    Ok(command.query_async(&mut conn).await?)
}

#[cfg(test)]
mod tests {
    use super::{bit_at, bitmap_eligible, grid_layout, looks_like_bitmap};

    #[test]
    fn bit_order_is_msb_first() {
        // 0b1000_0000 → only bit offset 0 is set (MSB of byte 0).
        let bytes = [0b1000_0000u8];
        assert!(bit_at(&bytes, 0));
        assert!(!bit_at(&bytes, 1));
        assert!(!bit_at(&bytes, 7));
        // 0b0000_0001 → only bit offset 7 is set (LSB of byte 0).
        let bytes = [0b0000_0001u8];
        assert!(!bit_at(&bytes, 0));
        assert!(bit_at(&bytes, 7));
        // Second byte, MSB → offset 8.
        let bytes = [0x00, 0b1000_0000u8];
        assert!(bit_at(&bytes, 8));
    }

    #[test]
    fn bit_at_out_of_range_is_zero() {
        assert!(!bit_at(&[], 0));
        assert!(!bit_at(&[0xFF], 8));
    }

    #[test]
    fn grid_columns_are_byte_aligned_and_clamped() {
        // Columns are always a multiple of 8 and within [8, 128].
        for n in [1usize, 64, 512, 4096, 100_000] {
            let gl = grid_layout(n, 800.0, 600.0);
            assert_eq!(gl.cols % 8, 0);
            assert!((8..=128).contains(&gl.cols));
        }
    }

    #[test]
    fn looks_like_bitmap_gates_on_size_format_and_text() {
        // Small opaque non-UTF8 blob → bitmap.
        assert!(looks_like_bitmap(&[0xff, 0x00, 0x80, 0x13]));
        // Plain text is valid UTF-8 → not a bitmap.
        assert!(!looks_like_bitmap(b"hello world"));
        // Empty → not a bitmap.
        assert!(!looks_like_bitmap(&[]));
        // A PNG signature is recognised by `infer` → not a bitmap.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        assert!(!looks_like_bitmap(&png));
        // Too large to auto-classify, even though binary.
        let big = vec![0xffu8; super::BITMAP_AUTO_MAX_BYTES];
        assert!(!looks_like_bitmap(&big));
    }

    #[test]
    fn toggle_eligibility_excludes_text_but_keeps_large_binaries() {
        // JSON / plain text never get the Bitmap toggle.
        assert!(!bitmap_eligible(br#"{"name":"zedis","stars":1}"#));
        assert!(!bitmap_eligible(b"plain text value"));
        // A recognised file format (PNG magic) doesn't either.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        assert!(!bitmap_eligible(&png));
        // A big opaque bitmap keeps the manual toggle even though it is
        // too large to auto-open.
        let big = vec![0u8; super::BITMAP_AUTO_MAX_BYTES * 2];
        assert!(bitmap_eligible(&big));
        assert!(!looks_like_bitmap(&big));
    }

    #[test]
    fn looks_like_bitmap_detects_sparse_nul_heavy_bitmap() {
        // A sparse "online users" bitmap: 64 bytes, mostly NUL, a couple of
        // low (< 0x80) set bytes — valid UTF-8, but the NUL bytes mark it as
        // a bitmap rather than text.
        let mut b = vec![0u8; 64];
        b[0] = 0x04; // SETBIT offset 5
        b[5] = 0x20; // SETBIT offset 42
        assert!(std::str::from_utf8(&b).is_ok()); // it really is valid UTF-8
        assert!(looks_like_bitmap(&b));
    }
}
