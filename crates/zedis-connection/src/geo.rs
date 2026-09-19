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

//! Geo keys: what the map view reads and writes. The projection onto a map,
//! the bounding box and what counts as a suspicious point are the view's
//! business; this is the data.

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::error::Error;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One member of a geo key, with the `(longitude, latitude)` `GEOPOS` decoded
/// for it — `None` for a nil reply.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoMember {
    pub member: String,
    pub position: Option<(f64, f64)>,
}

/// Up to a cap of a geo key's members, and how many there are in all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoSample {
    /// `ZCARD`.
    pub total: u64,
    pub members: Vec<GeoMember>,
}

/// What a `GEOSEARCH` covers. Redis offers both and they answer different
/// questions — "within 5 km of here" versus "inside this tile" — so the map
/// offers both rather than approximating one with the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GeoShape {
    /// Metres from the centre (`BYRADIUS`).
    Radius(f64),
    /// Full width and height in metres (`BYBOX`).
    Box { width_m: f64, height_m: f64 },
}

impl GeoShape {
    /// Half-extents in metres, east–west and north–south. A circle is the
    /// degenerate case where both are the radius.
    pub fn half_extents_m(self) -> (f64, f64) {
        match self {
            GeoShape::Radius(r) => (r, r),
            GeoShape::Box { width_m, height_m } => (width_m / 2.0, height_m / 2.0),
        }
    }
}

/// `ZCARD` + up to `cap` members + their `GEOPOS`.
///
/// At or under the cap the range is the whole set. Over it, `ZRANDMEMBER`
/// draws an unbiased sample: `ZRANGE 0..cap` would take the *lowest geohash
/// scores*, which sort geographically — one corner of the world — so a map
/// would claim the data lives only there. Servers without `ZRANDMEMBER`
/// (pre-6.2, some proxies) get that biased corner rather than nothing.
pub async fn geo_sample(at: &ServerDb, key: &str, cap: usize) -> Result<GeoSample> {
    let mut conn = at.connection().await?;
    let cap = cap as i64;
    let total: i64 = cmd("ZCARD").arg(key).query_async(&mut conn).await.unwrap_or(0);
    let names: Vec<String> = if total <= cap {
        cmd("ZRANGE")
            .arg(key)
            .arg(0)
            .arg(cap - 1)
            .query_async(&mut conn)
            .await?
    } else {
        // A positive count returns distinct members, at most `cap`.
        match cmd("ZRANDMEMBER").arg(key).arg(cap).query_async(&mut conn).await {
            Ok(names) => names,
            Err(_) => {
                cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(cap - 1)
                    .query_async(&mut conn)
                    .await?
            }
        }
    };
    let positions = positions_of(&mut conn, key, &names).await?;
    Ok(GeoSample {
        total: total.max(0) as u64,
        members: names
            .into_iter()
            .zip(positions)
            .map(|(member, position)| GeoMember { member, position })
            .collect(),
    })
}

/// `GEOSEARCH key FROMLONLAT lon lat BYRADIUS|BYBOX … km ASC COUNT cap` — the
/// names of the members inside `shape`, nearest first.
pub async fn geo_search(
    at: &ServerDb,
    key: &str,
    lon: f64,
    lat: f64,
    shape: GeoShape,
    cap: usize,
) -> Result<Vec<String>> {
    let mut command = cmd("GEOSEARCH");
    command.arg(key).arg("FROMLONLAT").arg(lon).arg(lat);
    match shape {
        GeoShape::Radius(radius_m) => {
            command.arg("BYRADIUS").arg(radius_m / 1000.0).arg("km");
        }
        // Width then height, both in the same unit as the radius form.
        GeoShape::Box { width_m, height_m } => {
            command
                .arg("BYBOX")
                .arg(width_m / 1000.0)
                .arg(height_m / 1000.0)
                .arg("km");
        }
    }
    command.arg("ASC").arg("COUNT").arg(cap as i64);
    Ok(command.query_async(&mut at.connection().await?).await?)
}

