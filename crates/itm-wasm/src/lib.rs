//! ITM (Longley-Rice) + MNE Copernicus GLO-90 en WebAssembly.
//!
//! Portage navigateur de l'API HTTP de RustITM : mêmes requêtes JSON, mêmes réponses, mêmes
//! validations. Chaque Web Worker a sa propre instance (état global ci-dessous : tuiles
//! chargées et plan de la couverture en cours). Les tuiles sont téléchargées côté JavaScript.
//!
//! Les erreurs sont renvoyées sous forme de chaîne JSON `{"status", "error", "itm_code"}`.

pub mod coverage;
pub mod dem;
pub mod error;
pub mod geo;
pub mod radio;
pub mod raster_out;

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use serde_json::json;
use wasm_bindgen::prelude::*;

use crate::coverage::{CoverageSpec, Plan};
use crate::dem::{keys_for_bbox, TileKey, TileSet};
use crate::error::AppError;
use crate::geo::LatLon;
use crate::radio::{field_strength_dbuvm, rx_power_dbm, ItmInput, LinkBudget};

// ---------------------------------------------------------------- limites

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_p2p_km: f64,
    pub max_radius_km: f64,
    pub max_grid_side: usize,
    pub max_tiles: usize,
    pub max_radials: usize,
    pub profile_step_m: f64,
    pub max_elevation_points: usize,
}

pub const LIMITS: Limits = Limits {
    max_p2p_km: 1000.0,
    max_radius_km: 150.0,
    max_grid_side: 6001,
    max_tiles: 64,
    max_radials: 7200,
    profile_step_m: coverage::ARCSEC3_M,
    max_elevation_points: 10_000,
};

// ---------------------------------------------------------------- état du worker

struct CoverageState {
    body: String,
    req: CoverageRequest,
    spec: CoverageSpec,
    plan: Plan,
}

