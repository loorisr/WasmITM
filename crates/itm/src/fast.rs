//! Mode rapide, approché, pour les cartes de couverture : ITM le long d'une radiale, à des
//! distances croissantes (sous-profils `[0..=k]` d'un même profil).
//!
//! Même principe que l'option `-fast N` de Signal-Server :
//! - **horizons en O(log n)** au lieu de O(n) : vu d'une extrémité, l'horizon est le point de
//!   tangence de l'enveloppe convexe supérieure des points `(x_i, z_i − x_i²/2a)` ; l'enveloppe
//!   est construite au fil de la radiale. Elle dépend du rayon terrestre effectif `a`, qui varie
//!   un peu d'un sous-profil à l'autre (via la hauteur moyenne) : elle est construite pour un
//!   `a` de référence et reconstruite quand l'écart dépasse [`QC_REBUILD_TOL`]. L'angle et la
//!   distance d'horizon sont ensuite recalculés avec le `a` exact du sous-profil ;
//! - **Δh recalculé tous les N points** (« ancres » k = N, 2N, 3N…) et interpolé linéairement
//!   entre deux ancres ; exact sur les N premiers points de la radiale. `N = 1` : Δh exact à
//!   chaque point, seuls les horizons et les sommes sont accélérés (mode quasi exact) ;
//! - **sommes préfixées** pour la hauteur moyenne du trajet et les régressions linéaires
//!   (O(1) au lieu de O(n)).
//!
//! Le reste (Longley-Rice, variabilité) est le calcul de référence.

use crate::complex::Complex;
use crate::core::{
    compute_delta_h_hinted, fdim, find_horizons, quick_pfl_finish, DeltaHHint, QuickPflOut,
};
use crate::Warnings;
use crate::{finish, ground_impedance, refractivity, validate, ItmError, P2pParams, P2pResult};

/// Pas des ancres de Δh par défaut, comme Signal-Server (`-fast` = `-fast 50`). Sur une carte
/// de 100 km dans les Alpes : 2,85× plus rapide que le mode exact sur un thread, 99,72 % des
/// pixels à 3 dB près. Voir `docs/FAST_MODE.md`.
pub const FAST_DEFAULT_STRIDE: u32 = 50;

/// Écart relatif de courbure (`1/2a`) au-delà duquel l'enveloppe convexe est reconstruite.
pub const QC_REBUILD_TOL: f64 = 1e-3;

/// Évaluateur ITM rapide pour une radiale. Les appels à [`FastRadial::loss`] doivent se faire
/// à `k` croissant (sinon les structures sont reconstruites, ce qui reste correct mais lent).
pub struct FastRadial<'a> {
    p: P2pParams,
    w0: Warnings,
    z_g: Complex,
    z: &'a [f64],
    xi: f64,
    stride: usize,
    /// `s0[i]` = Σ z[j], `s1[i]` = Σ j·z[j], pour j < i
    s0: Vec<f64>,
    s1: Vec<f64>,
    /// Distance du point i à l'émetteur, accumulée comme dans `find_horizons` (`d += xi`)
    d_tx: Vec<f64>,
    /// Enveloppe convexe supérieure des points intérieurs `1..added`, pour la courbure `qc_ref`
    hull: Vec<u32>,
    hull_y: Vec<f64>,
    qc_ref: f64,
    added: usize,
    rebuilds: u64,
    hint: DeltaHHint,
    anchor_hint: DeltaHHint,
    /// Δh aux ancres (indice m → ancre m·stride), NaN = pas encore calculé
    anchors: Vec<f64>,
}

