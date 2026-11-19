//! Carte de couverture calculée par ITM point à point le long de radiales.
//!
//! Pour chaque radiale, le profil complet (émetteur → rayon max) est extrait une seule fois,
//! puis ITM P2P est appelé pour chaque distance avec le sous-profil [0..k]. C'est la méthode
//! de SPLAT!/Signal-Server : chaque pixel reçoit un vrai calcul point à point avec le terrain
//! réel, et non le mode « zone » statistique d'ITM.
//! Les résultats polaires sont ensuite reportés sur une grille lat/lon (EPSG:4326) :
//! - `Interp::Nearest` (défaut) : chaque pixel prend le point (radiale, pas) le plus proche,
//!   et ITM n'est appelé qu'aux points utilisés par au moins un pixel (un appel par pixel
//!   au plus) ;
//! - `Interp::Bilinear` : ITM à tous les pas, puis interpolation bilinéaire en
//!   (azimut, distance).

//!
//! Version navigateur : le calcul est découpé en trois phases pour être réparti sur plusieurs
//! Web Workers — `plan` (pixel → point polaire, points utiles), `radials` (ITM sur une plage
//! de radiales, la partie coûteuse) et `assemble` (report sur la grille).

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use crate::dem::TileSet;
use crate::geo::{bearing, distance, radial_profile, LatLon, EARTH_R};

pub struct CoverageSpec {
    pub tx: LatLon,
    pub radius_m: f64,
    /// 1 = grille native GLO-90 ; 2, 4… = un pixel GLO-90 sur N (aperçu rapide)
    pub downsample: u32,
    pub params: itm::P2pParams,
    pub max_radials: usize,
    pub interp: Interp,
    /// Mode ITM rapide, approché (comme `-fast N` de Signal-Server) : 0 = exact (défaut) ;
    /// N ≥ 1 = horizons accélérés et Δh recalculé tous les N pas, interpolé entre deux.
    pub fast: u32,
}

/// Évaluation ITM le long d'une radiale, au pas k croissant : exacte ou rapide.
enum RadialItm<'a> {
    Exact {
        params: &'a itm::P2pParams,
        elev: &'a [f64],
        step: f64,
        hint: itm::DeltaHHint,
    },
    Fast(Box<itm::FastRadial<'a>>),
}

impl<'a> RadialItm<'a> {
    fn new(spec: &'a CoverageSpec, elev: &'a [f64], step: f64) -> Self {
        if spec.fast > 0 {
            if let Ok(f) = itm::FastRadial::new(&spec.params, elev, step, spec.fast) {
                return RadialItm::Fast(Box::new(f));
            }
        }
        RadialItm::Exact {
            params: &spec.params,
            elev,
            step,
            hint: itm::DeltaHHint::default(),
        }
    }

    #[inline]
    fn loss(&mut self, k: usize) -> Option<f32> {
        let r = match self {
            RadialItm::Exact {
                params,
                elev,
                step,
                hint,
            } => itm::p2p_hinted(params, &elev[..=k], *step, hint),
            RadialItm::Fast(f) => f.loss(k),
        };
        r.ok().map(|r| r.loss_db as f32)
    }
}

/// Report des résultats polaires sur la grille.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    /// Point polaire le plus proche ; ITM seulement aux points utilisés (le plus rapide)
    #[default]
    Nearest,
    /// Interpolation bilinéaire en (azimut, distance) ; ITM à tous les pas
    Bilinear,
}

/// 3 secondes d'arc en mètres (pas en latitude de GLO-90).
pub const ARCSEC3_M: f64 = EARTH_R * std::f64::consts::PI / 180.0 / 1200.0;

/// Taille de pixel GLO-90 (degrés lat, degrés lon) : 3" en latitude ; en longitude 3" sous 50°
/// puis 4,5", 6", 9", 15", 30" vers les pôles (tuiles de 1200, 800, 600, 400, 240, 120 colonnes).
pub fn glo90_pixel_deg(lat: f64) -> (f64, f64) {
    let a = lat.abs();
    let cols = if a < 50.0 {
        1200.0
    } else if a < 60.0 {
        800.0
    } else if a < 70.0 {
        600.0
    } else if a < 80.0 {
        400.0
    } else if a < 85.0 {
        240.0
    } else {
        120.0
    };
    (1.0 / 1200.0, 1.0 / cols)
}