#[derive(Default)]
struct State {
    tiles: TileSet,
    cov: Option<CoverageState>,
    /// (appels ITM, erreurs ITM) du dernier `coverage_radials`
    counts: (u64, u64),
    /// Métadonnées du dernier `coverage_finish`
    meta: String,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn js_err(e: AppError) -> JsValue {
    JsValue::from_str(&e.to_json())
}

type JsResult<T> = Result<T, JsValue>;

// ---------------------------------------------------------------- validations

fn check_point(p: LatLon, what: &str) -> Result<(), AppError> {
    if !(-90.0..=90.0).contains(&p.lat)
        || !(-180.0..=180.0).contains(&p.lon)
        || !p.lat.is_finite()
        || !p.lon.is_finite()
    {
        return Err(AppError::bad_request(format!(
            "{what} : coordonnées invalides"
        )));
    }
    if p.lat.abs() > 85.0 {
        return Err(AppError::bad_request(format!(
            "{what} : latitude au-delà de ±85° non supportée"
        )));
    }
    Ok(())
}

fn keys_checked(bbox: (f64, f64, f64, f64), max_tiles: usize) -> Result<Vec<TileKey>, AppError> {
    let keys = keys_for_bbox(bbox.0, bbox.1, bbox.2, bbox.3);
    if keys.len() > max_tiles {
        return Err(AppError::bad_request(format!(
            "zone trop grande : {} tuiles nécessaires (max {max_tiles})",
            keys.len()
        )));
    }
    Ok(keys)
}

fn keys_json(keys: &[TileKey]) -> String {
    let v: Vec<serde_json::Value> = keys
        .iter()
        .map(|k| json!({"lat": k.lat, "lon": k.lon, "name": k.name_ot()}))
        .collect();
    serde_json::to_string(&v).unwrap()
}

// ---------------------------------------------------------------- tuiles

/// Charge une tuile décodée dans ce worker. `bytes` absent = tuile inexistante (mer).
#[wasm_bindgen]
pub fn load_tile(lat: i32, lon: i32, bytes: Option<Box<[u8]>>) -> JsResult<()> {
    let tile = match bytes {
        Some(b) => Some(dem::read_geotiff_bytes(&b).map_err(|e| js_err(AppError::internal(e)))?),
        None => None,
    };
    ST.with_borrow_mut(|s| s.tiles.insert(TileKey { lat, lon }, tile));
    Ok(())
}

#[wasm_bindgen]
pub fn has_tile(lat: i32, lon: i32) -> bool {
    ST.with_borrow(|s| s.tiles.contains(TileKey { lat, lon }))
}

#[wasm_bindgen]
pub fn unload_tile(lat: i32, lon: i32) {
    ST.with_borrow_mut(|s| s.tiles.remove(TileKey { lat, lon }));
}

/// Vérifie qu'un fichier est une tuile lisible et renvoie la clé de la tuile qui contient
/// son centre (import de fichiers locaux).
#[wasm_bindgen]
pub fn tile_key_of(bytes: &[u8]) -> JsResult<String> {
    let t = dem::read_geotiff_bytes(bytes).map_err(|e| js_err(AppError::bad_request(e)))?;
    let lat = t.lat0 - (t.height as f64 - 1.0) * t.dlat / 2.0;
    let lon = t.lon0 + (t.width as f64 - 1.0) * t.dlon / 2.0;
    Ok(keys_json(&[TileKey::containing(lat, lon)]))
}

// ---------------------------------------------------------------- info

#[wasm_bindgen]
pub fn info() -> String {
    let l = &LIMITS;
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "runtime": "wasm",
        "limits": {
            "max_p2p_km": l.max_p2p_km,
            "max_radius_km": l.max_radius_km,
            "max_grid_side": l.max_grid_side,
            "max_tiles": l.max_tiles,
            "profile_step_m": l.profile_step_m,
            "max_downsample": 8,
            "max_radials": l.max_radials,
        },
        "grid": {
            "kind": "GLO-90 native (centres de pixels Copernicus)",
            "dlat_arcsec": 3.0,
            "dlon_arcsec_by_abs_lat": [[0, 3.0], [50, 4.5], [60, 6.0], [70, 9.0], [80, 15.0], [85, 30.0]],
            "arcsec3_m": coverage::ARCSEC3_M,
        },
        "dem": { "source": "Copernicus GLO-90 (miroir OpenTopography)", "download": true }
    })
    .to_string()
}

// ---------------------------------------------------------------- altitude

#[derive(Deserialize, Serialize, Clone, Copy)]
struct Point {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct ElevationBody {
    points: Vec<Point>,
}

fn elevation_bbox(b: &ElevationBody) -> Result<(f64, f64, f64, f64), AppError> {
    let max = LIMITS.max_elevation_points;
    if b.points.is_empty() || b.points.len() > max {
        return Err(AppError::bad_request(format!("1 à {max} points")));
    }
    for p in &b.points {
        check_point(
            LatLon {
                lat: p.lat,
                lon: p.lon,
            },
            "point",
        )?;
    }
    let s = b.points.iter().map(|p| p.lat).fold(f64::MAX, f64::min) - 0.01;
    let n = b.points.iter().map(|p| p.lat).fold(f64::MIN, f64::max) + 0.01;
    let w = b.points.iter().map(|p| p.lon).fold(f64::MAX, f64::min) - 0.01;
    let e = b.points.iter().map(|p| p.lon).fold(f64::MIN, f64::max) + 0.01;
    Ok((s, w, n, e))
}

/// Tuiles nécessaires à `POST /v1/elevation` (`{"points": [{lat, lon}, …]}`).
#[wasm_bindgen]
pub fn elevation_tiles(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let b: ElevationBody = serde_json::from_str(body)?;
        let max = if b.points.len() == 1 {
            4
        } else {
            LIMITS.max_tiles
        };
        Ok(keys_json(&keys_checked(elevation_bbox(&b)?, max)?))
    };
    run().map_err(js_err)
}

