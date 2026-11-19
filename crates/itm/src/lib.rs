//! Portage en Rust pur du modèle ITM (Irregular Terrain Model / Longley-Rice), mode point à point.
//!
//! Dérivé de l'implémentation de référence NTIA/ITS v1.3 (https://github.com/NTIA/itm),
//! logiciel du gouvernement américain non soumis au droit d'auteur aux États-Unis.
//! Modifications : traduction C++ -> Rust (2026), API idiomatique, suppression du mode zone.
//!
//! Le calcul est fidèle à la référence : les 5 cas de test officiels NTIA sont reproduits
//! (voir `tests/ntia.rs`).

mod complex;
/// Fonctions internes du modèle, exposées pour les micro-benchmarks (`examples/split.rs`).
/// Pas une API stable.
#[doc(hidden)]
pub mod core;
mod fast;
mod variability;

use complex::Complex;

pub use crate::core::{DeltaHHint, PathGeometry, PropMode};
pub use crate::fast::{FastRadial, FAST_DEFAULT_STRIDE};

/// Climat radio (TN101).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Climate {
    Equatorial = 1,
    ContinentalSubtropical = 2,
    MaritimeSubtropical = 3,
    Desert = 4,
    ContinentalTemperate = 5,
    MaritimeTemperateOverLand = 6,
    MaritimeTemperateOverSea = 7,
}

impl Climate {
    pub fn from_i32(v: i32) -> Option<Self> {
        use Climate::*;
        Some(match v {
            1 => Equatorial,
            2 => ContinentalSubtropical,
            3 => MaritimeSubtropical,
            4 => Desert,
            5 => ContinentalTemperate,
            6 => MaritimeTemperateOverLand,
            7 => MaritimeTemperateOverSea,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarization {
    Horizontal = 0,
    Vertical = 1,
}

/// Paramètres d'un calcul point à point (variabilité temps/lieux/situations).
#[derive(Debug, Clone, Copy)]
pub struct P2pParams {
    /// Hauteur de l'antenne émettrice au-dessus du sol (m), 0.5..3000
    pub h_tx_m: f64,
    /// Hauteur de l'antenne réceptrice au-dessus du sol (m), 0.5..3000
    pub h_rx_m: f64,
    pub climate: Climate,
    /// Réfractivité de surface au niveau de la mer N0 (N-units), 250..400
    pub n0: f64,
    /// Fréquence (MHz), 20..20000
    pub f_mhz: f64,
    pub polarization: Polarization,
    /// Permittivité relative du sol
    pub epsilon: f64,
    /// Conductivité du sol (S/m)
    pub sigma: f64,
    /// Mode de variabilité : 0 message unique, 1 accidentel, 2 mobile, 3 diffusion ;
    /// +10 : variabilité de lieu supprimée ; +20 : variabilité de situation supprimée.
    pub mdvar: i32,
    /// Pourcentage de temps, 0 < time < 100
    pub time: f64,
    /// Pourcentage de lieux, 0 < location < 100
    pub location: f64,
    /// Pourcentage de situations, 0 < situation < 100
    pub situation: f64,
}

impl Default for P2pParams {
    fn default() -> Self {
        Self {
            h_tx_m: 10.0,
            h_rx_m: 2.0,
            climate: Climate::ContinentalTemperate,
            n0: 301.0,
            f_mhz: 100.0,
            polarization: Polarization::Vertical,
            epsilon: 15.0,
            sigma: 0.005,
            mdvar: 12,
            time: 50.0,
            location: 50.0,
            situation: 50.0,
        }
    }
}

/// Erreurs ITM (mêmes codes que la référence NTIA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItmError {
    TxTerminalHeight = 1000,
    RxTerminalHeight = 1001,
    InvalidRadioClimate = 1002,
    InvalidTime = 1003,
    InvalidLocation = 1004,
    InvalidSituation = 1005,
    Refractivity = 1008,
    Frequency = 1009,
    Polarization = 1010,
    Epsilon = 1011,
    Sigma = 1012,
    GroundImpedance = 1013,
    Mdvar = 1014,
    EffectiveEarth = 1016,
    PathDistance = 1017,
    SurfaceRefractivitySmall = 1021,
    SurfaceRefractivityLarge = 1022,
}

impl ItmError {
    pub fn code(self) -> i32 {
        self as i32
    }
    pub fn message(self) -> &'static str {
        use ItmError::*;
        match self {
            TxTerminalHeight => "TX height out of range (0.5..3000 m)",
            RxTerminalHeight => "RX height out of range (0.5..3000 m)",
            InvalidRadioClimate => "invalid radio climate (1..7)",
            InvalidTime => "time percentage out of range (0..100, exclusive)",
            InvalidLocation => "location percentage out of range (0..100, exclusive)",
            InvalidSituation => "situation percentage out of range (0..100, exclusive)",
            Refractivity => "refractivity N0 out of range (250..400)",
            Frequency => "frequency out of range (20..20000 MHz)",
            Polarization => "invalid polarization",
            Epsilon => "relative permittivity < 1",
            Sigma => "conductivity <= 0",
            GroundImpedance => "invalid ground impedance",
            Mdvar => "invalid variability mode",
            EffectiveEarth => "invalid effective earth radius",
            PathDistance => "invalid terrain profile (at least 2 points and distance > 0)",
            SurfaceRefractivitySmall => "computed surface refractivity too small",
            SurfaceRefractivityLarge => "computed surface refractivity too large",
        }
    }
}

impl std::error::Error for ItmError {}

impl std::fmt::Display for ItmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ITM erreur {}: {}", self.code(), self.message())
    }
}

