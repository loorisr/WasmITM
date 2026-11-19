//! Nombres complexes minimaux (évite une dépendance pour quelques opérations).

use std::ops::{Add, Div, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }

    /// Racine carrée principale (comme std::sqrt(std::complex)).
    pub fn sqrt(self) -> Self {
        let r = self.norm();
        if r == 0.0 {
            return Self::new(0.0, 0.0);
        }
        let re = ((r + self.re) * 0.5).sqrt();
        let im = ((r - self.re) * 0.5).sqrt();
        Self::new(re, if self.im < 0.0 { -im } else { im })
    }
}

impl Add for Complex {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.re + o.re, self.im + o.im)
    }
}
impl Sub for Complex {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.re - o.re, self.im - o.im)
    }
}
impl Sub<f64> for Complex {
    type Output = Self;
    fn sub(self, o: f64) -> Self {
        Self::new(self.re - o, self.im)
    }
}
impl Mul for Complex {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        Self::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}
impl Mul<f64> for Complex {
    type Output = Self;
    fn mul(self, o: f64) -> Self {
        Self::new(self.re * o, self.im * o)
    }
}
impl Div for Complex {
    type Output = Self;
    fn div(self, o: Self) -> Self {
        let d = o.re * o.re + o.im * o.im;
        Self::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqrt_neg() {
        let s = Complex::new(-4.0, 0.0).sqrt();
        assert!((s.re).abs() < 1e-12 && (s.im - 2.0).abs() < 1e-12);
        let z = Complex::new(3.0, -4.0).sqrt();
        assert!((z.re - 2.0).abs() < 1e-12 && (z.im + 1.0).abs() < 1e-12);
    }
}