/// `POST /v1/elevation` : `{"results": [{lat, lon, elevation_m}, …]}`.
#[wasm_bindgen]
pub fn elevation(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let b: ElevationBody = serde_json::from_str(body)?;
        elevation_bbox(&b)?;
        let out: Vec<serde_json::Value> = ST.with_borrow(|s| {
            b.points
                .iter()
                .map(|p| json!({"lat": p.lat, "lon": p.lon, "elevation_m": s.tiles.elevation(p.lat, p.lon)}))
                .collect()
        });
        Ok(json!({ "results": out }).to_string())
    };
    run().map_err(js_err)
}

// ---------------------------------------------------------------- point à point

#[derive(Deserialize, Clone, Copy)]
struct Terminal {
    lat: f64,
    lon: f64,
    /// Hauteur d'antenne au-dessus du sol (m)
    height_m: f64,
}

#[derive(Deserialize)]
struct P2pRequest {
    tx: Terminal,
    rx: Terminal,
    #[serde(flatten)]
    itm: ItmInput,
    #[serde(flatten)]
    budget: LinkBudget,
    #[serde(default)]
    include_profile: bool,
    profile_step_m: Option<f64>,
}

#[derive(Serialize)]
struct P2pResponse {
    path_loss_db: f64,
    free_space_loss_db: f64,
    excess_loss_db: f64,
    mode: &'static str,
    path_geometry: &'static str,
    distance_km: f64,
    bearing_deg: f64,
    tx_ground_m: f64,
    rx_ground_m: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rx_power_dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field_strength_dbuvm: Option<f64>,
    warnings: WarningsOut,
    intermediate: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<ProfileOut>,
}

#[derive(Serialize)]
struct WarningsOut {
    flags: u32,
    messages: Vec<&'static str>,
}

#[derive(Serialize)]
struct ProfileOut {
    step_m: f64,
    elevations_m: Vec<f64>,
}

struct P2pChecked {
    req: P2pRequest,
    a: LatLon,
    b: LatLon,
    params: itm::P2pParams,
    step: f64,
}

fn p2p_check(body: &str) -> Result<P2pChecked, AppError> {
    let req: P2pRequest = serde_json::from_str(body)?;
    let a = LatLon {
        lat: req.tx.lat,
        lon: req.tx.lon,
    };
    let b = LatLon {
        lat: req.rx.lat,
        lon: req.rx.lon,
    };
    check_point(a, "tx")?;
    check_point(b, "rx")?;
    let params = req.itm.to_params(req.tx.height_m, req.rx.height_m)?;
    let d = geo::distance(a, b);
    if d < 100.0 {
        return Err(AppError::bad_request(
            "distance tx-rx trop courte (< 100 m)",
        ));
    }
    if d > LIMITS.max_p2p_km * 1000.0 {
        return Err(AppError::bad_request(format!(
            "distance > {} km",
            LIMITS.max_p2p_km
        )));
    }
    let step = req
        .profile_step_m
        .unwrap_or(LIMITS.profile_step_m)
        .clamp(10.0, 1000.0);
    Ok(P2pChecked {
        req,
        a,
        b,
        params,
        step,
    })
}

/// Tuiles nécessaires à `POST /v1/p2p`.
#[wasm_bindgen]
pub fn p2p_tiles(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let c = p2p_check(body)?;
        Ok(keys_json(&keys_checked(
            geo::path_bbox(c.a, c.b),
            LIMITS.max_tiles,
        )?))
    };
    run().map_err(js_err)
}

