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

//! GEO "radar" viewer for sorted sets that hold geospatial data.
//!
//! Redis GEO commands store members in a sorted set keyed by a 52-bit
//! geohash score, so there is no distinct TYPE — this view is reached
//! via a Map/Table toggle on the ZSET editor. It fetches member
//! coordinates with `GEOPOS` (Redis decodes the geohash for us — no
//! local bit-twiddling) and plots them on a tile-less, GPU-rendered
//! dark canvas using a Web Mercator projection: a debugging "radar",
//! not a real map.
//!
//! MVP scope: dark canvas + grid, fit-to-bounds, scroll-zoom + drag-pan,
//! scatter points with hover highlight + tooltip, a cursor lon/lat HUD,
//! and a side list that cross-highlights with the canvas. Invalid /
//! non-geo members are listed separately. Capped at [`GEO_CAP`] points.

use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ZedisServerState, i18n_geo_map},
};
use gpui::{
    BorderStyle, Bounds, Context, Entity, EventEmitter, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Task, Window, bounds,
    canvas, div, fill, point, prelude::*, px, quad, rgb, size, transparent_black,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex,
};
use redis::{Value, cmd};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Max points fetched / rendered. Beyond this we warn and truncate —
/// this is a debug radar, not a big-data viz, and painting tens of
/// thousands of quads every mouse-move would drop frames.
const GEO_CAP: usize = 2000;

/// Node dot diameter in pixels.
const NODE_SIZE: f32 = 8.0;
/// Hit-test radius for hover (pixels).
const HIT_RADIUS: f32 = 10.0;
/// Inner padding so points never touch the canvas edge (pixels).
const FIT_PADDING: f32 = 28.0;

/// One geo-encoded member projected to Web Mercator world space.
#[derive(Clone)]
struct GeoPoint {
    member: SharedString,
    lon: f64,
    lat: f64,
    /// Web Mercator world X in `[0, 1]` (0 = -180°, 1 = +180°).
    wx: f32,
    /// Web Mercator world Y in `[0, 1]` (0 = north/top).
    wy: f32,
}

#[derive(Clone, Default)]
struct GeoData {
    points: Rc<Vec<GeoPoint>>,
    /// Members with no/zero coordinates (nil from `GEOPOS`, or Null Island).
    invalid: Vec<SharedString>,
    /// Total ZSET cardinality (for the "showing N / total" notice).
    total: usize,
    /// (min_x, min_y, max_x, max_y) bounding box of `points` in world space.
    bbox: (f32, f32, f32, f32),
    /// True when 2+ points collapse to (effectively) one location — the
    /// signature of a plain sorted set (`ZADD`, not `GEOADD`) whose scores
    /// `GEOPOS` decodes to the same geohash corner. Surfaces a hint.
    degenerate: bool,
}

/// An active radius search (`GEOSEARCH FROMLONLAT … BYRADIUS`). Members
/// in `matches` are highlighted; the rest are dimmed.
#[derive(Clone)]
struct GeoSearch {
    lon: f64,
    lat: f64,
    radius_m: f64,
    matches: Rc<HashSet<SharedString>>,
}

/// Emitted when the user switches back to the table view from the map.
pub enum GeoMapEvent {
    ExitToTable,
}

pub struct ZedisGeoMap {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    /// Radius-search center/radius inputs.
    lon_input: Entity<InputState>,
    lat_input: Entity<InputState>,
    radius_input: Entity<InputState>,
    /// Active `GEOSEARCH` result, if any.
    search: Option<GeoSearch>,
    search_error: Option<SharedString>,
    search_task: Option<Task<()>>,
    data: Option<GeoData>,
    error: Option<SharedString>,
    loading: bool,
    load_task: Option<Task<()>>,
    /// User zoom multiplier on top of the fit-to-bounds base scale.
    zoom: f32,
    /// User pan offset in screen pixels.
    offset: Point<f32>,
    /// Last cursor position in canvas-local pixels (for crosshair + HUD).
    cursor: Option<Point<f32>>,
    /// Index into `data.points` currently hovered (canvas or side list).
    hovered: Option<usize>,
    /// Up to two point indices picked with Shift+click — the measuring ruler.
    ruler: Vec<usize>,
    /// Drag-pan state.
    dragging: bool,
    drag_last: Point<Pixels>,
    /// Canvas bounds captured during paint, so mouse handlers can convert
    /// window coordinates to canvas-local and hit-test points.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GeoMapEvent> for ZedisGeoMap {}

impl ZedisGeoMap {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        let make_input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let lon_input = make_input("lon", window, cx);
        let lat_input = make_input("lat", window, cx);
        let radius_input = make_input("km", window, cx);