impl<'a> FastRadial<'a> {
    /// `z` : profil complet de la radiale (émetteur en 0), au pas `xi` (m). `stride` : pas des
    /// ancres de Δh (≥ 1).
    pub fn new(p: &P2pParams, z: &'a [f64], xi: f64, stride: u32) -> Result<Self, ItmError> {
        let mut w0 = Warnings::default();
        validate(p, &mut w0)?;
        if z.len() < 2 || xi.is_nan() || xi <= 0.0 {
            return Err(ItmError::PathDistance);
        }
        let n = z.len();
        let mut s0 = Vec::with_capacity(n + 1);
        let mut s1 = Vec::with_capacity(n + 1);
        let mut d_tx = Vec::with_capacity(n);
        let (mut a, mut b, mut d) = (0.0, 0.0, 0.0);
        s0.push(0.0);
        s1.push(0.0);
        for (j, &v) in z.iter().enumerate() {
            a += v;
            b += j as f64 * v;
            s0.push(a);
            s1.push(b);
            d_tx.push(d);
            d += xi;
        }
        let stride = stride.max(1) as usize;
        Ok(Self {
            p: *p,
            w0,
            z_g: ground_impedance(p),
            z,
            xi,
            stride,
            s0,
            s1,
            d_tx,
            hull: Vec::new(),
            hull_y: Vec::new(),
            qc_ref: f64::NAN,
            added: 1,
            rebuilds: 0,
            hint: DeltaHHint::default(),
            anchor_hint: DeltaHHint::default(),
            anchors: vec![f64::NAN; (n - 1) / stride + 2],
        })
    }

    /// Nombre de reconstructions de l'enveloppe convexe (diagnostic).
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Hauteur moyenne du sous-profil `[0..=np]`, 10 % ignorés à chaque bout.
    #[inline]
    fn h_sys(&self, np: usize) -> f64 {
        let p10 = (0.1 * np as f64) as usize;
        (self.s0[np - p10 + 1] - self.s0[p10]) / (np - 2 * p10 + 1) as f64
    }

    /// `LinearLeastSquaresFit` sur `z[0..=np]`, par sommes préfixées.
    #[inline]
    fn lls(&self, np: usize, d_start: f64, d_end: f64) -> (f64, f64) {
        let (z, xi) = (self.z, self.xi);
        let np_i = np as i64;
        let mut i_start = fdim(d_start / xi, 0.0) as i64;
        let mut i_end = np_i - fdim(np as f64, d_end / xi) as i64;
        if i_end <= i_start {
            i_start = fdim(i_start as f64, 1.0) as i64;
            i_end = np_i - fdim(np as f64, (i_end + 1) as f64) as i64;
        }
        let x_length = (i_end - i_start) as f64;
        let mid0 = -0.5 * x_length;
        let mid_end = i_end as f64 + mid0;
        let (a, b) = (i_start as usize, i_end as usize);
        // Points intérieurs a+1 ..= b-1, de poids (mid0 + j - a)
        let (t0, t1) = if b > a + 1 {
            (self.s0[b] - self.s0[a + 1], self.s1[b] - self.s1[a + 1])
        } else {
            (0.0, 0.0)
        };
        let mut sum_y = 0.5 * (z[a] + z[b]) + t0;
        let mut scaled = 0.5 * (z[a] - z[b]) * mid0 + (mid0 - a as f64) * t0 + t1;
        sum_y /= x_length;
        scaled = scaled * 12.0 / ((x_length * x_length + 2.0) * x_length);
        (
            sum_y - scaled * mid_end,
            sum_y + scaled * (np as f64 - mid_end),
        )
    }

    #[inline]
    fn hull_push(&mut self, i: usize) {
        let x = i as f64 * self.xi;
        let y = self.z[i] - self.qc_ref * x * x;
        while self.hull.len() >= 2 {
            let n = self.hull.len();
            let (xa, ya) = (self.hull[n - 2] as f64 * self.xi, self.hull_y[n - 2]);
            let (xb, yb) = (self.hull[n - 1] as f64 * self.xi, self.hull_y[n - 1]);
            // b sous (ou sur) la droite a → i : b n'est plus sur l'enveloppe supérieure.
            if (xb - xa) * (y - ya) - (yb - ya) * (x - xa) >= 0.0 {
                self.hull.pop();
                self.hull_y.pop();
            } else {
                break;
            }
        }
        self.hull.push(i as u32);
        self.hull_y.push(y);
    }

    /// Met l'enveloppe à jour pour les points intérieurs `1..np`, à la courbure `qc`.
    fn advance(&mut self, np: usize, qc: f64) {
        let drift = (qc - self.qc_ref).abs() > QC_REBUILD_TOL * qc;
        if drift || np < self.added || self.qc_ref.is_nan() {
            if !self.qc_ref.is_nan() {
                self.rebuilds += 1;
            }
            self.qc_ref = qc;
            self.hull.clear();
            self.hull_y.clear();
            self.added = 1;
        }
        while self.added < np {
            self.hull_push(self.added);
            self.added += 1;
        }
    }