/// `POST /v1/p2p`.
#[wasm_bindgen]
pub fn p2p(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let P2pChecked {
            req,
            a,
            b,
            params,
            step,
        } = p2p_check(body)?;
        let (elev, real_step, dist) = ST.with_borrow(|s| geo::path_profile(&s.tiles, a, b, step));
        let r = itm::p2p(&params, &elev, real_step)?;
        let iv = r.intermediate;
        let rx_power = req
            .budget
            .eirp_dbm
            .map(|p| rx_power_dbm(p, req.budget.rx_gain_dbi, r.loss_db));
        let field = req
            .budget
            .eirp_dbm
            .map(|p| field_strength_dbuvm(p, r.loss_db, params.f_mhz));
        let resp = P2pResponse {
            path_loss_db: r.loss_db,
            free_space_loss_db: iv.a_fs_db,
            excess_loss_db: r.loss_db - iv.a_fs_db,
            mode: iv.mode.as_str(),
            path_geometry: iv.path_geometry.as_str(),
            distance_km: dist / 1000.0,
            bearing_deg: geo::bearing(a, b).to_degrees(),
            tx_ground_m: elev[0],
            rx_ground_m: *elev.last().unwrap(),
            rx_power_dbm: rx_power,
            field_strength_dbuvm: field,
            warnings: WarningsOut {
                flags: r.warnings.0,
                messages: r.warnings.descriptions(),
            },
            intermediate: json!({
                "reference_attenuation_db": iv.a_ref_db,
                "delta_h_m": iv.delta_h_m,
                "horizon_distance_m": iv.d_hzn_m,
                "terrain_horizon_distance_m": iv.terrain_d_hzn_m,
                "effective_height_m": iv.h_e_m,
                "horizon_angle_rad": iv.theta_hzn,
                "surface_refractivity": iv.n_s,
            }),
            profile: req.include_profile.then_some(ProfileOut {
                step_m: real_step,
                elevations_m: elev,
            }),
        };
        Ok(serde_json::to_string(&resp)?)
    };
    run().map_err(js_err)
}

// ---------------------------------------------------------------- couverture

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
enum Quantity {
    #[default]
    PathLoss,
    RxPower,
    FieldStrength,
}

fn d_rx_h() -> f64 {
    2.0
}
fn d_ds() -> u32 {
    1
}

#[derive(Deserialize)]
struct CoverageRequest {
    tx: Terminal,
    #[serde(default = "d_rx_h")]
    rx_height_m: f64,
    radius_km: f64,
    /// 1 = grille native GLO-90 (défaut) ; N = un pixel GLO-90 sur N (aperçu)
    #[serde(default = "d_ds")]
    downsample: u32,
    /// `nearest` (défaut, un appel ITM par pixel) ou `bilinear` (ancien rendu lissé)
    #[serde(default)]
    interp: coverage::Interp,
    /// ITM rapide, approché : absent / `false` / 0 = exact (défaut) ; `true` = N=50 ;
    /// entier N ≥ 1 = Δh recalculé tous les N pas (1 : quasi exact)
    #[serde(default)]
    fast: FastOpt,
    #[serde(flatten)]
    itm: ItmInput,
    #[serde(flatten)]
    budget: LinkBudget,
    #[serde(default)]
    output: Quantity,
}

/// Option `fast` : booléen ou pas N.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(untagged)]
enum FastOpt {
    #[default]
    Off,
    Flag(bool),
    Stride(u32),
}

impl FastOpt {
    fn stride(self) -> u32 {
        match self {
            FastOpt::Off | FastOpt::Flag(false) => 0,
            FastOpt::Flag(true) => itm::FAST_DEFAULT_STRIDE,
            FastOpt::Stride(n) => n,
        }
    }
}

