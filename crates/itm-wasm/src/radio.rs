//! Paramètres radio communs aux requêtes et conversions de grandeurs.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClimateName {
    Equatorial,
    ContinentalSubtropical,
    MaritimeSubtropical,
    Desert,
    ContinentalTemperate,
    MaritimeTemperateOverLand,
    MaritimeTemperateOverSea,
}

/// Climat donné par son nom ou son code ITM (1..7).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ClimateInput {
    Code(i32),
    Name(ClimateName),
}

impl ClimateInput {
    fn to_itm(self) -> Option<itm::Climate> {
        use itm::Climate as C;
        match self {
            ClimateInput::Code(c) => C::from_i32(c),
            ClimateInput::Name(n) => Some(match n {
                ClimateName::Equatorial => C::Equatorial,
                ClimateName::ContinentalSubtropical => C::ContinentalSubtropical,
                ClimateName::MaritimeSubtropical => C::MaritimeSubtropical,
                ClimateName::Desert => C::Desert,
                ClimateName::ContinentalTemperate => C::ContinentalTemperate,
                ClimateName::MaritimeTemperateOverLand => C::MaritimeTemperateOverLand,
                ClimateName::MaritimeTemperateOverSea => C::MaritimeTemperateOverSea,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolarizationInput {
    Horizontal,
    Vertical,
}

fn d_climate() -> ClimateInput {
    ClimateInput::Name(ClimateName::ContinentalTemperate)
}
fn d_pol() -> PolarizationInput {
    PolarizationInput::Vertical
}
fn d_n0() -> f64 {
    301.0
}
fn d_eps() -> f64 {
    15.0
}
fn d_sigma() -> f64 {
    0.005
}
fn d_mdvar() -> i32 {
    12
}
fn d_50() -> f64 {
    50.0
}

/// Paramètres ITM (hors géométrie). Valeurs par défaut : sol moyen, climat continental tempéré.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ItmInput {
    pub frequency_mhz: f64,
    #[serde(default = "d_pol")]
    pub polarization: PolarizationInput,
    #[serde(default = "d_climate")]
    pub climate: ClimateInput,
    /// Réfractivité de surface N0 (N-units)
    #[serde(default = "d_n0")]
    pub n0: f64,
    /// Permittivité relative du sol
    #[serde(default = "d_eps")]
    pub epsilon: f64,
    /// Conductivité du sol (S/m)
    #[serde(default = "d_sigma")]
    pub sigma: f64,
    #[serde(default = "d_mdvar")]
    pub mdvar: i32,
    #[serde(default = "d_50")]
    pub time: f64,
    #[serde(default = "d_50")]
    pub location: f64,
    #[serde(default = "d_50")]
    pub situation: f64,
}

impl ItmInput {
    pub fn to_params(&self, h_tx: f64, h_rx: f64) -> Result<itm::P2pParams, AppError> {
        let climate = self
            .climate
            .to_itm()
            .ok_or_else(|| AppError::bad_request("climat invalide (1..7 ou nom)"))?;
        let p = itm::P2pParams {
            h_tx_m: h_tx,
            h_rx_m: h_rx,
            climate,
            n0: self.n0,
            f_mhz: self.frequency_mhz,
            polarization: match self.polarization {
                PolarizationInput::Horizontal => itm::Polarization::Horizontal,
                PolarizationInput::Vertical => itm::Polarization::Vertical,
            },
            epsilon: self.epsilon,
            sigma: self.sigma,
            mdvar: self.mdvar,
            time: self.time,
            location: self.location,
            situation: self.situation,
        };
        // Validation anticipée pour renvoyer une erreur 400 claire avant tout calcul.
        let mut w = itm::Warnings::default();
        itm::validate(&p, &mut w).map_err(AppError::from)?;
        Ok(p)
    }
}

/// Bilan de liaison optionnel.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct LinkBudget {
    /// PIRE de l'émetteur (dBm)
    pub eirp_dbm: Option<f64>,
    /// Gain de l'antenne de réception (dBi), 0 par défaut
    #[serde(default)]
    pub rx_gain_dbi: f64,
}

/// Puissance reçue (dBm).
pub fn rx_power_dbm(eirp_dbm: f64, rx_gain_dbi: f64, loss_db: f64) -> f64 {
    eirp_dbm + rx_gain_dbi - loss_db
}

/// Champ électrique (dBµV/m) à partir de la PIRE et de l'affaiblissement de base.
/// E = PIRE(dBm) − Lb + 20·log10(f MHz) + 77.2  (UIT-R P.525)
pub fn field_strength_dbuvm(eirp_dbm: f64, loss_db: f64, f_mhz: f64) -> f64 {
    eirp_dbm - loss_db + 20.0 * f_mhz.log10() + 77.2
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_strength_1kw_1km_free_space() {
        // 1 kW PIRE à 1 km en espace libre : E = sqrt(30 P)/d = 104.77 dBµV/m
        let lb = itm::free_space_loss(1000.0, 100.0);
        let e = field_strength_dbuvm(60.0, lb, 100.0);
        assert!((e - 104.77).abs() < 0.05, "{e}");
    }
}
