//! MNE Copernicus GLO-90 (tuiles COG de 1°x1°), décodé depuis des octets.
//!
//! Version navigateur du module `dem` de RustITM : le téléchargement et le cache sont faits
//! côté JavaScript (fetch + Cache API) ; ici on ne fait que décoder et interroger les tuiles.
//! Les tuiles absentes du jeu Copernicus (océan ouvert) sont traitées comme altitude 0 m.
//! Les altitudes sont des hauteurs orthométriques (géoïde EGM2008) du modèle de *surface*
//! (bâtiments et végétation inclus).

use std::collections::HashMap;
use std::io::Cursor;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

/// Identifiant d'une tuile : coin sud-ouest en degrés entiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub lat: i32,
    pub lon: i32,
}

impl TileKey {
    pub fn containing(lat: f64, lon: f64) -> Self {
        let mut lon_i = lon.floor() as i32;
        if lon_i >= 180 {
            lon_i -= 360;
        }
        if lon_i < -180 {
            lon_i += 360;
        }
        Self {
            lat: (lat.floor() as i32).clamp(-90, 89),
            lon: lon_i,
        }
    }

    /// Nom officiel Copernicus, ex. `Copernicus_DSM_COG_30_N45_00_E005_00_DEM`.
    pub fn name(&self) -> String {
        let ns = if self.lat >= 0 { 'N' } else { 'S' };
        let ew = if self.lon >= 0 { 'E' } else { 'W' };
        format!(
            "Copernicus_DSM_COG_30_{}{:02}_00_{}{:03}_00_DEM",
            ns,
            self.lat.abs(),
            ew,
            self.lon.abs()
        )
    }

    /// Nom de la tuile sur le miroir OpenTopography, ex. `Copernicus_DSM_30_N45_00_E005_00_DEM`.
    pub fn name_ot(&self) -> String {
        self.name().replace("_COG_", "_")
    }
}

/// Tuile décodée en mémoire.
#[derive(Debug)]
pub struct Tile {
    pub width: usize,
    pub height: usize,
    /// Longitude du centre du pixel (0,0)
    pub lon0: f64,
    /// Latitude du centre du pixel (0,0)
    pub lat0: f64,
    pub dlon: f64,
    pub dlat: f64,
    pub data: Vec<f32>,
}

impl Tile {
    #[inline]
    fn px(&self, row: usize, col: usize) -> f32 {
        self.data[row * self.width + col]
    }

    /// Plus proche voisin (utilisé aux bords pour aller chercher dans la tuile voisine).
    #[inline]
    fn nearest(&self, lat: f64, lon: f64) -> f32 {
        let c = ((lon - self.lon0) / self.dlon)
            .round()
            .clamp(0.0, (self.width - 1) as f64) as usize;
        let r = ((self.lat0 - lat) / self.dlat)
            .round()
            .clamp(0.0, (self.height - 1) as f64) as usize;
        self.px(r, c)
    }
}

fn clean(v: f32) -> f64 {
    // Copernicus n'a pas de nodata officiel ; on protège quand même contre les valeurs aberrantes.
    if v.is_finite() && v > -1000.0 && v < 9000.0 {
        v as f64
    } else {
        0.0
    }
}

/// Décode une tuile GeoTIFF (COG ou non) en mémoire.
pub fn read_geotiff_bytes(bytes: &[u8]) -> Result<Tile, String> {
    read_geotiff_inner(bytes).map_err(|e| format!("décodage GeoTIFF : {e}"))
}