fn coverage_check(body: &str) -> Result<(CoverageRequest, CoverageSpec), AppError> {
    let req: CoverageRequest = serde_json::from_str(body)?;
    let tx = LatLon {
        lat: req.tx.lat,
        lon: req.tx.lon,
    };
    check_point(tx, "tx")?;
    let lim = &LIMITS;
    if !(req.radius_km > 0.0 && req.radius_km <= lim.max_radius_km) {
        return Err(AppError::bad_request(format!(
            "radius_km doit être dans ]0, {}]",
            lim.max_radius_km
        )));
    }
    if !(1..=8).contains(&req.downsample) {
        return Err(AppError::bad_request(
            "downsample doit être entre 1 (natif GLO-90) et 8",
        ));
    }
    let g = coverage::grid_geometry(tx, req.radius_km * 1000.0, req.downsample, lim.max_radials);
    if g.width.max(g.height) > lim.max_grid_side {
        return Err(AppError::bad_request(format!(
            "grille trop grande ({}x{}, max {2}x{2}) : réduire radius_km ou utiliser downsample",
            g.width, g.height, lim.max_grid_side
        )));
    }
    if req.output != Quantity::PathLoss && req.budget.eirp_dbm.is_none() {
        return Err(AppError::bad_request(
            "eirp_dbm est requis pour rx_power et field_strength",
        ));
    }
    let params = req.itm.to_params(req.tx.height_m, req.rx_height_m)?;
    let fast = req.fast.stride();
    if fast > 100_000 {
        return Err(AppError::bad_request(
            "fast doit être un booléen ou un entier entre 0 et 100000",
        ));
    }
    let spec = CoverageSpec {
        tx,
        radius_m: req.radius_km * 1000.0,
        downsample: req.downsample,
        params,
        max_radials: lim.max_radials,
        interp: req.interp,
        fast,
    };
    Ok((req, spec))
}

/// Valide une requête `POST /v1/coverage` et renvoie
/// `{"tiles": [...], "radials": n, "nk": n}`. Sans calcul.
#[wasm_bindgen]
pub fn coverage_tiles(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let (_, spec) = coverage_check(body)?;
        let keys = keys_checked(geo::circle_bbox(spec.tx, spec.radius_m), LIMITS.max_tiles)?;
        Ok(keys_json(&keys))
    };
    run().map_err(js_err)
}

/// Phase 1 (un seul worker) : plan de la couverture, gardé dans l'état du worker.
/// Renvoie `{"radials", "nk", "width", "height"}`.
#[wasm_bindgen]
pub fn coverage_plan(body: &str) -> JsResult<String> {
    let run = || -> Result<String, AppError> {
        let (req, spec) = coverage_check(body)?;
        let plan = coverage::plan(&spec);
        let out = json!({
            "radials": plan.geom.n_radials,
            "nk": plan.nk,
            "width": plan.geom.width,
            "height": plan.geom.height,
        })
        .to_string();
        ST.with_borrow_mut(|s| {
            s.cov = Some(CoverageState {
                body: body.to_string(),
                req,
                spec,
                plan,
            })
        });
        Ok(out)
    };
    run().map_err(js_err)
}

/// Masque des points polaires utiles du plan courant (vide en mode `bilinear`).
#[wasm_bindgen]
pub fn coverage_needed() -> Vec<u8> {
    ST.with_borrow_mut(|s| {
        s.cov
            .as_mut()
            .map(|c| c.plan.needed.clone())
            .unwrap_or_default()
    })
}

/// Phase 2 (tous les workers) : radiales `j0..j1`, `(j1 - j0) · nk` valeurs à plat.
/// `needed` : masque de [`coverage_needed`] (vide en mode `bilinear`).
/// Les compteurs (appels, erreurs) sont lus ensuite avec [`last_counts`].
#[wasm_bindgen]
pub fn coverage_radials(body: &str, needed: &[u8], j0: usize, j1: usize) -> JsResult<Vec<f32>> {
    let run = || -> Result<Vec<f32>, AppError> {
        let (_, spec) = coverage_check(body)?;
        let geom =
            coverage::grid_geometry(spec.tx, spec.radius_m, spec.downsample, spec.max_radials);
        let n_steps = ((spec.radius_m / geom.step_m).ceil() as usize).max(2);
        if !needed.is_empty() && needed.len() != geom.n_radials * (n_steps + 1) {
            return Err(AppError::internal("masque de points polaires incohérent"));
        }
        let (out, calls, errors) =
            ST.with_borrow(|s| coverage::radials(&s.tiles, &spec, &geom, n_steps, needed, j0, j1));
        ST.with_borrow_mut(|s| s.counts = (calls, errors));
        Ok(out)
    };
    run().map_err(js_err)
}

