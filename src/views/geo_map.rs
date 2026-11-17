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

use crate::assets::CustomIconName;
use crate::helpers::{canvas_surface_background, geo_node_colors, get_mono_font_family};
use crate::{
    connection::{
        Capability, GeoMember, GeoSample, GeoShape, ServerCommand, ServerDb, geo_add, geo_dist, geo_sample, geo_search,
    },
    states::{ZedisServerState, dialog_button_props, i18n_common, i18n_geo_map},
};
use gpui::{
    BorderStyle, Bounds, Context, Entity, EventEmitter, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollDelta, ScrollEvent, ScrollHandle, SharedString, Subscription, Task, Window,
    bounds, canvas, div, fill, point, prelude::*, px, quad, size, transparent_black, uniform_list,
};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use zedis_ui::ZedisDialog;

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

mod render;
use render::{
    GeoAddForm, GeoDistanceForm, haversine, lat_to_world, lon_to_world, nice_scale, project, project_geo, unproject,
};

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

/// An active search (`GEOSEARCH FROMLONLAT … BYRADIUS` / `… BYBOX`).
/// Members in `matches` are highlighted; the rest are dimmed.
#[derive(Clone)]
struct GeoSearch {
    lon: f64,
    lat: f64,
    shape: GeoShape,
    matches: Rc<HashSet<SharedString>>,
}

/// Emitted when the user switches back to the table view from the map.
pub enum GeoMapEvent {
    ExitToTable,
}