/// Cheap heuristic: does this sorted set hold GEO data? Probes the first
/// couple of members with `GEOPOS` and checks the decoded coordinates.
///
/// `GEOPOS` decodes *any* sorted-set score as a geohash, so a plain `ZADD`
/// set (e.g. all score 0) collapses to the south-west corner
/// `(-180, -85.05)`. The set counts as GEO only when at least one probed
/// member decodes to a real coordinate away from that corner — enough to
/// decide whether to offer a map. Any failure is "no".
pub async fn zset_looks_geo(at: &ServerDb, key: &str) -> bool {
    let Ok(mut conn) = at.connection().await else {
        return false;
    };
    let names: Vec<String> = cmd("ZRANGE")
        .arg(key)
        .arg(0)
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    if names.is_empty() {
        return false;
    }
    positions_of(&mut conn, key, &names)
        .await
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .any(|(lon, lat)| lon > -179.99 || lat > -85.0)
}

/// `GEOPOS key member…`, one entry per member.
async fn positions_of(
    conn: &mut crate::conn::RedisAsyncConn,
    key: &str,
    members: &[String],
) -> Result<Vec<Option<(f64, f64)>>> {
    if members.is_empty() {
        return Ok(Vec::new());
    }
    let mut geopos = cmd("GEOPOS");
    geopos.arg(key);
    for member in members {
        geopos.arg(member);
    }
    let raw: Value = geopos.query_async(conn).await?;
    let Value::Array(items) = raw else {
        return Ok(vec![None; members.len()]);
    };
    let mut positions: Vec<_> = items.iter().map(parse_lon_lat).collect();
    positions.resize(members.len(), None);
    Ok(positions)
}

/// One `GEOPOS` element: `[lon, lat]`, or nil.
fn parse_lon_lat(value: &Value) -> Option<(f64, f64)> {
    let Value::Array(pair) = value else {
        return None;
    };
    Some((reply::float(pair.first()?)?, reply::float(pair.get(1)?)?))
}

/// `GEOADD key longitude latitude member` — the only way to put a point in.
///
/// A geo key is a sorted set whose score is a geohash, so the sorted-set
/// editor cannot add one: nobody computes that score by hand. Longitude
/// comes first, which is the opposite of how coordinates are usually spoken,
/// so the dialog labels both.
pub async fn geo_add(at: &ServerDb, key: &str, lon: f64, lat: f64, member: &str) -> Result<i64> {
    Ok(cmd("GEOADD")
        .arg(key)
        .arg(lon)
        .arg(lat)
        .arg(member)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `GEODIST key member1 member2 m` — metres, or `None` when either member is
/// absent.
///
/// The unit is **lowercase on purpose**: Redis 6.2 compares it
/// case-sensitively and answers "unsupported unit provided. please use m,
/// km, ft, mi" for `M`, while later versions accept either. Lowercase works
/// everywhere, so don't "tidy" it to match the other argument keywords.
pub async fn geo_dist(at: &ServerDb, key: &str, from: &str, to: &str) -> Result<Option<f64>> {
    let raw: Option<String> = cmd("GEODIST")
        .arg(key)
        .arg(from)
        .arg(to)
        .arg("m")
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(raw.and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_geopos_element_is_a_pair_or_nothing() {
        let pair = Value::Array(vec![
            Value::BulkString(b"116.39".to_vec()),
            Value::BulkString(b"39.90".to_vec()),
        ]);
        assert_eq!(parse_lon_lat(&pair), Some((116.39, 39.90)));
        assert_eq!(
            parse_lon_lat(&Value::Array(vec![Value::Double(1.5), Value::Double(-2.0)])),
            Some((1.5, -2.0))
        );
        assert_eq!(parse_lon_lat(&Value::Nil), None);
        assert_eq!(
            parse_lon_lat(&Value::Array(vec![Value::Double(1.5)])),
            None,
            "half a pair"
        );
    }

    #[test]
    fn a_circle_is_a_box_with_equal_halves() {
        assert_eq!(GeoShape::Radius(500.0).half_extents_m(), (500.0, 500.0));
        assert_eq!(
            GeoShape::Box {
                width_m: 400.0,
                height_m: 100.0
            }
            .half_extents_m(),
            (200.0, 50.0)
        );
    }
}