/// Drapeaux d'avertissement (mêmes bits que la référence NTIA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Warnings(pub u32);

impl Warnings {
    pub const TX_TERMINAL_HEIGHT: u32 = 0x0001;
    pub const RX_TERMINAL_HEIGHT: u32 = 0x0002;
    pub const FREQUENCY: u32 = 0x0004;
    pub const PATH_DISTANCE_TOO_BIG_1: u32 = 0x0008;
    pub const PATH_DISTANCE_TOO_BIG_2: u32 = 0x0010;
    pub const PATH_DISTANCE_TOO_SMALL_1: u32 = 0x0020;
    pub const PATH_DISTANCE_TOO_SMALL_2: u32 = 0x0040;
    pub const TX_HORIZON_ANGLE: u32 = 0x0080;
    pub const RX_HORIZON_ANGLE: u32 = 0x0100;
    pub const TX_HORIZON_DISTANCE_1: u32 = 0x0200;
    pub const RX_HORIZON_DISTANCE_1: u32 = 0x0400;
    pub const TX_HORIZON_DISTANCE_2: u32 = 0x0800;
    pub const RX_HORIZON_DISTANCE_2: u32 = 0x1000;
    pub const EXTREME_VARIABILITIES: u32 = 0x2000;
    pub const SURFACE_REFRACTIVITY: u32 = 0x4000;

    pub(crate) fn set(&mut self, flag: u32) {
        self.0 |= flag;
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Libellés lisibles des avertissements actifs.
    pub fn descriptions(&self) -> Vec<&'static str> {
        const ALL: [(u32, &str); 15] = [
            (
                Warnings::TX_TERMINAL_HEIGHT,
                "TX height close to the limits",
            ),
            (
                Warnings::RX_TERMINAL_HEIGHT,
                "RX height close to the limits",
            ),
            (
                Warnings::FREQUENCY,
                "frequency close to the limits (40..10000 MHz)",
            ),
            (Warnings::PATH_DISTANCE_TOO_BIG_1, "distance > 1000 km"),
            (
                Warnings::PATH_DISTANCE_TOO_BIG_2,
                "distance > 2000 km, unreliable result",
            ),
            (
                Warnings::PATH_DISTANCE_TOO_SMALL_1,
                "distance close to the lower limit",
            ),
            (
                Warnings::PATH_DISTANCE_TOO_SMALL_2,
                "distance < 1 km, unreliable result",
            ),
            (
                Warnings::TX_HORIZON_ANGLE,
                "high TX horizon angle (> 200 mrad)",
            ),
            (
                Warnings::RX_HORIZON_ANGLE,
                "high RX horizon angle (> 200 mrad)",
            ),
            (
                Warnings::TX_HORIZON_DISTANCE_1,
                "TX horizon < 1/10 of the smooth-earth horizon",
            ),
            (
                Warnings::RX_HORIZON_DISTANCE_1,
                "RX horizon < 1/10 of the smooth-earth horizon",
            ),
            (
                Warnings::TX_HORIZON_DISTANCE_2,
                "TX horizon > 3x the smooth-earth horizon",
            ),
            (
                Warnings::RX_HORIZON_DISTANCE_2,
                "RX horizon > 3x the smooth-earth horizon",
            ),
            (
                Warnings::EXTREME_VARIABILITIES,
                "extreme variabilities requested",
            ),
            (Warnings::SURFACE_REFRACTIVITY, "low surface refractivity"),
        ];
        ALL.iter()
            .filter(|(b, _)| self.0 & b != 0)
            .map(|(_, s)| *s)
            .collect()
    }
}

