//! Cœur ITM : traitement du profil et affaiblissement de référence.
//! Chaque fonction correspond au fichier du même nom dans la référence NTIA v1.3.
//! Convention : `z[i]` correspond à `pfl[i + 2]` dans le code C++.

use crate::complex::Complex;
use crate::{ItmError, Warnings};

const A_0_M: f64 = 6370e3;
const THIRD: f64 = 1.0 / 3.0;
const PI: f64 = std::f64::consts::PI;
const SQRT2: f64 = std::f64::consts::SQRT_2;

/// Mode de propagation dominant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PropMode {
    #[default]
    NotSet = 0,
    LineOfSight = 1,
    Diffraction = 2,
    Troposcatter = 3,
}

impl PropMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PropMode::NotSet => "not_set",
            PropMode::LineOfSight => "line_of_sight",
            PropMode::Diffraction => "diffraction",
            PropMode::Troposcatter => "troposcatter",
        }
    }
}

/// Géométrie du trajet vue depuis le profil réel (terre effective).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathGeometry {
    #[default]
    LineOfSight,
    SingleHorizon,
    DoubleHorizon,
}

impl PathGeometry {
    pub fn as_str(self) -> &'static str {
        match self {
            PathGeometry::LineOfSight => "line_of_sight",
            PathGeometry::SingleHorizon => "single_horizon",
            PathGeometry::DoubleHorizon => "double_horizon",
        }
    }

    pub fn classify(terrain_d_hzn: [f64; 2], d: f64, xi: f64) -> Self {
        if terrain_d_hzn[0] >= d - 1e-6 {
            PathGeometry::LineOfSight
        } else if (terrain_d_hzn[0] + terrain_d_hzn[1] - d).abs() < 0.5 * xi {
            PathGeometry::SingleHorizon
        } else {
            PathGeometry::DoubleHorizon
        }
    }
}

#[inline]
pub(crate) fn fdim(x: f64, y: f64) -> f64 {
    if x > y {
        x - y
    } else {
        0.0
    }
}

pub fn free_space_loss(d_m: f64, f_mhz: f64) -> f64 {
    32.45 + 20.0 * f_mhz.log10() + 20.0 * (d_m / 1000.0).log10()
}

pub struct QuickPflOut {
    pub theta_hzn: [f64; 2],
    pub d_hzn_m: [f64; 2],
    pub h_e_m: [f64; 2],
    pub delta_h_m: f64,
    pub d_m: f64,
    /// Distances d'horizon brutes issues du profil (avant ajustements de QuickPfl)
    pub terrain_d_hzn_m: [f64; 2],
}

/// Régression linéaire sur le profil entre `d_start` et `d_end` (`LinearLeastSquaresFit`).
pub fn linear_least_squares_fit(z: &[f64], xi: f64, d_start: f64, d_end: f64) -> (f64, f64) {
    let np = (z.len() - 1) as i64;
    let mut i_start = fdim(d_start / xi, 0.0) as i64;
    let mut i_end = np - fdim(np as f64, d_end / xi) as i64;
    if i_end <= i_start {
        i_start = fdim(i_start as f64, 1.0) as i64;
        i_end = np - fdim(np as f64, (i_end + 1) as f64) as i64;
    }
    let x_length = (i_end - i_start) as f64;
    let mut mid = -0.5 * x_length;
    let mid_end = i_end as f64 + mid;
    let mut sum_y = 0.5 * (z[i_start as usize] + z[i_end as usize]);
    let mut scaled = 0.5 * (z[i_start as usize] - z[i_end as usize]) * mid;
    let mut i = 2.0;
    while i <= x_length {
        i_start += 1;
        mid += 1.0;
        let v = z[i_start as usize];
        sum_y += v;
        scaled += v * mid;
        i += 1.0;
    }
    sum_y /= x_length;
    scaled = scaled * 12.0 / ((x_length * x_length + 2.0) * x_length);
    (
        sum_y - scaled * mid_end,
        sum_y + scaled * (np as f64 - mid_end),
    )
}

/// Paramètre d'irrégularité du terrain Δh (`ComputeDeltaH`).
pub fn compute_delta_h(z: &[f64], xi: f64, d_start: f64, d_end: f64) -> f64 {
    compute_delta_h_hinted(z, xi, d_start, d_end, None)
}

/// Indice de sélection des quantiles de Δh, réutilisé d'un appel au suivant le long d'une
/// même radiale (distances croissantes, comme dans une carte de couverture).
///
/// Les quantiles 10 % et 90 % du profil redressé varient peu d'un point au suivant : la
/// sélection exacte se fait alors parmi les quelques valeurs proches des quantiles
/// précédents, au lieu de deux sélections complètes. Le résultat est toujours identique au
/// calcul sans indice (profils sans NaN) ; seule la vitesse en dépend. Un indice périmé (autre
/// radiale) reste correct, il est juste moins efficace au premier appel.
#[derive(Debug, Clone)]
pub struct DeltaHHint {
    prev: Option<(f64, f64)>,
    /// Demi-largeur des bandes, relative à l'écart entre les deux quantiles
    width: f64,
}