/// Géométrie de la grille de sortie, calée sur les centres de pixels GLO-90.
#[derive(Debug, Clone, Copy)]
pub struct GridGeom {
    pub width: usize,
    pub height: usize,
    pub west: f64,
    pub north: f64,
    pub dlon: f64,
    pub dlat: f64,
    /// Pas radial (m) = taille du pixel en latitude
    pub step_m: f64,
    pub n_radials: usize,
}

pub fn grid_geometry(tx: LatLon, radius_m: f64, downsample: u32, max_radials: usize) -> GridGeom {
    let ds = downsample.max(1) as f64;
    let (dlat0, dlon0) = glo90_pixel_deg(tx.lat);
    let (dlat, dlon) = (dlat0 * ds, dlon0 * ds);
    let m_per_deg = EARTH_R * std::f64::consts::PI / 180.0;
    let coslat = tx.lat.to_radians().cos().max(0.01);
    let step_m = dlat * m_per_deg;
    let dx_m = dlon * m_per_deg * coslat;
    // Centres de pixels GLO-90 : multiples entiers du pas, en lat comme en lon.
    let lat_c = (tx.lat / dlat).round() * dlat;
    let lon_c = (tx.lon / dlon).round() * dlon;
    let n_lat = (radius_m / step_m).ceil() as usize;
    let n_lon = (radius_m / dx_m).ceil() as usize;
    let n_radials =
        ((TAU * radius_m / step_m.min(dx_m)).ceil() as usize).clamp(8, max_radials.max(8));
    GridGeom {
        width: 2 * n_lon + 1,
        height: 2 * n_lat + 1,
        west: lon_c - (n_lon as f64 + 0.5) * dlon,
        north: lat_c + (n_lat as f64 + 0.5) * dlat,
        dlon,
        dlat,
        step_m,
        n_radials,
    }
}

/// Grille géoréférencée (EPSG:4326). `west`/`north` = bords extérieurs du pixel (0,0).
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub west: f64,
    pub north: f64,
    pub dlon: f64,
    pub dlat: f64,
    /// Valeurs ligne par ligne du nord au sud ; NaN = pas de donnée
    pub data: Vec<f32>,
}