/// Valeurs intermédiaires du calcul.
#[derive(Debug, Clone, Copy, Default)]
pub struct Intermediate {
    pub d_km: f64,
    pub a_ref_db: f64,
    pub a_fs_db: f64,
    pub delta_h_m: f64,
    pub d_hzn_m: [f64; 2],
    pub h_e_m: [f64; 2],
    pub n_s: f64,
    pub theta_hzn: [f64; 2],
    pub mode: PropMode,
    /// Horizons radio trouvés sur le profil réel (m)
    pub terrain_d_hzn_m: [f64; 2],
    /// Vue directe / un horizon / deux horizons (sur le profil réel)
    pub path_geometry: PathGeometry,
}

#[derive(Debug, Clone, Copy)]
pub struct P2pResult {
    /// Affaiblissement de transmission de base (dB)
    pub loss_db: f64,
    pub warnings: Warnings,
    pub intermediate: Intermediate,
}

/// Valide les paramètres (équivalent de `ValidateInputs`).
pub fn validate(p: &P2pParams, w: &mut Warnings) -> Result<(), ItmError> {
    if p.h_tx_m < 1.0 || p.h_tx_m > 1000.0 {
        w.set(Warnings::TX_TERMINAL_HEIGHT);
    }
    if !(0.5..=3000.0).contains(&p.h_tx_m) {
        return Err(ItmError::TxTerminalHeight);
    }
    if p.h_rx_m < 1.0 || p.h_rx_m > 1000.0 {
        w.set(Warnings::RX_TERMINAL_HEIGHT);
    }
    if !(0.5..=3000.0).contains(&p.h_rx_m) {
        return Err(ItmError::RxTerminalHeight);
    }
    if !(250.0..=400.0).contains(&p.n0) {
        return Err(ItmError::Refractivity);
    }
    if p.f_mhz < 40.0 || p.f_mhz > 10000.0 {
        w.set(Warnings::FREQUENCY);
    }
    if !(20.0..=20000.0).contains(&p.f_mhz) {
        return Err(ItmError::Frequency);
    }
    if p.epsilon < 1.0 || p.epsilon.is_nan() {
        return Err(ItmError::Epsilon);
    }
    if p.sigma <= 0.0 || p.sigma.is_nan() {
        return Err(ItmError::Sigma);
    }
    let m = p.mdvar;
    if m < 0 || (m > 3 && m < 10) || (m > 13 && m < 20) || (m > 23 && m < 30) || m > 33 {
        return Err(ItmError::Mdvar);
    }
    let open = |v: f64| v > 0.0 && v < 100.0;
    if !open(p.situation) {
        return Err(ItmError::InvalidSituation);
    }
    if !open(p.time) {
        return Err(ItmError::InvalidTime);
    }
    if !open(p.location) {
        return Err(ItmError::InvalidLocation);
    }
    Ok(())
}

/// Calcul ITM point à point (équivalent de `ITM_P2P_TLS_Ex`).
///
/// `elevations` : altitudes du terrain (m) régulièrement espacées de `spacing_m`, de l'émetteur
/// (index 0) au récepteur (dernier index). Au moins 2 points.
pub fn p2p(p: &P2pParams, elevations: &[f64], spacing_m: f64) -> Result<P2pResult, ItmError> {
    p2p_impl(p, elevations, spacing_m, None)
}