impl Default for DeltaHHint {
    fn default() -> Self {
        Self {
            prev: None,
            width: 0.02,
        }
    }
}

/// Taille maximale d'une bande pour le chemin rapide.
const BAND_MAX: usize = 48;

/// Comme [`compute_delta_h`], avec un indice facultatif pour accélérer la sélection.
pub fn compute_delta_h_hinted(
    z: &[f64],
    xi: f64,
    d_start: f64,
    d_end: f64,
    hint: Option<&mut DeltaHHint>,
) -> f64 {
    let np = (z.len() - 1) as i64;
    let mut x_start = d_start / xi;
    let mut x_end = d_end / xi;
    if x_end - x_start < 2.0 {
        return 0.0;
    }
    let p10 = ((0.1 * (x_end - x_start + 8.0)) as i64).clamp(4, 25);
    let n = 10 * p10 - 5;
    let p90 = n - p10;
    let np_s = (n - 1) as f64;
    x_end = (x_end - x_start) / np_s;
    let mut i = x_start as i64;
    x_start -= (i + 1) as f64;
    // n <= 245 : tableaux sur la pile, pas d'allocation (appelé des millions de fois en couverture).
    let mut s = [0.0f64; 245];
    let n_us = n as usize;
    for v in s.iter_mut().take(n_us) {
        while x_start > 0.0 && (i + 1) < np {
            x_start -= 1.0;
            i += 1;
        }
        let a = z[(i + 1) as usize];
        let b = z[i as usize];
        *v = a + (a - b) * x_start;
        x_start += x_end;
    }
    let s = &mut s[..n_us];
    let (mut fit_y1, fit_y2) = linear_least_squares_fit(s, 1.0, 0.0, np_s);
    let step = (fit_y2 - fit_y1) / np_s;
    for v in s.iter_mut() {
        *v -= fit_y1;
        fit_y1 += step;
    }
    let (ra, rb) = ((p10 - 1) as usize, p90 as usize);
    let (q10, q90) = match hint {
        Some(h) => two_quantiles_hinted(s, ra, rb, h),
        None => two_quantiles_desc(s, ra, rb),
    };
    (q10 - q90) / (1.0 - 0.8 * (-(d_end - d_start) / 50e3).exp())
}

fn desc(a: &f64, b: &f64) -> std::cmp::Ordering {
    b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
}

/// Valeurs de rangs `ra` et `rb` dans l'ordre décroissant : équivalent de deux
/// `std::nth_element(..., std::greater)`, même résultat que la référence.
fn two_quantiles_desc(s: &mut [f64], ra: usize, rb: usize) -> (f64, f64) {
    let qa = *s.select_nth_unstable_by(ra, desc).1;
    let qb = *s.select_nth_unstable_by(rb, desc).1;
    (qa, qb)
}

/// Comme [`two_quantiles_desc`], en cherchant d'abord chaque valeur dans une bande autour du
/// quantile précédent. Si `above` valeurs sont au-dessus de la bande et que le rang cherché
/// tombe dans la bande, la valeur de rang r est celle de rang r - above parmi les valeurs de
/// la bande : le résultat est exact quelle que soit la largeur de bande.
fn two_quantiles_hinted(s: &mut [f64], ra: usize, rb: usize, hint: &mut DeltaHHint) -> (f64, f64) {
    if let Some((pa, pb)) = hint.prev {
        let w = hint.width * (pa - pb).abs() + 1e-6;
        let (lo_a, hi_a, lo_b, hi_b) = (pa - w, pa + w, pb - w, pb + w);
        // Un seul passage : comptage au-dessus / dans chaque bande, et compaction sans branche
        // des valeurs de bande (au-delà de BAND_MAX, le dernier emplacement est réécrit ; le
        // chemin rapide est alors abandonné).
        let mut band_a = [0.0f64; BAND_MAX + 1];
        let mut band_b = [0.0f64; BAND_MAX + 1];
        let (mut above_a, mut in_a, mut above_b, mut in_b) = (0usize, 0usize, 0usize, 0usize);
        for &v in s.iter() {
            above_a += (v > hi_a) as usize;
            band_a[in_a.min(BAND_MAX)] = v;
            in_a += (v >= lo_a && v <= hi_a) as usize;
            above_b += (v > hi_b) as usize;
            band_b[in_b.min(BAND_MAX)] = v;
            in_b += (v >= lo_b && v <= hi_b) as usize;
        }
        // Rang hors bande : bande trop étroite ; bande trop pleine : trop large.
        let narrow = |above: usize, inside: usize, r: usize| r < above || r >= above + inside;
        let (narrow_a, narrow_b) = (narrow(above_a, in_a, ra), narrow(above_b, in_b, rb));
        let full = in_a > BAND_MAX || in_b > BAND_MAX;
        if !narrow_a && !narrow_b && !full {
            let (na, nb) = (in_a, in_b);
            let qa = *band_a[..na].select_nth_unstable_by(ra - above_a, desc).1;
            let qb = *band_b[..nb].select_nth_unstable_by(rb - above_b, desc).1;
            // Viser quelques valeurs par bande.
            if in_a.max(in_b) > 12 {
                hint.width *= 0.8;
            }
            hint.prev = Some((qa, qb));
            return (qa, qb);
        }
        if full {
            hint.width *= 0.5;
        } else {
            hint.width = (hint.width * 2.0).min(0.5);
        }
    }
    let q = two_quantiles_desc(s, ra, rb);
    hint.prev = Some(q);
    q
}

