//! Écriture des résultats raster : GeoTIFF float32 (EPSG:4326).

use std::io::Cursor;

use tiff::encoder::compression::DeflateLevel;
use tiff::encoder::{colortype, Compression, TiffEncoder};
use tiff::tags::Tag;

type Result<T> = std::result::Result<T, tiff::TiffError>;

use crate::coverage::Grid;

pub const NODATA: f32 = -9999.0;

/// Géoréférencement d'un raster lat/lon.
pub struct GeoRef {
    /// Coordonnées du coin nord-ouest (PixelIsArea) ou du centre du pixel (0,0) (PixelIsPoint)
    pub west: f64,
    pub north: f64,
    pub dlon: f64,
    pub dlat: f64,
    pub pixel_is_point: bool,
}

/// Encode un raster float32 en GeoTIFF (compression Deflate) avec les GeoKeys EPSG:4326.
pub fn geotiff_f32(
    width: u32,
    height: u32,
    data: &[f32],
    geo: &GeoRef,
    nodata: Option<f32>,
) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut enc = TiffEncoder::new(&mut buf)?
            .with_compression(Compression::Deflate(DeflateLevel::Balanced));
        let mut img = enc.new_image::<colortype::Gray32Float>(width, height)?;
        let e = img.encoder();
        e.write_tag(Tag::ModelPixelScaleTag, &[geo.dlon, geo.dlat, 0.0][..])?;
        e.write_tag(
            Tag::ModelTiepointTag,
            &[0.0, 0.0, 0.0, geo.west, geo.north, 0.0][..],
        )?;
        let raster_type: u16 = if geo.pixel_is_point { 2 } else { 1 };
        let keys: [u16; 16] = [
            1,
            1,
            0,
            3, // en-tête : version 1.1.0, 3 clés
            1024,
            0,
            1,
            2, // GTModelTypeGeoKey = Geographic
            1025,
            0,
            1,
            raster_type, // GTRasterTypeGeoKey
            2048,
            0,
            1,
            4326, // GeographicTypeGeoKey = WGS 84
        ];
        e.write_tag(Tag::GeoKeyDirectoryTag, &keys[..])?;
        if let Some(nd) = nodata {
            e.write_tag(Tag::GdalNodata, format!("{nd}").as_str())?;
        }
        img.write_data(data)?;
    }
    Ok(buf.into_inner())
}

pub fn grid_geotiff(g: &Grid) -> Result<Vec<u8>> {
    let data: Vec<f32> = g
        .data
        .iter()
        .map(|v| if v.is_finite() { *v } else { NODATA })
        .collect();
    let geo = GeoRef {
        west: g.west,
        north: g.north,
        dlon: g.dlon,
        dlat: g.dlat,
        pixel_is_point: false,
    };
    geotiff_f32(g.width as u32, g.height as u32, &data, &geo, Some(NODATA))
}