pub struct ZedisGeoMap {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    /// Search centre, and the first extent — a radius, or the box width.
    lon_input: Entity<InputState>,
    lat_input: Entity<InputState>,
    /// Box height; only read (and only shown) in box mode.
    height_input: Entity<InputState>,
    /// `BYBOX` rather than `BYRADIUS`.
    box_search: bool,
    /// Last `GEODIST` answer, shown next to the search result.
    distance: Option<SharedString>,
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
    /// Drives the invalid-members list's native scroll area and its
    /// visible scrollbar.
    invalid_scroll: ScrollHandle,
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
        let height_input = make_input("km", window, cx);

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
            height_input,
            box_search: false,
            distance: None,
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
            invalid_scroll: ScrollHandle::default(),
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
            let result = geo_sample(&ServerDb::new(server_id, db), &key, GEO_CAP)
                .await
                .map(project_geo);
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(data) => {
                        this.data = Some(data);
                        this.error = None;
                        // The ruler holds indices into the points it was
                        // drawn on; a reload's sample orders (or holds)
                        // other ones, and the line joined two strangers.
                        this.ruler.clear();
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
        let width_km = self.radius_input.read(cx).value().trim().parse::<f64>();
        let (Ok(lon), Ok(lat), Ok(width_km)) = (lon, lat, width_km) else {
            self.search_error = Some(i18n_geo_map(cx, "search_invalid"));
            cx.notify();
            return;
        };
        // In box mode the second field is the height; blank means "square",
        // which is the shape people draw when they think in tiles.
        let height_km = if self.box_search {
            let raw = self.height_input.read(cx).value().trim().to_string();
            if raw.is_empty() {
                width_km
            } else {
                match raw.parse::<f64>() {
                    Ok(value) => value,
                    Err(_) => {
                        self.search_error = Some(i18n_geo_map(cx, "search_invalid"));
                        cx.notify();
                        return;
                    }
                }
            }
        } else {
            width_km
        };
        if width_km <= 0.0 || height_km <= 0.0 {
            self.search_error = Some(i18n_geo_map(cx, "search_invalid"));
            cx.notify();
            return;
        }
        self.search_error = None;
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        let shape = if self.box_search {
            GeoShape::Box {
                width_m: width_km * 1000.0,
                height_m: height_km * 1000.0,
            }
        } else {
            GeoShape::Radius(width_km * 1000.0)
        };
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = geo_search(&ServerDb::new(server_id, db), &key, lon, lat, shape, GEO_CAP).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(members) => {
                        this.search = Some(GeoSearch {
                            lon,
                            lat,
                            shape,
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

    fn on_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
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
        let (node, node_hi) = geo_node_colors(theme.is_dark());
        let glow = Hsla { a: 0.25, ..node_hi };
        let bg = canvas_surface_background(cx);
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
                    let (half_x_m, half_y_m) = s.shape.half_extents_m();
                    let dlon = half_x_m / (111_320.0 * lat_rad.cos().abs().max(1e-6));
                    let dlat = half_y_m / 110_540.0;
                    let (esx, _) = project(lon_to_world(s.lon + dlon), cwy, data.bbox, vw, vh, zoom, offset);
                    let (_, north_sy) = project(cwx, lat_to_world(s.lat + dlat), data.bbox, vw, vh, zoom, offset);
                    let rx = (esx - csx).abs();
                    let ry = (north_sy - csy).abs();
                    // A circle is a fully-rounded quad; a box is the same
                    // quad with square corners and its own two extents.
                    let corner = match s.shape {
                        GeoShape::Radius(_) => rx.min(ry),
                        GeoShape::Box { .. } => 0.0,
                    };
                    if rx > 0.5 && ry > 0.5 {
                        window.paint_quad(quad(
                            bounds(
                                point(origin.x + px(csx - rx), origin.y + px(csy - ry)),
                                size(px(rx * 2.0), px(ry * 2.0)),
                            ),
                            px(corner),
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

    /// `GEOADD`: longitude, latitude, member name.
    ///
    /// Longitude first, which is the opposite of how coordinates are
    /// usually spoken — both fields are labelled for that reason, and the
    /// dialog is the only way to put a point in a geo key at all: the
    /// sorted-set editor would need a geohash score nobody computes by hand.
    fn add_point_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lon_state = cx.new(|cx| InputState::new(window, cx).placeholder("0.0"));
        let lat_state = cx.new(|cx| InputState::new(window, cx).placeholder("0.0"));
        let member_state = cx.new(|cx| InputState::new(window, cx));
        let lon_label = i18n_geo_map(cx, "add_lon");
        let lat_label = i18n_geo_map(cx, "add_lat");
        let member_label = i18n_geo_map(cx, "add_member");
        let hint = i18n_geo_map(cx, "add_hint");
        let body_view = cx.new(|_| GeoAddForm {
            lon: lon_state.clone(),
            lat: lat_state.clone(),
            member: member_state.clone(),
            lon_label,
            lat_label,
            member_label,
            hint,
        });
        let focus_lon = lon_state.clone();
        let submit = (lon_state, lat_state, member_state);
        let map = cx.entity().downgrade();

        ZedisDialog::new(i18n_geo_map(cx, "add_title"))
            .w(px(420.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body_view.clone())
            .on_ok(move |_, _window, cx| {
                let member = submit.2.read(cx).value().trim().to_string();
                let (Ok(lon), Ok(lat)) = (
                    submit.0.read(cx).value().trim().parse::<f64>(),
                    submit.1.read(cx).value().trim().parse::<f64>(),
                ) else {
                    return false;
                };
                // The ranges Redis itself enforces; catching them here means
                // an error about the form rather than about the command.
                if member.is_empty() || !(-180.0..=180.0).contains(&lon) || !(-85.05112878..=85.05112878).contains(&lat)
                {
                    return false;
                }
                if let Some(map) = map.upgrade() {
                    map.update(cx, |this, inner| this.run_geo_add(lon, lat, member, inner));
                }
                true
            })
            .open(window, cx);
        focus_lon.update(cx, |state, cx| state.focus(window, cx));
    }

    /// `GEOADD`, then a reload so the new point is on the map.
    fn run_geo_add(&mut self, lon: f64, lat: f64, member: String, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = async { geo_add(&ServerDb::new(server_id, db), &key, lon, lat, &member).await }.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    this.search_error = None;
                    this.load(cx);
                }
                Err(e) => {
                    this.search_error = Some(SharedString::from(e.to_string()));
                    cx.notify();
                }
            });
        }));
    }

    /// `GEODIST`: two member names → metres, reported in the search bar.
    fn distance_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from_state = cx.new(|cx| InputState::new(window, cx));
        let to_state = cx.new(|cx| InputState::new(window, cx));
        let from_label = i18n_geo_map(cx, "dist_from");
        let to_label = i18n_geo_map(cx, "dist_to");
        let body_view = cx.new(|_| GeoDistanceForm {
            from: from_state.clone(),
            to: to_state.clone(),
            from_label,
            to_label,
        });
        let focus_from = from_state.clone();
        let submit = (from_state, to_state);
        let map = cx.entity().downgrade();

        ZedisDialog::new(i18n_geo_map(cx, "dist_title"))
            .w(px(420.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body_view.clone())
            .on_ok(move |_, _window, cx| {
                let from = submit.0.read(cx).value().trim().to_string();
                let to = submit.1.read(cx).value().trim().to_string();
                if from.is_empty() || to.is_empty() {
                    return false;
                }
                if let Some(map) = map.upgrade() {
                    map.update(cx, |this, inner| this.run_geo_dist(from, to, inner));
                }
                true
            })
            .open(window, cx);
        focus_from.update(cx, |state, cx| state.focus(window, cx));
    }

    /// `GEODIST`, with the answer (or "one of them is not in the key")
    /// landing in the same slot the search reports through.
    fn run_geo_dist(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = async { geo_dist(&ServerDb::new(server_id, db), &key, &from, &to).await }.await;
            let _ = this.update(cx, |this, cx| {
                this.distance = match &result {
                    Ok(Some(metres)) => Some(SharedString::from(format!("{from} ↔ {to}: {:.1} m", metres))),
                    Ok(None) => Some(i18n_geo_map(cx, "dist_missing")),
                    Err(_) => None,
                };
                if let Err(e) = result {
                    this.search_error = Some(SharedString::from(e.to_string()));
                }
                cx.notify();
            });
        }));
    }

    /// Radius-search bar: center lon/lat + radius (km) → `GEOSEARCH`.
    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let state = self.server_state.read(cx);
        let can_write = state.can(Capability::GeoWrite);
        let can_measure = state.features().is_usable(ServerCommand::GeoDist);
        let mut bar = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(Input::new(&self.lon_input).small().w(px(90.)))
            .child(Input::new(&self.lat_input).small().w(px(90.)))
            .child(Input::new(&self.radius_input).small().w(px(80.)))
            // Height only exists in box mode; in radius mode a second
            // extent would be a field with nothing to mean.
            .when(self.box_search, |this| {
                this.child(Input::new(&self.height_input).small().w(px(80.)))
            })
            .child(
                Button::new("geo-shape-toggle")
                    .small()
                    .ghost()
                    .label(if self.box_search {
                        i18n_geo_map(cx, "shape_box")
                    } else {
                        i18n_geo_map(cx, "shape_radius")
                    })
                    .tooltip(i18n_geo_map(cx, "shape_tooltip"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.box_search = !this.box_search;
                        // The old result described a different shape, so it
                        // stops being shown rather than sitting under a
                        // toggle that no longer matches it.
                        this.clear_search(cx);
                    })),
            )
            .child(
                Button::new("geo-radius-search")
                    .small()
                    .primary()
                    .label(i18n_geo_map(cx, "search"))
                    .on_click(cx.listener(|this, _, _window, cx| this.run_search(cx))),
            )
            .when(can_write, |this| {
                this.child(
                    Button::new("geo-add")
                        .small()
                        .ghost()
                        .icon(IconName::Plus)
                        .tooltip(i18n_geo_map(cx, "add_title"))
                        .on_click(cx.listener(|this, _, window, cx| this.add_point_dialog(window, cx))),
                )
            })
            .when(can_measure, |this| {
                this.child(
                    Button::new("geo-dist")
                        .small()
                        .ghost()
                        .icon(CustomIconName::Equal)
                        .tooltip(i18n_geo_map(cx, "dist_title"))
                        .on_click(cx.listener(|this, _, window, cx| this.distance_dialog(window, cx))),
                )
            });
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
        if let Some(distance) = self.distance.clone() {
            bar = bar.child(Label::new(distance).text_xs().text_color(muted));
        }
        bar
    }

    fn render_side_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let list_active = cx.theme().list_active;
        let mut col = v_flex().w(px(220.)).h_full().flex_none().gap_1().child(
            Label::new(i18n_geo_map(cx, "points"))
                .text_xs()
                .font_semibold()
                .text_color(muted),
        );

        if let Some(data) = self.data.as_ref() {
            // Virtualized: with up to GEO_CAP points, only the rows in the
            // viewport build elements. That also makes the hover→notify
            // cross-highlight (list row ↔ canvas point) cheap — a repaint
            // rebuilds ~a screenful of rows, not the whole list. The range
            // closure is `'static`, so it holds its own `Rc` of the points
            // and reaches the view through its entity handle.
            let points = data.points.clone();
            let hovered = self.hovered;
            let entity = cx.entity();
            let list = uniform_list("geo-side-list", points.len(), move |range, _window, _cx| {
                let mut out = Vec::with_capacity(range.len());
                for ix in range {
                    let p = &points[ix];
                    let active = hovered == Some(ix);
                    let entity = entity.clone();
                    out.push(
                        h_flex()
                            .id(("geo-row", ix))
                            .w_full()
                            .gap_2()
                            .items_baseline()
                            .px_1()
                            .rounded_md()
                            .when(active, |s| s.bg(list_active))
                            .hover(move |s| s.bg(list_active))
                            .on_mouse_move(move |_, _window, cx| {
                                entity.update(cx, |this, cx| {
                                    if this.hovered != Some(ix) {
                                        this.hovered = Some(ix);
                                        cx.notify();
                                    }
                                });
                            })
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
                out
            })
            .flex_1()
            .min_h_0()
            .w_full();
            col = col.child(list);

            // Invalid members (no/zero coordinates) — typically a handful,
            // so this block stays unvirtualized below the list, capped and
            // scrollable on its own if it does grow.
            if !data.invalid.is_empty() {
                let mut invalid_list = v_flex().w_full().gap_0p5().child(
                    Label::new(i18n_geo_map(cx, "invalid"))
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().yellow)
                        .pt_2(),
                );
                for (ix, member) in data.invalid.iter().enumerate() {
                    invalid_list = invalid_list.child(
                        h_flex()
                            .id(("geo-invalid", ix))
                            .w_full()
                            .px_1()
                            .child(Label::new(member.clone()).text_xs().text_color(muted).truncate()),
                    );
                }
                // Native scroll + a sibling `Scrollbar` (the help-popover
                // pattern) so the capped list shows it is scrollable —
                // bare `overflow_y_scroll` was wheel-only with no track.
                col = col.child(
                    div()
                        .relative()
                        .w_full()
                        .flex_none()
                        .child(
                            div()
                                .id("geo-invalid-list")
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .track_scroll(&self.invalid_scroll)
                                .child(invalid_list),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .child(Scrollbar::vertical(&self.invalid_scroll).mode(ScrollbarMode::Always)),
                        ),
                );
            }
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GeoSample,
        render::{nice_distance, world_to_lat, world_to_lon},
        *,
    };

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
    fn a_sample_is_projected_and_what_has_no_position_is_set_aside() {
        let member = |name: &str, position| GeoMember {
            member: name.to_string(),
            position,
        };
        let data = project_geo(GeoSample {
            total: 9,
            members: vec![
                member("beijing", Some((116.39, 39.90))),
                member("no-position", None),
                // Null Island: a plain ZADD member decodes here, not a place.
                member("null-island", Some((0.0, 0.0))),
            ],
        });
        assert_eq!(data.total, 9, "the whole key, not the sample");
        assert_eq!(data.points.len(), 1);
        assert_eq!(data.points[0].member.as_ref(), "beijing");
        let invalid: Vec<&str> = data.invalid.iter().map(|m| m.as_ref()).collect();
        assert_eq!(invalid, ["no-position", "null-island"]);
        assert!(!data.degenerate);

        // Every member on one spot: the signature of a set that is not GEO.
        let same = project_geo(GeoSample {
            total: 2,
            members: vec![member("a", Some((-180.0, -85.05))), member("b", Some((-180.0, -85.05)))],
        });
        assert!(same.degenerate);
        // Nothing to draw: the box is the whole world, not an inverted one.
        assert_eq!(project_geo(GeoSample::default()).bbox, (0.0, 0.0, 1.0, 1.0));
    }
}