    /// Horizons (angles, distances) du sous-profil `[0..=np]` pour le rayon effectif `a_e` ;
    /// même convention que [`find_horizons`].
    fn horizons(&mut self, np: usize, a_e: f64) -> ([f64; 2], [f64; 2]) {
        let (z, xi) = (self.z, self.xi);
        let h = [self.p.h_tx_m, self.p.h_rx_m];
        let d = np as f64 * xi;
        let z_tx = z[0] + h[0];
        let z_rx = z[np] + h[1];
        let mut theta = [
            (z_rx - z_tx) / d - d / (2.0 * a_e),
            -(z_rx - z_tx) / d - d / (2.0 * a_e),
        ];
        let mut d_hzn = [d, d];
        if np < 2 {
            return (theta, d_hzn);
        }
        self.advance(np, 0.5 / a_e);
        let n = self.hull.len();
        let x = |m: usize| self.hull[m] as f64 * xi;
        let y = |m: usize| self.hull_y[m];

        // Émetteur : pente maximale depuis (0, z_tx). Unimodale le long de l'enveloppe.
        let (mut lo, mut hi) = (0, n - 1);
        while lo < hi {
            let m = (lo + hi) / 2;
            if (y(m) - z_tx) * x(m + 1) < (y(m + 1) - z_tx) * x(m) {
                lo = m + 1;
            } else {
                hi = m;
            }
        }
        let t_tx = |i: usize| (z[i] - z_tx) / self.d_tx[i] - self.d_tx[i] / (2.0 * a_e);
        let best = refine(&self.hull, lo, t_tx);
        let t = t_tx(best);
        if t > theta[0] {
            theta[0] = t;
            d_hzn[0] = self.d_tx[best];
        }

        // Récepteur : tangente depuis (d, z_rx − qc·d²), à droite de tous les points.
        let y_p = z_rx - self.qc_ref * d * d;
        let (mut lo, mut hi) = (0, n - 1);
        while lo < hi {
            let m = (lo + hi) / 2;
            if (y(m) - y_p) * (d - x(m + 1)) < (y(m + 1) - y_p) * (d - x(m)) {
                lo = m + 1;
            } else {
                hi = m;
            }
        }
        // Choix du point avec la distance approchée, puis angle et distance avec la
        // distance accumulée exactement comme la référence.
        let t_rx_at = |i: usize, dr: f64| -(z_rx - z[i]) / dr - dr / (2.0 * a_e);
        let best = refine(&self.hull, lo, |i| t_rx_at(i, (np - i) as f64 * xi));
        let dr = repeated_sub(d, xi, best);
        let t = t_rx_at(best, dr);
        if t > theta[1] {
            theta[1] = t;
            d_hzn[1] = dr;
        }
        (theta, d_hzn)
    }

    /// Δh exact du sous-profil `[0..=m]` (ancre), horizons calculés par la méthode de référence.
    fn anchor(&mut self, m: usize) -> f64 {
        let slot = m / self.stride + usize::from(!m.is_multiple_of(self.stride));
        if let Some(v) = self.anchors.get(slot).filter(|v| !v.is_nan()) {
            return *v;
        }
        let z = &self.z[..=m];
        let h = [self.p.h_tx_m, self.p.h_rx_m];
        let (gamma_e, _) = refractivity(self.p.n0, self.h_sys(m));
        let (_, d_hzn) = find_horizons(z, self.xi, 1.0 / gamma_e, h);
        let d = m as f64 * self.xi;
        let d_start = (15.0 * h[0]).min(0.1 * d_hzn[0]);
        let d_end = d - (15.0 * h[1]).min(0.1 * d_hzn[1]);
        let v = compute_delta_h_hinted(z, self.xi, d_start, d_end, Some(&mut self.anchor_hint));
        if let Some(s) = self.anchors.get_mut(slot) {
            *s = v;
        }
        v
    }