        let mut subscriptions = Vec::new();
        // Enter in any field runs the radius search.
        for input in [&lon_input, &lat_input, &radius_input] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _state, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.run_search(cx);
                }
            }));
        }

        let mut this = Self {
            server_state,
            key,
            lon_input,
            lat_input,
            radius_input,
            search: None,
            search_error: None,
            search_task: None,
            data: None,
            error: None,
            loading: false,
            load_task: None,
            zoom: 1.0,
            offset: point(0.0, 0.0),
            cursor: None,
            hovered: None,
            ruler: Vec::new(),
            dragging: false,
            drag_last: point(px(0.), px(0.)),
            viewport: Rc::new(Cell::new(None)),
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
            let result = fetch_geo(server_id, db, key).await;
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

    /// Reset zoom/pan back to the fit-to-bounds view.
    fn fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        self.offset = point(0.0, 0.0);
        cx.notify();
    }

    /// Run `GEOSEARCH FROMLONLAT <lon> <lat> BYRADIUS <r> km`, highlighting
    /// the matching members. Validates the three numeric inputs first.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        let lon = self.lon_input.read(cx).value().trim().parse::<f64>();
        let lat = self.lat_input.read(cx).value().trim().parse::<f64>();
        let radius_km = self.radius_input.read(cx).value().trim().parse::<f64>();
        let (Ok(lon), Ok(lat), Ok(radius_km)) = (lon, lat, radius_km) else {
            self.search_error = Some(i18n_geo_map(cx, "search_invalid"));
            cx.notify();
            return;
        };
        if radius_km <= 0.0 {
            self.search_error = Some(i18n_geo_map(cx, "search_invalid"));
            cx.notify();
            return;
        }
        self.search_error = None;
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        let radius_m = radius_km * 1000.0;
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = geosearch(server_id, db, key, lon, lat, radius_km).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(members) => {
                        this.search = Some(GeoSearch {
                            lon,
                            lat,
                            radius_m,
                            matches: Rc::new(members.into_iter().map(SharedString::from).collect()),
                        });
                        this.search_error = None;
                    }
                    Err(e) => this.search_error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Clear the active radius search.
    fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        self.search_error = None;
        cx.notify();
    }

    /// Local cursor position (canvas-relative pixels) for a window event.
    fn local(&self, position: Point<Pixels>) -> Option<Point<f32>> {
        let vp = self.viewport.get()?;
        Some(point(
            (position.x - vp.origin.x).as_f32(),
            (position.y - vp.origin.y).as_f32(),
        ))
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => p.y.as_f32(),
            ScrollDelta::Lines(l) => l.y * 20.0,
        };
        if dy == 0.0 {
            return;
        }
        let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
        self.zoom = (self.zoom * factor).clamp(0.2, 50.0);
        cx.notify();
    }

    fn on_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        // Shift+click measures: pick data nodes (A then B); a 3rd pick or a
        // click on empty space resets the ruler.
        if event.modifiers.shift {
            if let Some(local) = self.local(event.position) {
                match self.hit_test(local) {
                    Some(ix) => {
                        if self.ruler.len() >= 2 {
                            self.ruler.clear();
                        }
                        self.ruler.push(ix);
                    }
                    None => self.ruler.clear(),
                }
                cx.notify();
            }
            return;
        }
        // A normal click dismisses any active measurement.
        if !self.ruler.is_empty() {
            self.ruler.clear();
        }
        self.dragging = true;
        self.drag_last = event.position;
        cx.notify();
    }

    fn on_up(&mut self, _event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.dragging {
            self.dragging = false;
            cx.notify();
        }
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.dragging {
            let dx = (event.position.x - self.drag_last.x).as_f32();
            let dy = (event.position.y - self.drag_last.y).as_f32();
            self.offset.x += dx;
            self.offset.y += dy;
            self.drag_last = event.position;
            cx.notify();
            return;
        }
        let Some(local) = self.local(event.position) else {
            return;
        };
        self.cursor = Some(local);
        self.hovered = self.hit_test(local);
        cx.notify();
    }

    /// Find the nearest point within [`HIT_RADIUS`] of the local cursor.
    fn hit_test(&self, local: Point<f32>) -> Option<usize> {
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let (vw, vh) = (vp.size.width.as_f32(), vp.size.height.as_f32());
        let mut best: Option<(usize, f32)> = None;
        for (ix, p) in data.points.iter().enumerate() {
            let (sx, sy) = project(p.wx, p.wy, data.bbox, vw, vh, self.zoom, self.offset);
            let d = (sx - local.x).hypot(sy - local.y);
            if d <= HIT_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((ix, d));
            }
        }
        best.map(|(ix, _)| ix)
    }

    fn render_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let grid = theme.border.opacity(0.5);
        let node: Hsla = rgb(0x4fd1c5).into();
        let node_hi: Hsla = rgb(0x9af5ea).into();
        let glow = Hsla { a: 0.25, ..node_hi };
        let bg: Hsla = if theme.is_dark() {
            rgb(0x1a1d21).into()
        } else {
            theme.muted
        };
        let crosshair = theme.muted_foreground.opacity(0.4);

        let data = self.data.clone();
        let zoom = self.zoom;
        let offset = self.offset;
        let cursor = self.cursor;
        let hovered = self.hovered;
        let search = self.search.clone();
        let ruler = self.ruler.clone();
        let ruler_color = theme.foreground;
        let viewport = self.viewport.clone();

        canvas(
            move |b, _window, _cx| {
                // Hand the painted bounds to the mouse handlers.
                viewport.set(Some(b));
            },
            move |b, _state, window, _cx| {
                let origin = b.origin;
                let vw = b.size.width.as_f32();
                let vh = b.size.height.as_f32();

                // Background.
                window.paint_quad(fill(b, bg));

                // Fixed screen grid (radar feel).
                const GRID_N: usize = 8;
                for i in 1..GRID_N {
                    let gx = vw * (i as f32) / (GRID_N as f32);
                    window.paint_quad(fill(
                        bounds(point(origin.x + px(gx), origin.y), size(px(1.), px(vh))),
                        grid,
                    ));
                    let gy = vh * (i as f32) / (GRID_N as f32);
                    window.paint_quad(fill(
                        bounds(point(origin.x, origin.y + px(gy)), size(px(vw), px(1.))),
                        grid,
                    ));
                }

                // Crosshair following the cursor.
                if let Some(c) = cursor {
                    window.paint_quad(fill(
                        bounds(point(origin.x + px(c.x), origin.y), size(px(1.), px(vh))),
                        crosshair,
                    ));
                    window.paint_quad(fill(
                        bounds(point(origin.x, origin.y + px(c.y)), size(px(vw), px(1.))),
                        crosshair,
                    ));
                }

                let Some(data) = data.as_ref() else {
                    return;
                };

                // Radius search circle (approximate — matching is authoritative
                // via GEOSEARCH; this is a visual guide).
                if let Some(s) = search.as_ref() {
                    let cwx = lon_to_world(s.lon);
                    let cwy = lat_to_world(s.lat);
                    let (csx, csy) = project(cwx, cwy, data.bbox, vw, vh, zoom, offset);
                    let lat_rad = s.lat.to_radians();
                    let dlon = s.radius_m / (111_320.0 * lat_rad.cos().abs().max(1e-6));
                    let (esx, _) = project(lon_to_world(s.lon + dlon), cwy, data.bbox, vw, vh, zoom, offset);
                    let rad = (esx - csx).abs();
                    if rad > 0.5 {
                        window.paint_quad(quad(
                            bounds(
                                point(origin.x + px(csx - rad), origin.y + px(csy - rad)),
                                size(px(rad * 2.0), px(rad * 2.0)),
                            ),
                            px(rad),
                            Hsla { a: 0.08, ..node_hi },
                            px(1.5),
                            node_hi,
                            BorderStyle::default(),
                        ));
                    }
                    // Center marker.
                    window.paint_quad(quad(
                        bounds(
                            point(origin.x + px(csx - 3.), origin.y + px(csy - 3.)),
                            size(px(6.), px(6.)),
                        ),
                        px(3.),
                        node_hi,
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                }

                // Measuring ruler: dotted line between the two picked nodes.
                if ruler.len() == 2
                    && let (Some(a), Some(b)) = (data.points.get(ruler[0]), data.points.get(ruler[1]))
                {
                    let (ax, ay) = project(a.wx, a.wy, data.bbox, vw, vh, zoom, offset);
                    let (bx, by) = project(b.wx, b.wy, data.bbox, vw, vh, zoom, offset);
                    let total = (bx - ax).hypot(by - ay);
                    if total > 1.0 {
                        let steps = (total / 8.0).floor() as i32;
                        for i in 0..=steps {
                            let t = ((i as f32) * 8.0 / total).min(1.0);
                            let dx = ax + (bx - ax) * t;
                            let dy = ay + (by - ay) * t;
                            window.paint_quad(quad(
                                bounds(
                                    point(origin.x + px(dx - 1.5), origin.y + px(dy - 1.5)),
                                    size(px(3.), px(3.)),
                                ),
                                px(1.5),
                                ruler_color,
                                px(0.),
                                transparent_black(),
                                BorderStyle::default(),
                            ));
                        }
                    }
                }

                let dim = Hsla { a: 0.22, ..node };

                // Data nodes.
                for (ix, p) in data.points.iter().enumerate() {
                    let (sx, sy) = project(p.wx, p.wy, data.bbox, vw, vh, zoom, offset);
                    if sx < -NODE_SIZE || sy < -NODE_SIZE || sx > vw + NODE_SIZE || sy > vh + NODE_SIZE {
                        continue;
                    }
                    let is_hi = hovered == Some(ix) || ruler.contains(&ix);
                    let matched = search.as_ref().map(|s| s.matches.contains(&p.member));
                    let r = if is_hi { NODE_SIZE * 0.9 } else { NODE_SIZE / 2.0 };
                    if is_hi {
                        // Glow halo behind the highlighted node.
                        let gr = r * 2.2;
                        window.paint_quad(quad(
                            bounds(
                                point(origin.x + px(sx - gr), origin.y + px(sy - gr)),
                                size(px(gr * 2.0), px(gr * 2.0)),
                            ),
                            px(gr),
                            glow,
                            px(0.),
                            transparent_black(),
                            BorderStyle::default(),
                        ));
                    }
                    let color = if is_hi {
                        node_hi
                    } else {
                        match matched {
                            // In a radius search: matches stay bright, others fade.
                            Some(true) => node,
                            Some(false) => dim,
                            None => node,
                        }
                    };
                    window.paint_quad(quad(
                        bounds(
                            point(origin.x + px(sx - r), origin.y + px(sy - r)),
                            size(px(r * 2.0), px(r * 2.0)),
                        ),
                        px(r),
                        color,
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            },
        )
        .size_full()
    }

    fn render_hud(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let cursor = self.cursor?;
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let (lon, lat) = unproject(
            cursor,
            data.bbox,
            vp.size.width.as_f32(),
            vp.size.height.as_f32(),
            self.zoom,
            self.offset,
        );
        let muted = cx.theme().muted_foreground;
        Some(
            div()
                .absolute()
                .bottom_1()
                .right_2()
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .bg(cx.theme().background.opacity(0.7))
                .child(Label::new(format!("{lon:.5}, {lat:.5}")).text_xs().text_color(muted)),
        )
    }

    /// Scale bar (bottom-left): a labelled line whose length represents a
    /// round distance at the current zoom and the view's center latitude.
    fn render_scale(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let (bar_px, label) = nice_scale(
            vp.size.width.as_f32(),
            vp.size.height.as_f32(),
            data.bbox,
            self.zoom,
            self.offset,
        )?;
        let fg = cx.theme().foreground;
        Some(
            div()
                .absolute()
                .bottom_1()
                .left_2()
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .bg(cx.theme().background.opacity(0.7))
                .child(
                    v_flex()
                        .gap_0p5()
                        .items_center()
                        .child(Label::new(label).text_xs().text_color(fg))
                        .child(div().h(px(2.)).w(px(bar_px)).bg(fg)),
                ),
        )
    }

    /// Measuring-ruler distance pill, centered on the line between the two
    /// Shift-picked points. Shows the great-circle (Haversine) distance.
    fn render_ruler(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.ruler.len() != 2 {
            return None;
        }
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let a = data.points.get(self.ruler[0])?;
        let b = data.points.get(self.ruler[1])?;
        let vw = vp.size.width.as_f32();
        let vh = vp.size.height.as_f32();
        let (ax, ay) = project(a.wx, a.wy, data.bbox, vw, vh, self.zoom, self.offset);
        let (bx, by) = project(b.wx, b.wy, data.bbox, vw, vh, self.zoom, self.offset);
        let meters = haversine(a.lon, a.lat, b.lon, b.lat);
        let label = if meters >= 1000.0 {
            format!("{:.2} km", meters / 1000.0)
        } else {
            format!("{:.0} m", meters)
        };
        Some(
            div()
                .absolute()
                .left(px((ax + bx) / 2.0 - 28.0))
                .top(px((ay + by) / 2.0 - 10.0))
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .child(
                    Label::new(label)
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().foreground),
                ),
        )
    }

    /// Discoverability hint (top-center): how to measure. Hidden once a
    /// measurement is active or there aren't enough points.
    fn render_measure_hint(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.ruler.is_empty() || self.data.as_ref().is_none_or(|d| d.points.len() < 2) {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        Some(
            div()
                .absolute()
                .top_1()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(cx.theme().background.opacity(0.7))
                        .child(Label::new(i18n_geo_map(cx, "measure_hint")).text_xs().text_color(muted)),
                ),
        )
    }

    fn render_tooltip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let ix = self.hovered?;
        let vp = self.viewport.get()?;
        let data = self.data.as_ref()?;
        let p = data.points.get(ix)?;
        let (sx, sy) = project(
            p.wx,
            p.wy,
            data.bbox,
            vp.size.width.as_f32(),
            vp.size.height.as_f32(),
            self.zoom,
            self.offset,
        );
        let muted = cx.theme().muted_foreground;
        Some(
            div()
                .absolute()
                .left(px(sx + 12.0))
                .top(px(sy + 12.0))
                .max_w(px(260.))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(Label::new(p.member.clone()).text_xs().font_semibold().truncate())
                        .child(
                            Label::new(format!("{:.5}, {:.5}", p.lon, p.lat))
                                .text_xs()
                                .text_color(muted),
                        ),
                ),
        )
    }

    /// Radius-search bar: center lon/lat + radius (km) → `GEOSEARCH`.
    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut bar = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(Input::new(&self.lon_input).small().w(px(90.)))
            .child(Input::new(&self.lat_input).small().w(px(90.)))
            .child(Input::new(&self.radius_input).small().w(px(80.)))
            .child(
                Button::new("geo-radius-search")
                    .small()
                    .primary()
                    .label(i18n_geo_map(cx, "search"))
                    .on_click(cx.listener(|this, _, _window, cx| this.run_search(cx))),
            );
        if self.search.is_some() {
            bar = bar.child(
                Button::new("geo-radius-clear")
                    .small()
                    .ghost()
                    .label(i18n_geo_map(cx, "clear"))
                    .on_click(cx.listener(|this, _, _window, cx| this.clear_search(cx))),
            );
        }
        if let Some(err) = self.search_error.clone() {
            bar = bar.child(Label::new(err).text_xs().text_color(cx.theme().danger));
        } else if let Some(s) = self.search.as_ref() {
            bar = bar.child(
                Label::new(format!("{} {}", s.matches.len(), i18n_geo_map(cx, "matches")))
                    .text_xs()
                    .text_color(muted),
            );
        }
        bar
    }

    fn render_side_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut col = v_flex().w(px(220.)).h_full().flex_none().gap_1().child(
            Label::new(i18n_geo_map(cx, "points"))
                .text_xs()
                .font_semibold()
                .text_color(muted),
        );

        let mut list = v_flex().w_full().gap_0p5();
        if let Some(data) = self.data.as_ref() {
            for (ix, p) in data.points.iter().enumerate() {
                let active = self.hovered == Some(ix);
                list = list.child(
                    h_flex()
                        .id(("geo-row", ix))
                        .w_full()
                        .gap_2()
                        .items_baseline()
                        .px_1()
                        .rounded_md()
                        .when(active, |s| s.bg(cx.theme().list_active))
                        .hover(|s| s.bg(cx.theme().list_active))
                        .cursor_pointer()
                        .on_mouse_move(cx.listener(move |this, _, _window, cx| {
                            if this.hovered != Some(ix) {
                                this.hovered = Some(ix);
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Label::new(p.member.clone()).text_xs().truncate()),
                        )
                        .child(
                            Label::new(format!("{:.3}, {:.3}", p.lon, p.lat))
                                .text_xs()
                                .text_color(muted),
                        ),
                );
            }

            if !data.invalid.is_empty() {
                list = list.child(
                    Label::new(i18n_geo_map(cx, "invalid"))
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().yellow)
                        .pt_2(),
                );
                for (ix, member) in data.invalid.iter().enumerate() {
                    list = list.child(
                        h_flex()
                            .id(("geo-invalid", ix))
                            .w_full()
                            .px_1()
                            .child(Label::new(member.clone()).text_xs().text_color(muted).truncate()),
                    );
                }
            }
        }

        col = col.child(
            div()
                .id("geo-side-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        );
        col
    }
}

impl Render for ZedisGeoMap {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let mut header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Label::new(i18n_geo_map(cx, "title")).font_semibold())
            // Icon-only "back to table" button (matches the footer toggle).
            .child(
                Button::new("geo-view-table")
                    .small()
                    .ghost()
                    .icon(IconName::Menu)
                    .tooltip(i18n_geo_map(cx, "table"))
                    .on_click(cx.listener(|_this, _, _window, cx| cx.emit(GeoMapEvent::ExitToTable))),
            );
        if let Some(data) = self.data.as_ref() {
            header = header.child(
                Label::new(format!("{} / {}", data.points.len(), data.total))
                    .text_xs()
                    .text_color(muted),
            );
            if data.total > GEO_CAP {
                header = header.child(
                    Label::new(i18n_geo_map(cx, "cap_notice"))
                        .text_xs()
                        .text_color(cx.theme().yellow),
                );
            }
            if data.degenerate {
                header = header.child(
                    Label::new(i18n_geo_map(cx, "not_geo"))
                        .text_xs()
                        .text_color(cx.theme().yellow),
                );
            }
        }
        header = header.child(div().flex_1()).child(
            Button::new("geo-fit")
                .small()
                .ghost()
                .label(i18n_geo_map(cx, "fit"))
                .on_click(cx.listener(|this, _, _window, cx| this.fit(cx))),
        );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if self.loading && self.data.is_none() {
            div()
                .p_4()
                .child(Label::new(i18n_geo_map(cx, "loading")).text_sm().text_color(muted))
                .into_any_element()
        } else if self.data.as_ref().is_some_and(|d| d.points.is_empty()) {
            div()
                .p_4()
                .child(Label::new(i18n_geo_map(cx, "empty")).text_sm().text_color(muted))
                .into_any_element()
        } else {
            let map = div()
                .id("geo-map-canvas")
                .relative()
                .flex_1()
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .cursor_crosshair()
                .child(self.render_canvas(cx))
                .when_some(self.render_hud(cx), |this, hud| this.child(hud))
                .when_some(self.render_scale(cx), |this, s| this.child(s))
                .when_some(self.render_ruler(cx), |this, r| this.child(r))
                .when_some(self.render_measure_hint(cx), |this, h| this.child(h))
                .when_some(self.render_tooltip(cx), |this, tip| this.child(tip))
                .on_scroll_wheel(cx.listener(|this, event, _window, cx| this.on_scroll(event, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event, _window, cx| this.on_down(event, cx)),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, event, _window, cx| this.on_up(event, cx)),
                )
                .on_mouse_move(cx.listener(|this, event, _window, cx| this.on_move(event, cx)));

            h_flex()
                .w_full()
                .flex_1()
                .gap_3()
                .min_h_0()
                .child(self.render_side_list(cx))
                .child(map)
                .into_any_element()
        };

        let has_points = self.data.as_ref().is_some_and(|d| !d.points.is_empty());
        v_flex()
            .size_full()
            .gap_3()
            .p_3()
            .child(header)
            .when(has_points, |this| this.child(self.render_search_bar(cx)))
            .child(body)
    }
}