/// `[appels ITM, erreurs ITM]` du dernier [`coverage_radials`] de ce worker.
#[wasm_bindgen]
pub fn last_counts() -> Vec<f64> {
    ST.with_borrow(|s| vec![s.counts.0 as f64, s.counts.1 as f64])
}

/// Phase 3 (le worker du plan) : report sur la grille et conversion de grandeur.
/// Renvoie la grille (float32, nord → sud, NaN = pas de donnée) ; les métadonnées
/// (`bounds`, `unit`, `width`, `height`, `stats`) sont lues avec [`coverage_meta`].
#[wasm_bindgen]
pub fn coverage_finish(
    body: &str,
    polar: &[f32],
    itm_calls: f64,
    itm_errors: f64,
    elapsed_ms: f64,
) -> JsResult<Vec<f32>> {
    let run = || -> Result<Vec<f32>, AppError> {
        let cov = ST.with_borrow_mut(|s| s.cov.take());
        let cov = match cov {
            Some(c) if c.body == body => c,
            _ => return Err(AppError::internal("pas de plan pour cette couverture")),
        };
        let p = &cov.plan;
        if polar.len() != p.geom.n_radials * p.nk {
            return Err(AppError::internal("taille du tableau polaire incohérente"));
        }
        let mut grid = coverage::assemble(&cov.spec, p, polar);
        let mut stats = coverage::stats(&cov.spec, p, itm_calls as u64, itm_errors as u64);
        stats.elapsed_ms = elapsed_ms.max(0.0) as u128;

        let req = &cov.req;
        let f = cov.spec.params.f_mhz;
        let unit = match req.output {
            Quantity::PathLoss => "dB",
            Quantity::RxPower => {
                let (pw, g) = (req.budget.eirp_dbm.unwrap(), req.budget.rx_gain_dbi);
                grid.data
                    .iter_mut()
                    .for_each(|v| *v = rx_power_dbm(pw, g, *v as f64) as f32);
                "dBm"
            }
            Quantity::FieldStrength => {
                let pw = req.budget.eirp_dbm.unwrap();
                grid.data
                    .iter_mut()
                    .for_each(|v| *v = field_strength_dbuvm(pw, *v as f64, f) as f32);
                "dBuV/m"
            }
        };
        let meta = json!({
            "bounds": grid.bounds(),
            "unit": unit,
            "width": grid.width,
            "height": grid.height,
            "west": grid.west,
            "north": grid.north,
            "dlon": grid.dlon,
            "dlat": grid.dlat,
            "stats": stats,
        })
        .to_string();
        ST.with_borrow_mut(|s| s.meta = meta);
        Ok(grid.data)
    };
    run().map_err(js_err)
}

#[wasm_bindgen]
pub fn coverage_meta() -> String {
    ST.with_borrow(|s| s.meta.clone())
}

/// GeoTIFF float32 (EPSG:4326, Deflate, nodata −9999) d'une grille.
#[wasm_bindgen]
pub fn grid_geotiff(
    width: usize,
    height: usize,
    west: f64,
    north: f64,
    dlon: f64,
    dlat: f64,
    data: Vec<f32>,
) -> JsResult<Vec<u8>> {
    if data.len() != width * height {
        return Err(js_err(AppError::bad_request(
            "taille de grille incohérente",
        )));
    }
    let g = coverage::Grid {
        width,
        height,
        west,
        north,
        dlon,
        dlat,
        data,
    };
    raster_out::grid_geotiff(&g).map_err(|e| js_err(AppError::internal(format!("GeoTIFF : {e}"))))
}