    /// Affaiblissement ITM du sous-profil `[0..=k]` (récepteur au point k, k ≥ 1).
    pub fn loss(&mut self, k: usize) -> Result<P2pResult, ItmError> {
        if k == 0 || k >= self.z.len() {
            return Err(ItmError::PathDistance);
        }
        let p = self.p;
        let (xi, np) = (self.xi, k);
        let h = [p.h_tx_m, p.h_rx_m];
        let (gamma_e, n_s) = refractivity(p.n0, self.h_sys(np));
        let a_e = 1.0 / gamma_e;
        let d = np as f64 * xi;
        let (theta, d_hzn) = self.horizons(np, a_e);
        let d_start = (15.0 * h[0]).min(0.1 * d_hzn[0]);
        let d_end = d - (15.0 * h[1]).min(0.1 * d_hzn[1]);

        let s = self.stride;
        let delta_h = if s <= 1 || np <= s {
            compute_delta_h_hinted(&self.z[..=np], xi, d_start, d_end, Some(&mut self.hint))
        } else {
            let a = np / s * s;
            if a == np {
                self.anchor(a)
            } else {
                let b = (a + s).min(self.z.len() - 1);
                let (va, vb) = (self.anchor(a), self.anchor(b));
                va + (vb - va) * (np - a) as f64 / (b - a) as f64
            }
        };

        let qp: QuickPflOut = quick_pfl_finish(
            [self.z[0], self.z[np]],
            d,
            a_e,
            h,
            (theta, d_hzn),
            delta_h,
            (d_start, d_end),
            |a, b| self.lls(np, a, b),
        );
        finish(&p, self.w0, self.z_g, gamma_e, n_s, &qp, xi)
    }
}

/// `x` après `n` soustractions flottantes successives `x -= y` (x > 0, y > 0), en
/// O(nombre de binades) : dans une binade, `x` est un multiple de l'ulp `u` et chaque
/// soustraction retire exactement `y` arrondi à la grille de `u` (progression arithmétique
/// exacte), tant que le résultat reste dans la binade. Cas d'égalité d'arrondi (`y/u` en
/// demi-entier, dépendant de la parité) et sortie de binade : pas à pas.
fn repeated_sub(mut x: f64, y: f64, mut n: usize) -> f64 {
    while n > 0 {
        if !(x.is_normal() && x > 0.0) {
            x -= y;
            n -= 1;
            continue;
        }
        let lo = f64::from_bits(x.to_bits() & 0xfff0_0000_0000_0000);
        let u = lo * f64::EPSILON;
        let q = y / u;
        let delta = q.round() * u;
        let tie = q - q.floor() == 0.5;
        let fit = if delta > 0.0 && !tie {
            ((x - lo) / delta).floor() as usize
        } else {
            0
        };
        // Garde-fou : ne garder que des pas qui restent dans la binade.
        let mut m = fit.min(n);
        while m > 0 && x - m as f64 * delta < lo {
            m -= 1;
        }
        if m == 0 {
            x -= y;
            n -= 1;
        } else {
            x -= m as f64 * delta;
            n -= m;
        }
    }
    x
}