/// Project a Web Mercator world point to canvas-local pixels, fitting the
/// data `bbox` (preserving aspect ratio) then applying user zoom/pan.
fn project(
    wx: f32,
    wy: f32,
    bbox: (f32, f32, f32, f32),
    vw: f32,
    vh: f32,
    zoom: f32,
    offset: Point<f32>,
) -> (f32, f32) {
    let (minx, miny, maxx, maxy) = bbox;
    let dw = (maxx - minx).max(1e-9);
    let dh = (maxy - miny).max(1e-9);
    let avail_w = (vw - 2.0 * FIT_PADDING).max(1.0);
    let avail_h = (vh - 2.0 * FIT_PADDING).max(1.0);
    let k = (avail_w / dw).min(avail_h / dh) * zoom;
    let cx = (minx + maxx) / 2.0;
    let cy = (miny + maxy) / 2.0;
    let sx = vw / 2.0 + (wx - cx) * k + offset.x;
    let sy = vh / 2.0 + (wy - cy) * k + offset.y;
    (sx, sy)
}

/// Inverse of [`project`]: canvas-local pixels back to `(lon, lat)`.
fn unproject(
    local: Point<f32>,
    bbox: (f32, f32, f32, f32),
    vw: f32,
    vh: f32,
    zoom: f32,
    offset: Point<f32>,
) -> (f64, f64) {
    let (minx, miny, maxx, maxy) = bbox;
    let dw = (maxx - minx).max(1e-9);
    let dh = (maxy - miny).max(1e-9);
    let avail_w = (vw - 2.0 * FIT_PADDING).max(1.0);
    let avail_h = (vh - 2.0 * FIT_PADDING).max(1.0);
    let k = (avail_w / dw).min(avail_h / dh) * zoom;
    let cx = (minx + maxx) / 2.0;
    let cy = (miny + maxy) / 2.0;
    let wx = (local.x - vw / 2.0 - offset.x) / k + cx;
    let wy = (local.y - vh / 2.0 - offset.y) / k + cy;
    (world_to_lon(wx as f64), world_to_lat(wy as f64))
}

