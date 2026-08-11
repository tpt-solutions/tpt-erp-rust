//! Geofencing: containment tests and distance math for fleet zones.
//!
//! A [`Geofence`] is either a [`Geofence::Circle`] (center + radius) or a
//! [`Geofence::Polygon`] (an ordered ring of [`LatLng`] vertices). Both expose a
//! total [`Geofence::contains`] predicate, and [`Geofence::crossing`] turns a GPS
//! position sample into a zone **entry/exit** [`ZoneEvent`] suitable for publishing on
//! `tpt-erp-bus`.

use serde::{Deserialize, Serialize};

/// A geographic coordinate (decimal degrees).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLng {
    pub lat: f64,
    pub lng: f64,
}

impl LatLng {
    pub fn new(lat: f64, lng: f64) -> Self {
        Self { lat, lng }
    }
}

/// Mean Earth radius in kilometers (WGS-84 average).
const EARTH_RADIUS_KM: f64 = 6371.0088;

fn to_rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

/// Great-circle distance between two coordinates, in kilometers.
pub fn haversine_km(a: LatLng, b: LatLng) -> f64 {
    let dlat = to_rad(b.lat - a.lat);
    let dlng = to_rad(b.lng - a.lng);
    let lat1 = to_rad(a.lat);
    let lat2 = to_rad(b.lat);

    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    let c = 2.0 * h.sqrt().asin();
    EARTH_RADIUS_KM * c
}

/// A fleet zone: a circle or a polygon ring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Geofence {
    /// A circle defined by a center and a radius in meters.
    Circle { center: LatLng, radius_m: f64 },
    /// A polygon ring (vertices in order; implicitly closed).
    Polygon { points: Vec<LatLng> },
}

impl Geofence {
    /// Whether `point` lies inside the fence.
    pub fn contains(&self, point: LatLng) -> bool {
        match self {
            Geofence::Circle { center, radius_m } => {
                let km = haversine_km(*center, point);
                km * 1000.0 <= *radius_m
            }
            Geofence::Polygon { points } => point_in_polygon(point, points),
        }
    }

    /// If `point` is on the *inside* of the fence while `previous` was outside, this is
    /// an `entered` event; if `point` is outside while `previous` was inside, an `exit`.
    /// Returns `None` when the membership did not change (stable inside or outside).
    pub fn crossing(&self, previous: LatLng, point: LatLng) -> Option<ZoneEvent> {
        let was_inside = self.contains(previous);
        let is_inside = self.contains(point);
        if was_inside == is_inside {
            return None;
        }
        Some(ZoneEvent {
            entered: is_inside,
            point,
        })
    }
}

/// A zone entry/exit event emitted as a vehicle crosses a fence boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoneEvent {
    /// `true` when the vehicle entered the zone, `false` on exit.
    pub entered: bool,
    /// The coordinate at the moment of crossing.
    pub point: LatLng,
}

/// Ray-casting point-in-polygon test (even-odd rule). A degenerate (≤2 vertex) ring is
/// never considered containing.
fn point_in_polygon(p: LatLng, poly: &[LatLng]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[j];
        let intersect = ((a.lat > p.lat) != (b.lat > p.lat))
            && (p.lng < (b.lng - a.lng) * (p.lat - a.lat) / (b.lat - a.lat) + a.lng);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_known_distance() {
        // ~111 km between two points 1 degree apart in latitude near the equator.
        let a = LatLng::new(0.0, 0.0);
        let b = LatLng::new(1.0, 0.0);
        let d = haversine_km(a, b);
        assert!((d - 111.0).abs() < 1.0, "got {d}");
    }

    #[test]
    fn circle_containment() {
        let fence = Geofence::Circle {
            center: LatLng::new(40.0, -73.0),
            radius_m: 1000.0,
        };
        assert!(fence.contains(LatLng::new(40.0001, -73.0)));
        assert!(!fence.contains(LatLng::new(40.02, -73.0)));
    }

    #[test]
    fn polygon_containment_and_crossing() {
        // A unit square around the origin.
        let square = Geofence::Polygon {
            points: vec![
                LatLng::new(0.0, 0.0),
                LatLng::new(0.0, 1.0),
                LatLng::new(1.0, 1.0),
                LatLng::new(1.0, 0.0),
            ],
        };
        assert!(square.contains(LatLng::new(0.5, 0.5)));
        assert!(!square.contains(LatLng::new(-0.5, 0.5)));

        // Cross from outside to inside.
        let ev = square.crossing(LatLng::new(-0.5, 0.5), LatLng::new(0.5, 0.5));
        assert_eq!(
            ev,
            Some(ZoneEvent {
                entered: true,
                point: LatLng::new(0.5, 0.5)
            })
        );

        // Stable inside => no event.
        assert_eq!(
            square.crossing(LatLng::new(0.4, 0.4), LatLng::new(0.6, 0.6)),
            None
        );
    }
}