/// Horizons radio des terminaux (`FindHorizons`).
pub fn find_horizons(z: &[f64], xi: f64, a_e: f64, h: [f64; 2]) -> ([f64; 2], [f64; 2]) {
    let np = z.len() - 1;
    let d = np as f64 * xi;
    let z_tx = z[0] + h[0];
    let z_rx = z[np] + h[1];
    let mut theta = [
        (z_rx - z_tx) / d - d / (2.0 * a_e),
        -(z_rx - z_tx) / d - d / (2.0 * a_e),
    ];
    let mut d_hzn = [d, d];
    let mut d_tx = 0.0;
    let mut d_rx = d;
    // Test sans division : t > θ  ⇔  (zi - z0) - d·(θ + d/2a) > 0 (d > 0). Un point n'est
    // candidat que si ce reste dépasse -TOL ; l'angle est alors recalculé avec l'expression
    // de la référence, qui seule décide. TOL (1 µm) couvre largement les erreurs d'arrondi
    // de la forme multipliée (~1e-12 m), donc le résultat est identique au bit près à la
    // boucle d'origine (2 divisions par point) tout en ne divisant que sur les candidats.
    const TOL: f64 = 1e-6;
    let inv_2a = 0.5 / a_e;
    for &zi in &z[1..np] {
        d_tx += xi;
        d_rx -= xi;
        if (zi - z_tx) - d_tx * (theta[0] + d_tx * inv_2a) > -TOL {
            let t_tx = (zi - z_tx) / d_tx - d_tx / (2.0 * a_e);
            if t_tx > theta[0] {
                theta[0] = t_tx;
                d_hzn[0] = d_tx;
            }
        }
        if (zi - z_rx) - d_rx * (theta[1] + d_rx * inv_2a) > -TOL {
            let t_rx = -(z_rx - zi) / d_rx - d_rx / (2.0 * a_e);
            if t_rx > theta[1] {
                theta[1] = t_rx;
                d_hzn[1] = d_rx;
            }
        }
    }
    (theta, d_hzn)
}

/// Extraction des paramètres du profil (`QuickPfl`).
pub fn quick_pfl(z: &[f64], xi: f64, gamma_e: f64, h: [f64; 2]) -> QuickPflOut {
    quick_pfl_hinted(z, xi, gamma_e, h, None)
}

/// Comme [`quick_pfl`], avec un indice facultatif pour le calcul de Δh.
pub fn quick_pfl_hinted(
    z: &[f64],
    xi: f64,
    gamma_e: f64,
    h: [f64; 2],
    hint: Option<&mut DeltaHHint>,
) -> QuickPflOut {
    let np = z.len() - 1;
    let d = np as f64 * xi;
    let a_e = 1.0 / gamma_e;
    let (theta, d_hzn) = find_horizons(z, xi, a_e, h);
    let d_start = (15.0 * h[0]).min(0.1 * d_hzn[0]);
    let d_end = d - (15.0 * h[1]).min(0.1 * d_hzn[1]);
    let delta_h = compute_delta_h_hinted(z, xi, d_start, d_end, hint);
    quick_pfl_finish(
        [z[0], z[np]],
        d,
        a_e,
        h,
        (theta, d_hzn),
        delta_h,
        (d_start, d_end),
        |a, b| linear_least_squares_fit(z, xi, a, b),
    )
}