/// Pick a "nice" round distance (1 / 2 / 5 × 10ⁿ) at or below `m`.
fn nice_distance(m: f64) -> f64 {
    if m <= 0.0 || !m.is_finite() {
        return 1.0;
    }
    let pow = 10f64.powf(m.log10().floor());
    let n = m / pow;
    let nice = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * pow
}

/// Compute a scale bar: `(bar_width_px, label)` for a round distance near
/// 100px, measured along the view's center latitude (Web Mercator scale
/// varies with latitude, so this is the standard "good enough" approach).
fn nice_scale(vw: f32, vh: f32, bbox: (f32, f32, f32, f32), zoom: f32, offset: Point<f32>) -> Option<(f32, String)> {
    if vw < 60.0 || vh < 20.0 {
        return None;
    }
    const REF_PX: f32 = 100.0;
    let cy = vh / 2.0;
    let (lon_c, lat_c) = unproject(point(vw / 2.0, cy), bbox, vw, vh, zoom, offset);
    let (lon_r, _) = unproject(point(vw / 2.0 + REF_PX, cy), bbox, vw, vh, zoom, offset);
    // Equirectangular metres for REF_PX at this latitude (both points share it).
    let meters_ref = (lon_r - lon_c).abs() * 111_320.0 * lat_c.to_radians().cos().abs();
    if !meters_ref.is_finite() || meters_ref <= 0.0 {
        return None;
    }
    let meters_per_px = meters_ref / REF_PX as f64;
    // Beyond ~half Earth's circumference a scale bar is meaningless (the view
    // is zoomed out past the globe). Hide it rather than show an impossible
    // distance like "40000 km".
    let target_m = meters_per_px * 100.0;
    if target_m > 20_000_000.0 {
        return None;
    }
    let nice_m = nice_distance(target_m);
    let bar_px = (nice_m / meters_per_px) as f32;
    let label = if nice_m >= 1000.0 {
        format!("{:.0} km", nice_m / 1000.0)
    } else {
        format!("{:.0} m", nice_m)
    };
    Some((bar_px, label))
}