/// Comme [`p2p`], pour une suite d'appels le long d'une même radiale (distances croissantes) :
/// `hint` garde d'un appel à l'autre de quoi accélérer le calcul de Δh. Le résultat est
/// identique à [`p2p`]. Utiliser un indice par radiale (par thread).
pub fn p2p_hinted(
    p: &P2pParams,
    elevations: &[f64],
    spacing_m: f64,
    hint: &mut DeltaHHint,
) -> Result<P2pResult, ItmError> {
    p2p_impl(p, elevations, spacing_m, Some(hint))
}

fn p2p_impl(
    p: &P2pParams,
    elevations: &[f64],
    spacing_m: f64,
    hint: Option<&mut DeltaHHint>,
) -> Result<P2pResult, ItmError> {
    let mut w = Warnings::default();
    validate(p, &mut w)?;
    if elevations.len() < 2 || spacing_m.is_nan() || spacing_m <= 0.0 {
        return Err(ItmError::PathDistance);
    }
    let np = elevations.len() - 1;

    // Hauteur moyenne du trajet, en ignorant 10 % à chaque extrémité.
    let p10 = (0.1 * np as f64) as usize;
    let h_sys = elevations[p10..=(np - p10)].iter().sum::<f64>() / (np - 2 * p10 + 1) as f64;

    let (gamma_e, n_s) = refractivity(p.n0, h_sys);
    let z_g = ground_impedance(p);
    let h = [p.h_tx_m, p.h_rx_m];
    let qp = core::quick_pfl_hinted(elevations, spacing_m, gamma_e, h, hint);
    finish(p, w, z_g, gamma_e, n_s, &qp, spacing_m)
}

/// Gradient de courbure effectif γe et réfractivité de surface Ns (`InitializePointToPoint`).
#[inline]
pub(crate) fn refractivity(n0: f64, h_sys: f64) -> (f64, f64) {
    let gamma_a = 157e-9;
    let n_s = if h_sys == 0.0 {
        n0
    } else {
        n0 * (-h_sys / 9460.0).exp()
    };
    (gamma_a * (1.0 - 0.04665 * (n_s / 179.3).exp()), n_s)
}

/// Impédance de surface du sol (`InitializePointToPoint`).
pub(crate) fn ground_impedance(p: &P2pParams) -> Complex {
    let ep_r = Complex::new(p.epsilon, 18000.0 * p.sigma / p.f_mhz);
    let z_g = (ep_r - 1.0).sqrt();
    if p.polarization == Polarization::Vertical {
        z_g / ep_r
    } else {
        z_g
    }
}

/// Affaiblissement de référence, variabilité et résultat, une fois le profil analysé.
#[inline]
pub(crate) fn finish(
    p: &P2pParams,
    mut w: Warnings,
    z_g: Complex,
    gamma_e: f64,
    n_s: f64,
    qp: &core::QuickPflOut,
    spacing_m: f64,
) -> Result<P2pResult, ItmError> {
    let h = [p.h_tx_m, p.h_rx_m];
    let (a_ref, mode) = core::longley_rice(
        qp.theta_hzn,
        p.f_mhz,
        z_g,
        qp.d_hzn_m,
        qp.h_e_m,
        gamma_e,
        n_s,
        qp.delta_h_m,
        h,
        qp.d_m,
        &mut w,
    )?;

    let a_fs = core::free_space_loss(qp.d_m, p.f_mhz);
    let var = variability::variability(
        p.time,
        p.location,
        p.situation,
        qp.h_e_m,
        qp.delta_h_m,
        p.f_mhz,
        qp.d_m,
        a_ref,
        p.climate as i32,
        p.mdvar,
        &mut w,
    );

    Ok(P2pResult {
        loss_db: var + a_fs,
        warnings: w,
        intermediate: Intermediate {
            d_km: qp.d_m / 1000.0,
            a_ref_db: a_ref,
            a_fs_db: a_fs,
            delta_h_m: qp.delta_h_m,
            d_hzn_m: qp.d_hzn_m,
            h_e_m: qp.h_e_m,
            n_s,
            theta_hzn: qp.theta_hzn,
            mode,
            terrain_d_hzn_m: qp.terrain_d_hzn_m,
            path_geometry: PathGeometry::classify(qp.terrain_d_hzn_m, qp.d_m, spacing_m),
        },
    })
}

/// Affaiblissement en espace libre (dB).
pub fn free_space_loss(d_m: f64, f_mhz: f64) -> f64 {
    core::free_space_loss(d_m, f_mhz)
}