/// Fin de `QuickPfl`, une fois les horizons et Δh connus : hauteurs effectives et, en vue
/// directe, horizons terre lisse. `lls(d_start, d_end)` est la régression linéaire du profil
/// (directe, ou par sommes préfixées en mode rapide).
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn quick_pfl_finish<F: Fn(f64, f64) -> (f64, f64)>(
    z_ends: [f64; 2],
    d: f64,
    a_e: f64,
    h: [f64; 2],
    horizons: ([f64; 2], [f64; 2]),
    delta_h: f64,
    (d_start, d_end): (f64, f64),
    lls: F,
) -> QuickPflOut {
    let (mut theta, mut d_hzn) = horizons;
    let terrain_d_hzn_m = d_hzn;
    let mut h_e = [0.0; 2];

    if d_hzn[0] + d_hzn[1] > 1.5 * d {
        let (fit_tx, fit_rx) = lls(d_start, d_end);
        h_e[0] = h[0] + fdim(z_ends[0], fit_tx);
        h_e[1] = h[1] + fdim(z_ends[1], fit_rx);
        let hzn =
            |he: f64| (2.0 * he * a_e).sqrt() * (-0.07 * (delta_h / he.max(5.0)).sqrt()).exp();
        for i in 0..2 {
            d_hzn[i] = hzn(h_e[i]);
        }
        let combined = d_hzn[0] + d_hzn[1];
        if combined <= d {
            let q = (d / combined).powi(2);
            for i in 0..2 {
                h_e[i] *= q;
                d_hzn[i] = hzn(h_e[i]);
            }
        }
        for i in 0..2 {
            let q = (2.0 * h_e[i] * a_e).sqrt();
            theta[i] = (0.65 * delta_h * (q / d_hzn[i] - 1.0) - 2.0 * h_e[i]) / q;
        }
    } else {
        let (fit_tx, _) = lls(d_start, 0.9 * d_hzn[0]);
        h_e[0] = h[0] + fdim(z_ends[0], fit_tx);
        let (_, fit_rx) = lls(d - 0.9 * d_hzn[1], d_end);
        h_e[1] = h[1] + fdim(z_ends[1], fit_rx);
    }

    QuickPflOut {
        theta_hzn: theta,
        d_hzn_m: d_hzn,
        h_e_m: h_e,
        delta_h_m: delta_h,
        d_m: d,
        terrain_d_hzn_m,
    }
}

fn terrain_roughness(d: f64, delta_h: f64) -> f64 {
    delta_h * (1.0 - 0.8 * (-d / 50e3).exp())
}

fn sigma_h(delta_h: f64) -> f64 {
    0.78 * delta_h * (-0.5 * delta_h.powf(0.25)).exp()
}

fn fresnel_integral(v2: f64) -> f64 {
    if v2 < 5.76 {
        6.02 + 9.11 * v2.sqrt() - 1.27 * v2
    } else {
        12.953 + 10.0 * v2.log10()
    }
}

fn knife_edge_diffraction(d: f64, f_mhz: f64, a_e: f64, theta_los: f64, d_hzn: [f64; 2]) -> f64 {
    let d_ml = d_hzn[0] + d_hzn[1];
    let theta_nlos = d / a_e - theta_los;
    let d_nlos = d - d_ml;
    let k = 0.0795775 * (f_mhz / 47.7) * theta_nlos.powi(2);
    let v1 = k * d_hzn[0] * d_nlos / (d_nlos + d_hzn[0]);
    let v2 = k * d_hzn[1] * d_nlos / (d_nlos + d_hzn[1]);
    fresnel_integral(v1) + fresnel_integral(v2)
}

fn height_function(x_km: f64, k: f64) -> f64 {
    if x_km < 200.0 {
        let w = -k.ln();
        if k < 1e-5 || x_km * w.powi(3) > 5495.0 {
            let mut r = -117.0;
            if x_km > 1.0 {
                r += 17.372 * x_km.ln();
            }
            r
        } else {
            2.5e-5 * x_km.powi(2) / k - 8.686 * w - 15.0
        }
    } else {
        let mut r = 0.05751 * x_km - 4.343 * x_km.ln();
        if x_km < 2000.0 {
            let w = 0.0134 * x_km * (-0.005 * x_km).exp();
            r = (1.0 - w) * r + w * (17.372 * x_km.ln() - 117.0);
        }
        r
    }
}

fn smooth_earth_diffraction(
    d: f64,
    f_mhz: f64,
    a_e: f64,
    theta_los: f64,
    d_hzn: [f64; 2],
    h_e: [f64; 2],
    z_g: Complex,
) -> f64 {
    let theta_nlos = d / a_e - theta_los;
    let d_ml = d_hzn[0] + d_hzn[1];
    let a = [
        (d - d_ml) / (d / a_e - theta_los),
        0.5 * d_hzn[0].powi(2) / h_e[0],
        0.5 * d_hzn[1].powi(2) / h_e[1],
    ];
    let d_km = [
        (a[0] * theta_nlos) / 1000.0,
        d_hzn[0] / 1000.0,
        d_hzn[1] / 1000.0,
    ];
    let mut c0 = [0.0; 3];
    let mut k = [0.0; 3];
    let mut b0 = [0.0; 3];
    let zabs = z_g.norm();
    for i in 0..3 {
        c0[i] = ((4.0 / 3.0) * A_0_M / a[i]).powf(THIRD);
        k[i] = 0.017778 * c0[i] * f_mhz.powf(-THIRD) / zabs;
        b0[i] = 1.607 - k[i];
    }
    let f3 = f_mhz.powf(THIRD);
    let x1 = b0[1] * c0[1].powi(2) * f3 * d_km[1];
    let x2 = b0[2] * c0[2].powi(2) * f3 * d_km[2];
    let x0 = b0[0] * c0[0].powi(2) * f3 * d_km[0] + x1 + x2;
    let f1 = height_function(x1, k[1]);
    let f2 = height_function(x2, k[2]);
    let g = 0.05751 * x0 - 10.0 * x0.log10();
    g - f1 - f2 - 20.0
}

