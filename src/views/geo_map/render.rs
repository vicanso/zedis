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

//! Render implementation and dialog form bodies for [`ZedisGeoMap`].

use super::*;
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
                .on_scroll(cx.listener(|this, event, _window, cx| this.on_scroll(event, cx)))
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
            .font_family(get_mono_font_family())
            .gap_3()
            .p_3()
            .child(header)
            .when(has_points, |this| this.child(self.render_search_bar(cx)))
            .child(body)
    }
}

/// Project a Web Mercator world point to canvas-local pixels, fitting the
/// data `bbox` (preserving aspect ratio) then applying user zoom/pan.
pub(super) fn project(
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
pub(super) fn unproject(
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
pub(super) fn nice_distance(m: f64) -> f64 {
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
pub(super) fn nice_scale(
    vw: f32,
    vh: f32,
    bbox: (f32, f32, f32, f32),
    zoom: f32,
    offset: Point<f32>,
) -> Option<(f32, String)> {
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
pub(super) fn haversine(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let p1 = lat1.to_radians();
    let p2 = lat2.to_radians();
    let dphi = (lat2 - lat1).to_radians();
    let dlam = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlam / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().clamp(0.0, 1.0).asin()
}

/// Web Mercator longitude → world `[0, 1]`.
pub(super) fn lon_to_world(lon: f64) -> f32 {
    (((lon + 180.0) / 360.0) as f32).clamp(0.0, 1.0)
}

/// Web Mercator latitude → world `[0, 1]` (0 = north).
pub(super) fn lat_to_world(lat: f64) -> f32 {
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78);
    let rad = lat.to_radians();
    let y = (1.0 - (std::f64::consts::FRAC_PI_4 + rad / 2.0).tan().ln() / std::f64::consts::PI) / 2.0;
    y as f32
}

pub(super) fn world_to_lon(wx: f64) -> f64 {
    wx * 360.0 - 180.0
}

pub(super) fn world_to_lat(wy: f64) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * wy);
    n.sinh().atan().to_degrees()
}

/// A sample of the key's members (`geo_sample`) projected for the map: the
/// points, the members without a usable position, the bounding box.
pub(super) fn project_geo(sample: GeoSample) -> GeoData {
    let mut points = Vec::new();
    let mut invalid = Vec::new();
    for GeoMember { member, position } in sample.members {
        match position {
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

    GeoData {
        points: Rc::new(points),
        invalid,
        total: sample.total as usize,
        bbox,
        degenerate,
    }
}

pub(super) struct GeoAddForm {
    pub(super) lon: Entity<InputState>,
    pub(super) lat: Entity<InputState>,
    pub(super) member: Entity<InputState>,
    pub(super) lon_label: SharedString,
    pub(super) lat_label: SharedString,
    pub(super) member_label: SharedString,
    pub(super) hint: SharedString,
}

impl Render for GeoAddForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(Label::new(self.lon_label.clone()).text_xs())
            .child(Input::new(&self.lon).small().h(px(32.)))
            .child(Label::new(self.lat_label.clone()).text_xs())
            .child(Input::new(&self.lat).small().h(px(32.)))
            .child(Label::new(self.member_label.clone()).text_xs())
            .child(Input::new(&self.member).small().h(px(32.)))
            .child(Label::new(self.hint.clone()).text_xs())
    }
}

pub(super) struct GeoDistanceForm {
    pub(super) from: Entity<InputState>,
    pub(super) to: Entity<InputState>,
    pub(super) from_label: SharedString,
    pub(super) to_label: SharedString,
}

impl Render for GeoDistanceForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(Label::new(self.from_label.clone()).text_xs())
            .child(Input::new(&self.from).small().h(px(32.)))
            .child(Label::new(self.to_label.clone()).text_xs())
            .child(Input::new(&self.to).small().h(px(32.)))
    }
}
