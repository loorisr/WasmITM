//! Géométrie sphérique (grand cercle) et extraction de profils de terrain.

use crate::dem::{normalize_lon, TileSet};

/// Rayon terrestre moyen (m).
pub const EARTH_R: f64 = 6_371_008.8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// Distance orthodromique (m), formule de haversine.
pub fn distance(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_R * h.sqrt().min(1.0).asin()
}

/// Azimut initial (radians, 0 = nord, sens horaire, dans [0, 2π)).
pub fn bearing(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (b.lon - a.lon).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).rem_euclid(std::f64::consts::TAU)
}

/// Point atteint depuis `a` selon un azimut (rad) et une distance (m).
pub fn destination(a: LatLon, brg: f64, d: f64) -> LatLon {
    let delta = d / EARTH_R;
    let p1 = a.lat.to_radians();
    let l1 = a.lon.to_radians();
    let sp2 = p1.sin() * delta.cos() + p1.cos() * delta.sin() * brg.cos();
    let p2 = sp2.clamp(-1.0, 1.0).asin();
    let l2 = l1 + (brg.sin() * delta.sin() * p1.cos()).atan2(delta.cos() - p1.sin() * sp2);
    LatLon {
        lat: p2.to_degrees(),
        lon: normalize_lon(l2.to_degrees()),
    }
}

/// Emprise (sud, ouest, nord, est) d'un cercle de rayon `r` (m) — avec une marge.
pub fn circle_bbox(c: LatLon, r: f64) -> (f64, f64, f64, f64) {
    let dlat = (r / EARTH_R).to_degrees() * 1.02 + 0.01;
    let coslat = (c.lat.abs() + dlat).min(89.0).to_radians().cos();
    let dlon = (dlat / coslat).min(180.0);
    (
        (c.lat - dlat).max(-90.0),
        c.lon - dlon,
        (c.lat + dlat).min(90.0),
        c.lon + dlon,
    )
}

/// Emprise d'un segment orthodromique (approximée par échantillonnage).
pub fn path_bbox(a: LatLon, b: LatLon) -> (f64, f64, f64, f64) {
    let d = distance(a, b);
    let brg = bearing(a, b);
    let n = ((d / 10_000.0).ceil() as usize).max(1);
    let (mut s, mut w, mut no, mut e) = (a.lat, a.lon, a.lat, a.lon);
    for i in 0..=n {
        let p = destination(a, brg, d * i as f64 / n as f64);
        // Déroule la longitude par rapport au départ pour gérer l'antiméridien.
        let mut lon = p.lon;
        if lon - a.lon > 180.0 {
            lon -= 360.0;
        } else if lon - a.lon < -180.0 {
            lon += 360.0;
        }
        s = s.min(p.lat);
        no = no.max(p.lat);
        w = w.min(lon);
        e = e.max(lon);
    }
    let m = 0.02;
    (s - m, w - m, no + m, e + m)
}

/// Profil le long d'une radiale : `n + 1` altitudes espacées de `step` à partir de `a`.
pub fn radial_profile(tiles: &TileSet, a: LatLon, brg: f64, step: f64, n: usize) -> Vec<f64> {
    (0..=n)
        .map(|i| {
            let p = destination(a, brg, step * i as f64);
            tiles.elevation(p.lat, p.lon)
        })
        .collect()
}

/// Profil entre deux points avec un pas proche de `target_step` (m).
/// Renvoie (altitudes, pas réel, distance totale).
pub fn path_profile(
    tiles: &TileSet,
    a: LatLon,
    b: LatLon,
    target_step: f64,
) -> (Vec<f64>, f64, f64) {
    let d = distance(a, b);
    let n = ((d / target_step).ceil() as usize).max(2);
    let step = d / n as f64;
    let brg = bearing(a, b);
    (radial_profile(tiles, a, brg, step, n), step, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_paris_lyon() {
        let paris = LatLon {
            lat: 48.8566,
            lon: 2.3522,
        };
        let lyon = LatLon {
            lat: 45.7640,
            lon: 4.8357,
        };
        let d = distance(paris, lyon);
        assert!((d - 392_000.0).abs() < 3_000.0, "{d}");
        let back = destination(paris, bearing(paris, lyon), d);
        assert!((back.lat - lyon.lat).abs() < 1e-6 && (back.lon - lyon.lon).abs() < 1e-6);
    }
}