/// Great-circle (Haversine) distance between two lon/lat points, in metres.
fn haversine(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let p1 = lat1.to_radians();
    let p2 = lat2.to_radians();
    let dphi = (lat2 - lat1).to_radians();
    let dlam = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlam / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().clamp(0.0, 1.0).asin()
}

/// Web Mercator longitude → world `[0, 1]`.
fn lon_to_world(lon: f64) -> f32 {
    (((lon + 180.0) / 360.0) as f32).clamp(0.0, 1.0)
}

/// Web Mercator latitude → world `[0, 1]` (0 = north).
fn lat_to_world(lat: f64) -> f32 {
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78);
    let rad = lat.to_radians();
    let y = (1.0 - (std::f64::consts::FRAC_PI_4 + rad / 2.0).tan().ln() / std::f64::consts::PI) / 2.0;
    y as f32
}

fn world_to_lon(wx: f64) -> f64 {
    wx * 360.0 - 180.0
}

fn world_to_lat(wy: f64) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * wy);
    n.sinh().atan().to_degrees()
}

/// `ZCARD` + `ZRANGE 0..cap` + `GEOPOS` → projected points + invalid members.
async fn fetch_geo(server_id: String, db: usize, key: String) -> Result<GeoData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let total: i64 = cmd("ZCARD").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let members: Vec<String> = cmd("ZRANGE")
        .arg(&key)
        .arg(0)
        .arg(GEO_CAP as i64 - 1)
        .query_async(&mut conn)
        .await?;

    let mut geopos = cmd("GEOPOS");
    geopos.arg(&key);
    for m in &members {
        geopos.arg(m);
    }
    let raw: Value = geopos.query_async(&mut conn).await?;
    let coords = match raw {
        Value::Array(items) => items,
        _ => Vec::new(),
    };

    let mut points = Vec::new();
    let mut invalid = Vec::new();
    for (member, coord) in members.into_iter().zip(coords) {
        match parse_lon_lat(&coord) {
            // Treat exact (0,0) "Null Island" as suspicious, not a real point.
            Some((lon, lat)) if lon != 0.0 || lat != 0.0 => points.push(GeoPoint {
                wx: lon_to_world(lon),
                wy: lat_to_world(lat),
                lon,
                lat,
                member: SharedString::from(member),
            }),
            _ => invalid.push(SharedString::from(member)),
        }
    }

    let bbox = if points.is_empty() {
        (0.0, 0.0, 1.0, 1.0)
    } else {
        points.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(minx, miny, maxx, maxy), p| (minx.min(p.wx), miny.min(p.wy), maxx.max(p.wx), maxy.max(p.wy)),
        )
    };

    // 2+ points that span essentially nothing means every score decoded
    // to the same geohash cell — almost always a plain ZADD set, not GEO.
    let degenerate = points.len() >= 2 && (bbox.2 - bbox.0) < 1e-6 && (bbox.3 - bbox.1) < 1e-6;

    Ok(GeoData {
        points: Rc::new(points),
        invalid,
        total: total.max(0) as usize,
        bbox,
        degenerate,
    })
}

