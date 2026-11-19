//! Micro-benchmark : radiale de 556 points, un appel ITM par distance (comme une couverture).
use std::time::Instant;
fn main() {
    let n = 556;
    let elev: Vec<f64> = (0..=n)
        .map(|i| 400.0 + 250.0 * ((i as f64) * 0.05).sin() + 80.0 * ((i as f64) * 0.31).cos())
        .collect();
    let p = itm::P2pParams {
        h_tx_m: 30.0,
        h_rx_m: 2.0,
        f_mhz: 446.0,
        ..Default::default()
    };
    let t = Instant::now();
    let mut acc = 0.0;
    let reps = 20;
    for _ in 0..reps {
        for k in 2..=n {
            acc += itm::p2p(&p, &elev[..=k], 90.0).unwrap().loss_db;
        }
    }
    let calls = reps * (n - 1);
    println!(
        "{:.2} µs/appel (checksum {acc:.3})",
        t.elapsed().as_secs_f64() * 1e6 / calls as f64
    );
}