/// Part du sommet `m` de l'enveloppe (choisi avec la courbure de référence) et suit
/// l'enveloppe tant que l'angle exact `f` augmente ; égalité : l'indice le plus petit,
/// comme la référence (`>` strict en parcourant le profil).
#[inline]
fn refine<F: Fn(usize) -> f64>(hull: &[u32], mut m: usize, f: F) -> usize {
    let mut best = f(hull[m] as usize);
    while m > 0 {
        let v = f(hull[m - 1] as usize);
        if v >= best {
            best = v;
            m -= 1;
        } else {
            break;
        }
    }
    while m + 1 < hull.len() {
        let v = f(hull[m + 1] as usize);
        if v > best {
            best = v;
            m += 1;
        } else {
            break;
        }
    }
    hull[m] as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{p2p, P2pParams};

    fn rng(state: &mut u64) -> f64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Profils variés : montagne, plaine, mer (0 m), marches.
    fn profiles() -> Vec<Vec<f64>> {
        let mut st = 0x9e3779b97f4a7c15u64;
        let mut out = Vec::new();
        for kind in 0..6 {
            let n = 600 + 200 * kind;
            let ph = rng(&mut st) * 6.0;
            let v: Vec<f64> = (0..=n)
                .map(|i| {
                    let x = i as f64;
                    match kind {
                        0 => 0.0,
                        1 => 200.0 + 5.0 * rng(&mut st),
                        2 => {
                            if (i / 97) % 2 == 0 {
                                300.0
                            } else {
                                900.0
                            }
                        }
                        _ => {
                            1000.0
                                + 700.0 * (x * 0.011 + ph).sin()
                                + 300.0 * (x * 0.047).sin()
                                + 60.0 * rng(&mut st)
                        }
                    }
                })
                .collect();
            out.push(v);
        }
        out
    }

    fn params() -> P2pParams {
        P2pParams {
            h_tx_m: 30.0,
            h_rx_m: 2.0,
            f_mhz: 145.0,
            ..Default::default()
        }
    }

    #[test]
    fn repeated_sub_matches_loop() {
        let mut st = 12345u64;
        for _ in 0..2000 {
            let y = 10.0 + 200.0 * rng(&mut st);
            let np = 2 + (3000.0 * rng(&mut st)) as usize;
            let x0 = np as f64 * y;
            let mut x = x0;
            for n in 0..np {
                assert_eq!(
                    repeated_sub(x0, y, n).to_bits(),
                    x.to_bits(),
                    "y={y} np={np} n={n}"
                );
                x -= y;
            }
        }
        // Valeurs réelles : pas GLO-90
        let y = 92.66;
        let x0 = 1700.0 * y;
        let mut x = x0;
        for n in 0..1700 {
            assert_eq!(repeated_sub(x0, y, n).to_bits(), x.to_bits());
            x -= y;
        }
    }

    #[test]
    fn horizons_match_reference() {
        let p = params();
        let xi = 92.66;
        for z in profiles() {
            let mut fr = FastRadial::new(&p, &z, xi, 1).unwrap();
            for k in 2..z.len() {
                let (gamma_e, _) = refractivity(p.n0, fr.h_sys(k));
                let a_e = 1.0 / gamma_e;
                let (tf, df) = fr.horizons(k, a_e);
                let (tr, dr) = find_horizons(&z[..=k], xi, a_e, [p.h_tx_m, p.h_rx_m]);
                for i in 0..2 {
                    assert!(
                        (tf[i] - tr[i]).abs() < 1e-9,
                        "k={k} θ{i} {} {}",
                        tf[i],
                        tr[i]
                    );
                    assert_eq!(
                        df[i].to_bits(),
                        dr[i].to_bits(),
                        "k={k} d{i} {} {}",
                        df[i],
                        dr[i]
                    );
                }
            }
        }
    }

    #[test]
    fn stride_1_close_to_exact() {
        let p = params();
        let xi = 92.66;
        for z in profiles() {
            let mut fr = FastRadial::new(&p, &z, xi, 1).unwrap();
            for k in 2..z.len() {
                let ra = fr.loss(k).unwrap();
                let rb = p2p(&p, &z[..=k], xi).unwrap();
                let (a, b) = (ra.loss_db, rb.loss_db);
                assert!(
                    (a - b).abs() < 1e-6,
                    "k={k}: {a} vs {b}\n{:?}\n{:?}\n{:?}",
                    ra.intermediate,
                    rb.intermediate,
                    &z[..4]
                );
            }
        }
    }

    #[test]
    fn stride_50_bounded_error() {
        let p = params();
        let xi = 92.66;
        let (mut n, mut within) = (0usize, 0usize);
        for z in profiles() {
            let mut fr = FastRadial::new(&p, &z, xi, 50).unwrap();
            // Pas irréguliers, comme en mode « nearest » (points sautés).
            let mut k = 2;
            while k < z.len() {
                let a = fr.loss(k).unwrap().loss_db;
                let b = p2p(&p, &z[..=k], xi).unwrap().loss_db;
                n += 1;
                within += ((a - b).abs() <= 3.0) as usize;
                k += 1 + k % 3;
            }
        }
        assert!(within as f64 >= 0.99 * n as f64, "{within}/{n}");
    }
}