#[allow(clippy::too_many_arguments)]
fn diffraction_loss(
    d: f64,
    d_hzn: [f64; 2],
    h_e: [f64; 2],
    z_g: Complex,
    a_e: f64,
    delta_h: f64,
    h: [f64; 2],
    theta_los: f64,
    d_sml: f64,
    f_mhz: f64,
) -> f64 {
    let a_k = knife_edge_diffraction(d, f_mhz, a_e, theta_los, d_hzn);
    let a_se = smooth_earth_diffraction(d, f_mhz, a_e, theta_los, d_hzn, h_e, z_g);
    let sig = sigma_h(terrain_roughness(d_sml, delta_h));
    let a_fo = (5.0 * (1.0 + 1e-5 * h[0] * h[1] * f_mhz * sig).log10()).min(15.0);
    let delta_h_d = terrain_roughness(d, delta_h);
    let mut q = h[0] * h[1];
    let qk = h_e[0] * h_e[1] - q;
    q += 10.0; // mode P2P
    let term1 = (1.0 + qk / q).sqrt();
    let d_ml = d_hzn[0] + d_hzn[1];
    let q = (term1 + (-theta_los * a_e + d_ml) / d) * (delta_h_d * f_mhz / 47.7).min(6283.2);
    let w = 25.1 / (25.1 + q.sqrt());
    w * a_se + (1.0 - w) * a_k + a_fo
}

#[allow(clippy::too_many_arguments)]
fn line_of_sight_loss(
    d: f64,
    h_e: [f64; 2],
    z_g: Complex,
    delta_h: f64,
    m_d: f64,
    a_d0: f64,
    d_sml: f64,
    f_mhz: f64,
) -> f64 {
    let sig = sigma_h(terrain_roughness(d, delta_h));
    let wn = f_mhz / 47.7;
    let hs = h_e[0] + h_e[1];
    let sin_psi = hs / (d * d + hs * hs).sqrt();
    let sp = Complex::new(sin_psi, 0.0);
    let mut r_e = ((sp - z_g) / (sp + z_g)) * (-(wn * sig * sin_psi).min(10.0)).exp();
    let q = r_e.re.powi(2) + r_e.im.powi(2);
    if q < 0.25 || q < sin_psi {
        r_e = r_e * (sin_psi / q).sqrt();
    }
    let mut dphi = wn * 2.0 * h_e[0] * h_e[1] / d;
    if dphi > PI / 2.0 {
        dphi = PI - (PI / 2.0).powi(2) / dphi;
    }
    let rr = Complex::new(dphi.cos(), -dphi.sin()) + r_e;
    let a_t = -10.0 * (rr.re.powi(2) + rr.im.powi(2)).log10();
    let a_d = m_d * d + a_d0;
    let w = 1.0 / (1.0 + f_mhz * delta_h / d_sml.max(10e3));
    w * a_t + (1.0 - w) * a_d
}

fn h0_curve(j: usize, r: f64) -> f64 {
    const A: [f64; 5] = [25.0, 80.0, 177.0, 395.0, 705.0];
    const B: [f64; 5] = [24.0, 45.0, 68.0, 80.0, 105.0];
    10.0 * (1.0 + A[j] * (1.0 / r).powi(4) + B[j] * (1.0 / r).powi(2)).log10()
}

fn h0_function(r: f64, eta_s: f64) -> f64 {
    let eta_s = eta_s.clamp(1.0, 5.0);
    let i = eta_s as usize;
    let q = eta_s - i as f64;
    let mut res = h0_curve(i - 1, r);
    if q != 0.0 {
        res = (1.0 - q) * res + q * h0_curve(i, r);
    }
    res
}

fn f_function(td: f64) -> f64 {
    const A: [f64; 3] = [133.4, 104.6, 71.8];
    const B: [f64; 3] = [0.332e-3, 0.212e-3, 0.157e-3];
    const C: [f64; 3] = [-10.0, -2.5, 5.0];
    let i = if td <= 10e3 {
        0
    } else if td <= 70e3 {
        1
    } else {
        2
    };
    A[i] + B[i] * td + C[i] * td.log10()
}

