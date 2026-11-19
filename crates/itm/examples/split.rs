//! Micro-benchmark par étape d'un appel ITM, sur une radiale de 1080 points (100 km au pas
//! GLO-90), un appel par distance comme dans une couverture.
//! `cargo run --release -p itm --example split`
use std::hint::black_box;
use std::time::Instant;

fn per_call<F: FnMut(usize)>(n: usize, reps: usize, mut f: F) -> f64 {
    let t = Instant::now();
    for _ in 0..reps {
        for k in 2..=n {
            f(k);
        }
    }
    t.elapsed().as_secs_f64() * 1e6 / (reps * (n - 1)) as f64
}

fn main() {
    let n = 1080;
    let xi = 92.66;
    // Relief montagneux synthétique : grandes ondulations + rugosité.
    let elev: Vec<f64> = (0..=n)
        .map(|i| {
            let x = i as f64;
            800.0 + 600.0 * (x * 0.013).sin() + 250.0 * (x * 0.057).sin() + 80.0 * (x * 0.31).cos()
        })
        .collect();
    let p = itm::P2pParams {
        h_tx_m: 30.0,
        h_rx_m: 2.0,
        f_mhz: 145.0,
        ..Default::default()
    };
    let gamma_e = 157e-9 * (1.0 - 0.04665 * (301.0f64 / 179.3).exp());
    let h = [30.0, 2.0];
    let reps = 5;

    let mut acc = 0.0;
    let t = per_call(n, reps, |k| {
        acc += itm::p2p(&p, black_box(&elev[..=k]), xi).unwrap().loss_db
    });
    println!("p2p complet       {t:.3} µs/appel (checksum {acc:.6})");
    let t = per_call(n, reps, |k| {
        black_box(itm::core::find_horizons(
            black_box(&elev[..=k]),
            xi,
            1.0 / gamma_e,
            h,
        ));
    });
    println!("find_horizons     {t:.3} µs");
    let t = per_call(n, reps, |k| {
        let d = k as f64 * xi;
        black_box(itm::core::compute_delta_h(
            black_box(&elev[..=k]),
            xi,
            450.0,
            d - 30.0,
        ));
    });
    println!("compute_delta_h   {t:.3} µs");
    let t = per_call(n, reps, |k| {
        black_box(itm::core::quick_pfl(black_box(&elev[..=k]), xi, gamma_e, h));
    });
    println!("quick_pfl (total) {t:.3} µs");
}