fn read_geotiff_inner(bytes: &[u8]) -> Result<Tile, Box<dyn std::error::Error>> {
    let mut dec = Decoder::new(Cursor::new(bytes))?;
    let (w, h) = dec.dimensions()?;
    let scale = dec.get_tag_f64_vec(Tag::ModelPixelScaleTag)?;
    let tie = dec.get_tag_f64_vec(Tag::ModelTiepointTag)?;
    if scale.len() < 2 || tie.len() < 6 {
        return Err("géoréférencement GeoTIFF invalide".into());
    }
    let mut pixel_is_point = false;
    if let Ok(keys) = dec.get_tag_u16_vec(Tag::GeoKeyDirectoryTag) {
        for k in keys.chunks(4).skip(1) {
            if k.len() == 4 && k[0] == 1025 && k[1] == 0 {
                pixel_is_point = k[3] == 2;
            }
        }
    }
    let nodata: Option<f32> = dec
        .get_tag_ascii_string(Tag::GdalNodata)
        .ok()
        .and_then(|s| s.trim_matches(char::from(0)).trim().parse().ok());

    let (dlon, dlat) = (scale[0], scale[1]);
    let (i, j, x, y) = (tie[0], tie[1], tie[3], tie[4]);
    let mut lon0 = x - i * dlon;
    let mut lat0 = y + j * dlat;
    if !pixel_is_point {
        lon0 += dlon / 2.0;
        lat0 -= dlat / 2.0;
    }

    let mut data: Vec<f32> = match dec.read_image()? {
        DecodingResult::F32(v) => v,
        DecodingResult::F64(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::I16(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::U16(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::I32(v) => v.into_iter().map(|x| x as f32).collect(),
        _ => return Err("type de pixel non supporté".into()),
    };
    if data.len() != (w as usize) * (h as usize) {
        return Err("taille de données inattendue (bandes multiples ?)".into());
    }
    if let Some(nd) = nodata {
        for v in data.iter_mut() {
            if *v == nd {
                *v = 0.0;
            }
        }
    }
    Ok(Tile {
        width: w as usize,
        height: h as usize,
        lon0,
        lat0,
        dlon,
        dlat,
        data,
    })
}

pub fn keys_for_bbox(south: f64, west: f64, north: f64, east: f64) -> Vec<TileKey> {
    let mut keys = Vec::new();
    let lat_a = south.floor() as i32;
    let lat_b = north.floor() as i32;
    let lon_a = west.floor() as i32;
    let lon_b = east.floor() as i32;
    for lat in lat_a.max(-90)..=lat_b.min(89) {
        for lon in lon_a..=lon_b {
            let k = TileKey::containing(lat as f64 + 0.5, lon as f64 + 0.5);
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys
}

/// Ensemble de tuiles chargées. `None` = tuile inexistante (mer).
#[derive(Default)]
pub struct TileSet {
    tiles: HashMap<TileKey, Option<Tile>>,
}

impl TileSet {
    pub fn insert(&mut self, key: TileKey, tile: Option<Tile>) {
        self.tiles.insert(key, tile);
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains_key(&key)
    }

    pub fn remove(&mut self, key: TileKey) {
        self.tiles.remove(&key);
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    fn nearest_global(&self, lat: f64, lon: f64) -> f64 {
        match self.tiles.get(&TileKey::containing(lat, lon)) {
            Some(Some(t)) => clean(t.nearest(lat, lon)),
            _ => 0.0,
        }
    }

    /// Altitude (m) par interpolation bilinéaire ; 0 m pour la mer ou hors emprise chargée.
    pub fn elevation(&self, lat: f64, lon: f64) -> f64 {
        let lon = normalize_lon(lon);
        let t = match self.tiles.get(&TileKey::containing(lat, lon)) {
            Some(Some(t)) => t,
            _ => return 0.0,
        };
        let fc = (lon - t.lon0) / t.dlon;
        let fr = (t.lat0 - lat) / t.dlat;
        let c0 = fc.floor();
        let r0 = fr.floor();
        let (wx, wy) = (fc - c0, fr - r0);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let get = |r: i64, c: i64| -> f64 {
            if r >= 0 && c >= 0 && (r as usize) < t.height && (c as usize) < t.width {
                clean(t.px(r as usize, c as usize))
            } else {
                // Pixel dans une tuile voisine
                let la = t.lat0 - r as f64 * t.dlat;
                let lo = normalize_lon(t.lon0 + c as f64 * t.dlon);
                self.nearest_global(la, lo)
            }
        };
        let v00 = get(r0, c0);
        let v01 = get(r0, c0 + 1);
        let v10 = get(r0 + 1, c0);
        let v11 = get(r0 + 1, c0 + 1);
        let top = v00 * (1.0 - wx) + v01 * wx;
        let bot = v10 * (1.0 - wx) + v11 * wx;
        top * (1.0 - wy) + bot * wy
    }
}

pub fn normalize_lon(lon: f64) -> f64 {
    let mut l = lon;
    while l >= 180.0 {
        l -= 360.0;
    }
    while l < -180.0 {
        l += 360.0;
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_names() {
        assert_eq!(
            TileKey::containing(45.5, 5.2).name(),
            "Copernicus_DSM_COG_30_N45_00_E005_00_DEM"
        );
        assert_eq!(
            TileKey::containing(-0.5, -0.5).name(),
            "Copernicus_DSM_COG_30_S01_00_W001_00_DEM"
        );
        assert_eq!(
            TileKey::containing(-0.5, -0.5).name_ot(),
            "Copernicus_DSM_30_S01_00_W001_00_DEM"
        );
        assert_eq!(
            TileKey::containing(48.85, 2.35).name(),
            "Copernicus_DSM_COG_30_N48_00_E002_00_DEM"
        );
        assert_eq!(
            TileKey::containing(-33.9, 151.2).name(),
            "Copernicus_DSM_COG_30_S34_00_E151_00_DEM"
        );
    }

    #[test]
    fn bbox_keys() {
        let k = keys_for_bbox(45.2, 5.8, 46.1, 6.3);
        assert_eq!(k.len(), 4);
    }
}