#[allow(clippy::too_many_arguments)]
fn troposcatter_loss(
    d: f64,
    theta_hzn: [f64; 2],
    d_hzn: [f64; 2],
    h_e: [f64; 2],
    a_e: f64,
    n_s: f64,
    f_mhz: f64,
    theta_los: f64,
    h0: &mut f64,
) -> f64 {
    let wn = f_mhz / 47.7;
    let h_0 = if *h0 > 15.0 {
        *h0
    } else {
        let mut ad = d_hzn[0] - d_hzn[1];
        let mut rr = h_e[1] / h_e[0];
        if ad < 0.0 {
            ad = -ad;
            rr = 1.0 / rr;
        }
        let theta = theta_hzn[0] + theta_hzn[1] + d / a_e;
        let r1 = 2.0 * wn * theta * h_e[0];
        let r2 = 2.0 * wn * theta * h_e[1];
        if r1 < 0.2 && r2 < 0.2 {
            return 1001.0;
        }
        let mut s = (d - ad) / (d + ad);
        let q = (rr / s).clamp(0.1, 10.0);
        s = s.max(0.1);
        let h_0_m = (d - ad) * (d + ad) * theta * 0.25 / d;
        let z0 = 1.7556e3;
        let z1 = 8.0e3;
        let eta_s = (h_0_m / z0)
            * (1.0
                + (0.031 - n_s * 2.32e-3 + n_s.powi(2) * 5.67e-6)
                    * (-(h_0_m / z1).min(1.7).powi(6)).exp());
        let h_00 = (h0_function(r1, eta_s) + h0_function(r2, eta_s)) / 2.0;
        let dh0 = h_00.min(6.0 * (0.6 - eta_s.max(1.0).log10()) * s.log10() * q.log10());
        let mut hh = (h_00 + dh0).max(0.0);
        if eta_s < 1.0 {
            hh = eta_s * hh
                + (1.0 - eta_s)
                    * 10.0
                    * (((1.0 + SQRT2 / r1) * (1.0 + SQRT2 / r2)).powi(2) * (r1 + r2)
                        / (r1 + r2 + 2.0 * SQRT2))
                        .log10();
        }
        if hh > 15.0 && *h0 >= 0.0 {
            hh = *h0;
        }
        hh
    };
    *h0 = h_0;
    let th = d / a_e - theta_los;
    f_function(th * d) + 10.0 * (wn * 47.7 * th.powi(4)).log10()
        - 0.1 * (n_s - 301.0) * (-th * d / 40e3).exp()
        + h_0
}