/// `GEOSEARCH key FROMLONLAT <lon> <lat> BYRADIUS <r> km ASC COUNT <cap>`
/// → matching member names.
async fn geosearch(
    server_id: String,
    db: usize,
    key: String,
    lon: f64,
    lat: f64,
    radius_km: f64,
) -> Result<Vec<String>> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    let members: Vec<String> = cmd("GEOSEARCH")
        .arg(&key)
        .arg("FROMLONLAT")
        .arg(lon)
        .arg(lat)
        .arg("BYRADIUS")
        .arg(radius_km)
        .arg("km")
        .arg("ASC")
        .arg("COUNT")
        .arg(GEO_CAP as i64)
        .query_async(&mut conn)
        .await?;
    Ok(members)
}

/// Cheap heuristic: does this sorted set hold GEO data? Probes the first
/// couple of members with `GEOPOS` and checks the decoded coordinates.
///
/// `GEOPOS` decodes *any* sorted-set score as a geohash, so a plain
/// `ZADD` set (e.g. all score 0) collapses to the south-west corner
/// `(-180, -85.05)`. We treat the set as GEO only when at least one
/// probed member decodes to a real coordinate away from that corner —
/// enough to decide whether to offer the Map view.
pub(crate) async fn zset_looks_geo(server_id: String, db: usize, key: String) -> bool {
    let Ok(mut conn) = get_connection_manager().get_connection(&server_id, db).await else {
        return false;
    };
    let members: Vec<String> = cmd("ZRANGE")
        .arg(&key)
        .arg(0)
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    if members.is_empty() {
        return false;
    }
    let mut geopos = cmd("GEOPOS");
    geopos.arg(&key);
    for m in &members {
        geopos.arg(m);
    }
    let Ok(Value::Array(coords)) = geopos.query_async::<Value>(&mut conn).await else {
        return false;
    };
    // GEO if any probed member sits away from the (-180, -85.05) corner.
    coords
        .iter()
        .filter_map(parse_lon_lat)
        .any(|(lon, lat)| lon > -179.99 || lat > -85.0)
}