impl Grid {
    pub fn bounds(&self) -> Bounds {
        Bounds {
            west: self.west,
            south: self.north - self.height as f64 * self.dlat,
            east: self.west + self.width as f64 * self.dlon,
            north: self.north,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Bounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageStats {
    pub radials: usize,
    pub steps_per_radial: usize,
    pub step_m: f64,
    pub pixel_arcsec: [f64; 2],
    pub interp: Interp,
    /// 0 = ITM exact ; N = mode rapide, Δh tous les N pas
    pub fast: u32,
    pub itm_calls: u64,
    pub itm_errors: u64,
    pub width: usize,
    pub height: usize,
    /// Rempli côté JavaScript (pas d'horloge en wasm32-unknown-unknown)
    pub elapsed_ms: u128,
}

/// Position polaire fractionnaire (indice de radiale, indice de pas) d'un point, ou `None`
/// au-delà du rayon.
fn polar_pos(
    tx: LatLon,
    p: LatLon,
    radius_m: f64,
    step: f64,
    n_steps: usize,
    n_rad: usize,
) -> Option<(f64, f64)> {
    let d = distance(tx, p);
    if d > radius_m {
        return None;
    }
    let fk = (d / step).min(n_steps as f64);
    let fj = if d < 1e-6 {
        0.0
    } else {
        bearing(tx, p) / TAU * n_rad as f64
    };
    Some((fj, fk))
}

const OUTSIDE: u32 = u32::MAX;

/// Phase 1 : géométrie et, en mode `Nearest`, point polaire (j·nk + k) de chaque pixel et
/// masque des points réellement utilisés.
pub struct Plan {
    pub geom: GridGeom,
    pub n_steps: usize,
    /// Nombre de pas par radiale dans le tableau polaire (n_steps + 1)
    pub nk: usize,
    /// `Nearest` : point polaire de chaque pixel (OUTSIDE au-delà du rayon) ; vide sinon
    pub keys: Vec<u32>,
    /// `Nearest` : 1 si le point polaire est utilisé par au moins un pixel ; vide sinon
    pub needed: Vec<u8>,
}

pub fn plan(spec: &CoverageSpec) -> Plan {
    let g = grid_geometry(spec.tx, spec.radius_m, spec.downsample, spec.max_radials);
    // Au moins 2 pas : ITM n'est appelé qu'à partir du pas 2.
    let n_steps = ((spec.radius_m / g.step_m).ceil() as usize).max(2);
    let nk = n_steps + 1;
    let (mut keys, mut needed) = (Vec::new(), Vec::new());
    if spec.interp == Interp::Nearest {
        let (w, h) = (g.width, g.height);
        let (step, n_rad) = (g.step_m, g.n_radials);
        assert!(n_rad * nk < OUTSIDE as usize);
        keys = vec![OUTSIDE; w * h];
        for (r, row) in keys.chunks_mut(w).enumerate() {
            let lat = g.north - (r as f64 + 0.5) * g.dlat;
            for (c, key) in row.iter_mut().enumerate() {
                let p = LatLon {
                    lat,
                    lon: g.west + (c as f64 + 0.5) * g.dlon,
                };
                if let Some((fj, fk)) = polar_pos(spec.tx, p, spec.radius_m, step, n_steps, n_rad) {
                    let j = (fj.round() as usize) % n_rad;
                    // Les deux premiers pas (< 2 pas de l'émetteur) reprennent la valeur du pas 2.
                    let k = (fk.round() as usize).clamp(2, n_steps);
                    *key = (j * nk + k) as u32;
                }
            }
        }
        needed = vec![0u8; n_rad * nk];
        for &key in &keys {
            if key != OUTSIDE {
                needed[key as usize] = 1;
            }
        }
    }
    Plan {
        geom: g,
        n_steps,
        nk,
        keys,
        needed,
    }
}

/// Phase 2 : affaiblissement aux points polaires des radiales `j0..j1`, à plat
/// (`(j1 - j0) · nk` valeurs, NaN = non calculé). `needed` : masque de [`Plan::needed`]
/// (vide = tous les pas, mode `Bilinear`). Renvoie aussi (appels ITM, erreurs ITM).
pub fn radials(
    tiles: &TileSet,
    spec: &CoverageSpec,
    geom: &GridGeom,
    n_steps: usize,
    needed: &[u8],
    j0: usize,
    j1: usize,
) -> (Vec<f32>, u64, u64) {
    let (step, n_rad) = (geom.step_m, geom.n_radials);
    let nk = n_steps + 1;
    let j1 = j1.min(n_rad);
    let mut out = vec![f32::NAN; j1.saturating_sub(j0) * nk];
    let (mut calls, mut errors) = (0u64, 0u64);
    for j in j0..j1 {
        let row = &mut out[(j - j0) * nk..(j - j0 + 1) * nk];
        let brg = TAU * j as f64 / n_rad as f64;
        if needed.is_empty() {
            // Bilinear : ITM à chaque pas de la radiale.
            let elev = radial_profile(tiles, spec.tx, brg, step, n_steps);
            let mut eval = RadialItm::new(spec, &elev, step);
            for (k, v) in row.iter_mut().enumerate().skip(2) {
                calls += 1;
                match eval.loss(k) {
                    Some(l) => *v = l,
                    None => errors += 1,
                }
            }
            // Les deux premiers pas (< 2 pas de l'émetteur) reprennent la valeur du pas 2.
            row[0] = row[2];
            row[1] = row[2];
        } else {
            // Nearest : ITM uniquement aux points utilisés ; profil jusqu'au dernier d'entre eux.
            let need = &needed[j * nk..(j + 1) * nk];
            let Some(k_max) = need.iter().rposition(|&b| b != 0) else {
                continue;
            };
            let elev = radial_profile(tiles, spec.tx, brg, step, k_max);
            let mut eval = RadialItm::new(spec, &elev, step);
            for k in 2..=k_max {
                if need[k] != 0 {
                    calls += 1;
                    match eval.loss(k) {
                        Some(v) => row[k] = v,
                        None => errors += 1,
                    }
                }
            }
        }
    }
    (out, calls, errors)
}

/// Phase 3 : report des valeurs polaires (`n_radials · nk`, à plat) sur la grille lat/lon.
pub fn assemble(spec: &CoverageSpec, plan: &Plan, polar: &[f32]) -> Grid {
    let g = &plan.geom;
    let (w, h, nk) = (g.width, g.height, plan.nk);
    let mut data = vec![f32::NAN; w * h];
    match spec.interp {
        Interp::Nearest => {
            for (cell, &key) in data.iter_mut().zip(plan.keys.iter()) {
                if key != OUTSIDE {
                    *cell = polar[key as usize];
                }
            }
        }
        Interp::Bilinear => {
            let (step, n_rad, n_steps) = (g.step_m, g.n_radials, plan.n_steps);
            let at = |j: usize, k: usize| polar[j * nk + k];
            for (r, row) in data.chunks_mut(w).enumerate() {
                let lat = g.north - (r as f64 + 0.5) * g.dlat;
                for (c, cell) in row.iter_mut().enumerate() {
                    let p = LatLon {
                        lat,
                        lon: g.west + (c as f64 + 0.5) * g.dlon,
                    };
                    let Some((fj, fk)) = polar_pos(spec.tx, p, spec.radius_m, step, n_steps, n_rad)
                    else {
                        continue;
                    };
                    let k0 = (fk.floor() as usize).min(n_steps);
                    let k1 = (k0 + 1).min(n_steps);
                    let wk = fk - k0 as f64;
                    let j0 = (fj.floor() as usize) % n_rad;
                    let j1 = (j0 + 1) % n_rad;
                    let wj = fj - fj.floor();
                    let samples = [
                        (at(j0, k0), (1.0 - wj) * (1.0 - wk)),
                        (at(j0, k1), (1.0 - wj) * wk),
                        (at(j1, k0), wj * (1.0 - wk)),
                        (at(j1, k1), wj * wk),
                    ];
                    let (mut acc, mut wsum) = (0.0f64, 0.0f64);
                    for (v, w) in samples {
                        if v.is_finite() && w > 0.0 {
                            acc += v as f64 * w;
                            wsum += w;
                        }
                    }
                    if wsum > 0.0 {
                        *cell = (acc / wsum) as f32;
                    } else if let Some(v) = samples.iter().map(|s| s.0).find(|v| v.is_finite()) {
                        *cell = v;
                    }
                }
            }
        }
    }
    Grid {
        width: w,
        height: h,
        west: g.west,
        north: g.north,
        dlon: g.dlon,
        dlat: g.dlat,
        data,
    }
}

pub fn stats(spec: &CoverageSpec, plan: &Plan, itm_calls: u64, itm_errors: u64) -> CoverageStats {
    let g = &plan.geom;
    CoverageStats {
        radials: g.n_radials,
        steps_per_radial: plan.n_steps,
        step_m: g.step_m,
        pixel_arcsec: [g.dlat * 3600.0, g.dlon * 3600.0],
        interp: spec.interp,
        fast: spec.fast,
        itm_calls,
        itm_errors,
        width: g.width,
        height: g.height,
        elapsed_ms: 0,
    }
}

/// Calcul complet en une seule fois (tests et outils natifs).
pub fn compute(tiles: &TileSet, spec: &CoverageSpec) -> (Grid, CoverageStats) {
    let p = plan(spec);
    let (polar, calls, errors) = radials(
        tiles,
        spec,
        &p.geom,
        p.n_steps,
        &p.needed,
        0,
        p.geom.n_radials,
    );
    let grid = assemble(spec, &p, &polar);
    (grid, stats(spec, &p, calls, errors))
}

/// Point utile aux tests : position d'un pixel.
#[allow(dead_code)]
pub fn pixel_center(g: &Grid, r: usize, c: usize) -> LatLon {
    LatLon {
        lat: g.north - (r as f64 + 0.5) * g.dlat,
        lon: g.west + (c as f64 + 0.5) * g.dlon,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_aligned_on_glo90_pixels() {
        let tx = LatLon {
            lat: 45.123456,
            lon: 5.654321,
        };
        let g = grid_geometry(tx, 10_000.0, 1, 7200);
        // Centre du pixel (0,0) sur la grille des centres Copernicus (multiples de 1/1200°)
        let lat0 = g.north - g.dlat / 2.0;
        let lon0 = g.west + g.dlon / 2.0;
        assert!(((lat0 * 1200.0) - (lat0 * 1200.0).round()).abs() < 1e-6);
        assert!(((lon0 * 1200.0) - (lon0 * 1200.0).round()).abs() < 1e-6);
        assert!((g.step_m - 92.66).abs() < 0.05, "{}", g.step_m);
        // 10 km / 92,7 m -> 108 pixels de part et d'autre en latitude
        assert_eq!(g.height, 2 * 108 + 1);
        // Au nord de 50° : 800 colonnes par degré
        let g2 = grid_geometry(
            LatLon {
                lat: 55.3,
                lon: 10.1,
            },
            5_000.0,
            1,
            7200,
        );
        assert!((g2.dlon - 1.0 / 800.0).abs() < 1e-12);
        // Aperçu x4
        let g4 = grid_geometry(tx, 10_000.0, 4, 7200);
        assert!((g4.dlat - 4.0 / 1200.0).abs() < 1e-12);
    }

    fn synthetic_tiles() -> TileSet {
        // Tuile 45..46 N, 5..6 E : relief ondulé, PixelIsPoint, 1201 x 1201.
        let n = 1201;
        let mut data = vec![0f32; n * n];
        for r in 0..n {
            for c in 0..n {
                let (x, y) = (c as f64 / 40.0, r as f64 / 55.0);
                data[r * n + c] =
                    (300.0 + 250.0 * x.sin() * y.cos() + 80.0 * (x * 3.1 + y).sin()) as f32;
            }
        }
        let mut ts = TileSet::default();
        ts.insert(
            crate::dem::TileKey { lat: 45, lon: 5 },
            Some(crate::dem::Tile {
                width: n,
                height: n,
                lon0: 5.0,
                lat0: 46.0,
                dlon: 1.0 / 1200.0,
                dlat: 1.0 / 1200.0,
                data,
            }),
        );
        ts
    }

    #[test]
    fn chunked_radials_match_whole() {
        let ts = synthetic_tiles();
        for (interp, fast) in [
            (Interp::Nearest, 0),
            (Interp::Bilinear, 0),
            (Interp::Nearest, 50),
        ] {
            let spec = CoverageSpec {
                tx: LatLon {
                    lat: 45.5,
                    lon: 5.5,
                },
                radius_m: 15_000.0,
                downsample: 2,
                params: itm::P2pParams {
                    h_tx_m: 30.0,
                    h_rx_m: 2.0,
                    climate: itm::Climate::ContinentalTemperate,
                    n0: 301.0,
                    f_mhz: 145.0,
                    polarization: itm::Polarization::Vertical,
                    epsilon: 15.0,
                    sigma: 0.005,
                    mdvar: 12,
                    time: 50.0,
                    location: 50.0,
                    situation: 50.0,
                },
                max_radials: 7200,
                interp,
                fast,
            };
            let (whole, st) = compute(&ts, &spec);
            assert!(st.itm_calls > 0 && st.itm_errors == 0);
            let p = plan(&spec);
            let (n, nk) = (p.geom.n_radials, p.nk);
            let mut polar = vec![f32::NAN; n * nk];
            let mut j0 = 0;
            while j0 < n {
                let j1 = (j0 + 37).min(n);
                let (v, _, _) = radials(&ts, &spec, &p.geom, p.n_steps, &p.needed, j0, j1);
                polar[j0 * nk..j1 * nk].copy_from_slice(&v);
                j0 = j1;
            }
            let g = assemble(&spec, &p, &polar);
            assert_eq!(g.data.len(), whole.data.len());
            assert!(g
                .data
                .iter()
                .zip(&whole.data)
                .all(|(a, b)| a.to_bits() == b.to_bits()));
            assert!(whole.data.iter().filter(|v| v.is_finite()).count() > 1000);
        }
    }

    #[test]
    fn geotiff_roundtrip() {
        let data: Vec<f32> = (0..1201 * 1201).map(|i| (i % 977) as f32).collect();
        let geo = crate::raster_out::GeoRef {
            west: 5.0 - 0.5 / 1200.0,
            north: 46.0 + 0.5 / 1200.0,
            dlon: 1.0 / 1200.0,
            dlat: 1.0 / 1200.0,
            pixel_is_point: false,
        };
        let tif = crate::raster_out::geotiff_f32(1201, 1201, &data, &geo, None).unwrap();
        let t = crate::dem::read_geotiff_bytes(&tif).unwrap();
        assert_eq!((t.width, t.height), (1201, 1201));
        assert!((t.lon0 - 5.0).abs() < 1e-12 && (t.lat0 - 46.0).abs() < 1e-12);
        assert_eq!(t.data, data);
    }
}