/// Affaiblissement de référence Longley-Rice (`LongleyRice`, mode P2P).
#[allow(clippy::too_many_arguments)]
pub fn longley_rice(
    theta_hzn: [f64; 2],
    f_mhz: f64,
    z_g: Complex,
    d_hzn: [f64; 2],
    h_e: [f64; 2],
    gamma_e: f64,
    n_s: f64,
    delta_h: f64,
    h: [f64; 2],
    d: f64,
    w: &mut Warnings,
) -> Result<(f64, PropMode), ItmError> {
    let a_e = 1.0 / gamma_e;
    let d_hzn_s = [(2.0 * h_e[0] * a_e).sqrt(), (2.0 * h_e[1] * a_e).sqrt()];
    let d_sml = d_hzn_s[0] + d_hzn_s[1];
    let d_ml = d_hzn[0] + d_hzn[1];
    let theta_los = -(theta_hzn[0] + theta_hzn[1]).max(-d_ml / a_e);

    if theta_hzn[0].abs() > 200e-3 {
        w.set(Warnings::TX_HORIZON_ANGLE);
    }
    if theta_hzn[1].abs() > 200e-3 {
        w.set(Warnings::RX_HORIZON_ANGLE);
    }
    if d_hzn[0] < 0.1 * d_hzn_s[0] {
        w.set(Warnings::TX_HORIZON_DISTANCE_1);
    }
    if d_hzn[1] < 0.1 * d_hzn_s[1] {
        w.set(Warnings::RX_HORIZON_DISTANCE_1);
    }
    if d_hzn[0] > 3.0 * d_hzn_s[0] {
        w.set(Warnings::TX_HORIZON_DISTANCE_2);
    }
    if d_hzn[1] > 3.0 * d_hzn_s[1] {
        w.set(Warnings::RX_HORIZON_DISTANCE_2);
    }
    if n_s < 150.0 {
        return Err(ItmError::SurfaceRefractivitySmall);
    }
    if n_s > 400.0 {
        return Err(ItmError::SurfaceRefractivityLarge);
    }
    if n_s < 250.0 {
        w.set(Warnings::SURFACE_REFRACTIVITY);
    }
    if !(4_000_000.0..=13_333_333.0).contains(&a_e) {
        return Err(ItmError::EffectiveEarth);
    }
    if z_g.re <= z_g.im.abs() {
        return Err(ItmError::GroundImpedance);
    }

    let scale = (a_e.powi(2) / f_mhz).powf(THIRD);
    let d3 = d_sml.max(d_ml + 5.0 * scale);
    let d4 = d3 + 10.0 * scale;
    let a3 = diffraction_loss(
        d3, d_hzn, h_e, z_g, a_e, delta_h, h, theta_los, d_sml, f_mhz,
    );
    let a4 = diffraction_loss(
        d4, d_hzn, h_e, z_g, a_e, delta_h, h, theta_los, d_sml, f_mhz,
    );
    let m_d = (a4 - a3) / (d4 - d3);
    let a_d0 = a3 - m_d * d3;

    let d_min = (h_e[0] - h_e[1]).abs() / 200e-3;
    if d < d_min {
        w.set(Warnings::PATH_DISTANCE_TOO_SMALL_1);
    }
    if d < 1e3 {
        w.set(Warnings::PATH_DISTANCE_TOO_SMALL_2);
    }
    if d > 1000e3 {
        w.set(Warnings::PATH_DISTANCE_TOO_BIG_1);
    }
    if d > 2000e3 {
        w.set(Warnings::PATH_DISTANCE_TOO_BIG_2);
    }

    let (a_ref, mode);
    if d < d_sml {
        let a_sml = d_sml * m_d + a_d0;
        let mut d0 = 0.04 * f_mhz * h_e[0] * h_e[1];
        let d1 = if a_d0 >= 0.0 {
            d0 = d0.min(0.5 * d_ml);
            d0 + 0.25 * (d_ml - d0)
        } else {
            (-a_d0 / m_d).max(0.25 * d_ml)
        };
        let a1 = line_of_sight_loss(d1, h_e, z_g, delta_h, m_d, a_d0, d_sml, f_mhz);
        let mut flag = false;
        let mut k1 = 0.0;
        let mut k2 = 0.0;
        if d0 < d1 {
            let a0 = line_of_sight_loss(d0, h_e, z_g, delta_h, m_d, a_d0, d_sml, f_mhz);
            let q = (d_sml / d0).ln();
            k2 = (((d_sml - d0) * (a1 - a0) - (d1 - d0) * (a_sml - a0))
                / ((d_sml - d0) * (d1 / d0).ln() - (d1 - d0) * q))
                .max(0.0);
            flag = a_d0 > 0.0 || k2 > 0.0;
            if flag {
                k1 = (a_sml - a0 - k2 * q) / (d_sml - d0);
                if k1 < 0.0 {
                    k1 = 0.0;
                    k2 = fdim(a_sml, a0) / q;
                    if k2 == 0.0 {
                        k1 = m_d;
                    }
                }
            }
        }
        if !flag {
            k1 = fdim(a_sml, a1) / (d_sml - d1);
            k2 = 0.0;
            if k1 == 0.0 {
                k1 = m_d;
            }
        }
        let a_o = a_sml - k1 * d_sml - k2 * d_sml.ln();
        a_ref = a_o + k1 * d + k2 * d.ln();
        mode = PropMode::LineOfSight;
    } else {
        let d5 = d_ml + 200e3;
        let d6 = d_ml + 400e3;
        let mut h0 = -1.0;
        let a6 = troposcatter_loss(
            d6, theta_hzn, d_hzn, h_e, a_e, n_s, f_mhz, theta_los, &mut h0,
        );
        let a5 = troposcatter_loss(
            d5, theta_hzn, d_hzn, h_e, a_e, n_s, f_mhz, theta_los, &mut h0,
        );
        let (m_s, a_s0, d_x);
        if a5 < 1000.0 {
            m_s = (a6 - a5) / 200e3;
            d_x = d_sml
                .max(d_ml + 1.088 * scale * f_mhz.ln())
                .max((a5 - a_d0 - m_s * d5) / (m_d - m_s));
            a_s0 = (m_d - m_s) * d_x + a_d0;
        } else {
            m_s = m_d;
            a_s0 = a_d0;
            d_x = 10e6;
        }
        if d > d_x {
            a_ref = m_s * d + a_s0;
            mode = PropMode::Troposcatter;
        } else {
            a_ref = m_d * d + a_d0;
            mode = PropMode::Diffraction;
        }
    }
    Ok((a_ref.max(0.0), mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boucle d'origine (référence NTIA) : 2 divisions par point.
    fn find_horizons_ref(z: &[f64], xi: f64, a_e: f64, h: [f64; 2]) -> ([f64; 2], [f64; 2]) {
        let np = z.len() - 1;
        let d = np as f64 * xi;
        let z_tx = z[0] + h[0];
        let z_rx = z[np] + h[1];
        let mut theta = [
            (z_rx - z_tx) / d - d / (2.0 * a_e),
            -(z_rx - z_tx) / d - d / (2.0 * a_e),
        ];
        let mut d_hzn = [d, d];
        let mut d_tx = 0.0;
        let mut d_rx = d;
        for &zi in &z[1..np] {
            d_tx += xi;
            d_rx -= xi;
            let t_tx = (zi - z_tx) / d_tx - d_tx / (2.0 * a_e);
            let t_rx = -(z_rx - zi) / d_rx - d_rx / (2.0 * a_e);
            if t_tx > theta[0] {
                theta[0] = t_tx;
                d_hzn[0] = d_tx;
            }
            if t_rx > theta[1] {
                theta[1] = t_rx;
                d_hzn[1] = d_rx;
            }
        }
        (theta, d_hzn)
    }

    /// Générateur pseudo-aléatoire déterministe (xorshift64*).
    fn rng(state: &mut u64) -> f64 {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        (state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `ComputeDeltaH` d'origine (deux sélections complètes, rééchantillonnage pas à pas).
    fn compute_delta_h_ref(z: &[f64], xi: f64, d_start: f64, d_end: f64) -> f64 {
        let np = (z.len() - 1) as i64;
        let mut x_start = d_start / xi;
        let mut x_end = d_end / xi;
        if x_end - x_start < 2.0 {
            return 0.0;
        }
        let p10 = ((0.1 * (x_end - x_start + 8.0)) as i64).clamp(4, 25);
        let n = 10 * p10 - 5;
        let p90 = n - p10;
        let np_s = (n - 1) as f64;
        x_end = (x_end - x_start) / np_s;
        let mut i = x_start as i64;
        x_start -= (i + 1) as f64;
        let mut s = vec![0.0f64; n as usize];
        for v in s.iter_mut() {
            while x_start > 0.0 && (i + 1) < np {
                x_start -= 1.0;
                i += 1;
            }
            let a = z[(i + 1) as usize];
            let b = z[i as usize];
            *v = a + (a - b) * x_start;
            x_start += x_end;
        }
        let (mut fit_y1, fit_y2) = linear_least_squares_fit(&s, 1.0, 0.0, np_s);
        let step = (fit_y2 - fit_y1) / np_s;
        for v in s.iter_mut() {
            *v -= fit_y1;
            fit_y1 += step;
        }
        let desc = |a: &f64, b: &f64| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal);
        let q10 = *s.select_nth_unstable_by((p10 - 1) as usize, desc).1;
        let q90 = *s.select_nth_unstable_by(p90 as usize, desc).1;
        (q10 - q90) / (1.0 - 0.8 * (-(d_end - d_start) / 50e3).exp())
    }

    fn test_profiles(n: usize) -> Vec<Vec<f64>> {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut walk = 500.0;
        vec![
            // relief montagneux
            (0..=n)
                .map(|i| {
                    let x = i as f64;
                    800.0 + 600.0 * (x * 0.013).sin() + 80.0 * (x * 0.31).cos()
                })
                .collect(),
            // terrain plat, cas d'égalités
            vec![100.0; n + 1],
            // marches entières (nombreuses égalités exactes)
            (0..=n).map(|i| ((i / 37) % 5) as f64 * 50.0).collect(),
            // bruit aléatoire
            (0..=n).map(|_| 2000.0 * rng(&mut s)).collect(),
            // marche aléatoire (relief réaliste)
            (0..=n)
                .map(|_| {
                    walk += 40.0 * (rng(&mut s) - 0.5);
                    walk
                })
                .collect(),
        ]
    }

    #[test]
    fn delta_h_hinted_matches_reference_bit_for_bit() {
        let n = 1200;
        let profiles = test_profiles(n);
        // Indice partagé entre profils et pas : un indice périmé doit rester exact.
        let mut shared = DeltaHHint::default();
        for z in &profiles {
            for xi in [30.0, 92.662_566_861_277_44, 250.0] {
                let mut hint = DeltaHHint::default();
                for k in 2..=n {
                    let d = k as f64 * xi;
                    let (d0, d1) = (
                        (15.0 * 30.0f64).min(0.1 * d),
                        d - (15.0 * 2.0f64).min(0.1 * d),
                    );
                    let want = compute_delta_h_ref(&z[..=k], xi, d0, d1);
                    let plain = compute_delta_h(&z[..=k], xi, d0, d1);
                    let hinted = compute_delta_h_hinted(&z[..=k], xi, d0, d1, Some(&mut hint));
                    let stale = compute_delta_h_hinted(&z[..=k], xi, d0, d1, Some(&mut shared));
                    for (name, got) in [
                        ("sans indice", plain),
                        ("indice", hinted),
                        ("indice périmé", stale),
                    ] {
                        assert_eq!(
                            got.to_bits(),
                            want.to_bits(),
                            "{name} k={k} xi={xi}: {got} vs {want}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn find_horizons_matches_reference_bit_for_bit() {
        let a_e = 1.0 / (157e-9 * (1.0 - 0.04665 * (301.0f64 / 179.3).exp()));
        let n = 1200;
        let profiles = test_profiles(n);
        for z in &profiles {
            for xi in [30.0, 92.662_566_861_277_44, 250.0] {
                for h in [[30.0, 2.0], [1.0, 1.0], [500.0, 10.0]] {
                    for k in 2..=n {
                        let got = find_horizons(&z[..=k], xi, a_e, h);
                        let want = find_horizons_ref(&z[..=k], xi, a_e, h);
                        assert!(
                            got.0[0].to_bits() == want.0[0].to_bits()
                                && got.0[1].to_bits() == want.0[1].to_bits()
                                && got.1[0].to_bits() == want.1[0].to_bits()
                                && got.1[1].to_bits() == want.1[1].to_bits(),
                            "k={k} xi={xi} h={h:?}: {got:?} != {want:?}"
                        );
                    }
                }
            }
        }
    }
}