/// Parse a single `GEOPOS` element: `[lon, lat]` bulk strings, or nil.
fn parse_lon_lat(value: &Value) -> Option<(f64, f64)> {
    let Value::Array(pair) = value else {
        return None;
    };
    let lon = value_to_f64(pair.first()?)?;
    let lat = value_to_f64(pair.get(1)?)?;
    Some((lon, lat))
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Double(d) => Some(*d),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).trim().parse().ok(),
        Value::SimpleString(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_one_degree_latitude() {
        // 1° of latitude ≈ 111.2 km on a sphere of R = 6371 km.
        let d = haversine(0.0, 0.0, 0.0, 1.0);
        assert!((d - 111_195.0).abs() < 500.0, "got {d}");
        // Same key point → zero distance.
        assert_eq!(haversine(116.4, 39.9, 116.4, 39.9), 0.0);
    }

    #[test]
    fn nice_distance_rounds_to_1_2_5_10() {
        assert_eq!(nice_distance(1.0), 1.0);
        assert_eq!(nice_distance(3.0), 2.0);
        assert_eq!(nice_distance(6.0), 5.0);
        assert_eq!(nice_distance(9.0), 10.0);
        assert_eq!(nice_distance(1234.0), 1000.0);
        assert_eq!(nice_distance(0.0), 1.0);
    }

    #[test]
    fn mercator_roundtrip_is_stable() {
        for (lon, lat) in [(116.397, 39.908), (-122.42, 37.77), (0.1, 0.1), (-180.0, -85.0)] {
            let wx = lon_to_world(lon) as f64;
            let wy = lat_to_world(lat) as f64;
            assert!((world_to_lon(wx) - lon).abs() < 0.01, "lon {lon}");
            assert!((world_to_lat(wy) - lat).abs() < 0.05, "lat {lat}");
        }
    }

    #[test]
    fn parse_lon_lat_handles_pair_and_nil() {
        let pair = Value::Array(vec![
            Value::BulkString(b"116.39".to_vec()),
            Value::BulkString(b"39.90".to_vec()),
        ]);
        assert_eq!(parse_lon_lat(&pair), Some((116.39, 39.90)));
        assert_eq!(parse_lon_lat(&Value::Nil), None);
    }
}
