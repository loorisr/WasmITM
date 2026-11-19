//! Conformité avec les cas de test officiels NTIA (p2p.csv / pfls.csv du dépôt NTIA/itm).

use itm::{Climate, P2pParams, Polarization};

fn load() -> Vec<(P2pParams, Vec<f64>, f64, f64)> {
    let p2p = include_str!("data/p2p.csv");
    let pfls = include_str!("data/pfls.csv");
    p2p.lines()
        .skip(1)
        .zip(pfls.lines())
        .map(|(row, pfl)| {
            let v: Vec<f64> = row.split(',').map(|s| s.trim().parse().unwrap()).collect();
            let pf: Vec<f64> = pfl.split(',').map(|s| s.trim().parse().unwrap()).collect();
            let np = pf[0] as usize;
            let spacing = pf[1];
            let elev = pf[2..2 + np + 1].to_vec();
            let params = P2pParams {
                h_tx_m: v[0],
                h_rx_m: v[1],
                epsilon: v[2],
                sigma: v[3],
                n0: v[4],
                f_mhz: v[5],
                polarization: if v[6] as i32 == 1 {
                    Polarization::Vertical
                } else {
                    Polarization::Horizontal
                },
                climate: Climate::from_i32(v[7] as i32).unwrap(),
                time: v[8],
                location: v[9],
                situation: v[10],
                mdvar: v[11] as i32,
            };
            (params, elev, spacing, v[12])
        })
        .collect()
}

#[test]
fn ntia_reference_cases() {
    let cases = load();
    assert_eq!(cases.len(), 5);
    for (i, (p, elev, xi, expected)) in cases.iter().enumerate() {
        let r = itm::p2p(p, elev, *xi).expect("calcul ITM");
        println!(
            "cas {i}: {:.2} dB (attendu {:.2}), mode {:?}",
            r.loss_db, expected, r.intermediate.mode
        );
        assert!(
            (r.loss_db - expected).abs() < 0.01,
            "cas {i}: {} != {}",
            r.loss_db,
            expected
        );
    }
}

#[test]
fn rejects_bad_inputs() {
    let elev = vec![0.0; 100];
    let p = P2pParams {
        f_mhz: 5.0,
        ..Default::default()
    };
    assert_eq!(
        itm::p2p(&p, &elev, 100.0).unwrap_err(),
        itm::ItmError::Frequency
    );
    let p = P2pParams::default();
    assert_eq!(
        itm::p2p(&p, &elev[..1], 100.0).unwrap_err(),
        itm::ItmError::PathDistance
    );
}

#[test]
fn flat_earth_near_free_space_short_range() {
    // Terrain plat, 2 km, antennes hautes : visibilité directe, perte proche de l'espace libre.
    let elev = vec![100.0; 21];
    let p = P2pParams {
        h_tx_m: 30.0,
        h_rx_m: 10.0,
        f_mhz: 450.0,
        ..Default::default()
    };
    let r = itm::p2p(&p, &elev, 100.0).unwrap();
    assert_eq!(r.intermediate.mode, itm::PropMode::LineOfSight);
    let fs = itm::free_space_loss(2000.0, 450.0);
    assert!((r.loss_db - fs).abs() < 15.0, "{} vs fs {}", r.loss_db, fs);
}

#[test]
fn path_geometry_classification() {
    let p = P2pParams {
        h_tx_m: 10.0,
        h_rx_m: 10.0,
        f_mhz: 450.0,
        ..Default::default()
    };
    // Terrain plat 10 km : vue directe
    let flat = vec![100.0; 101];
    assert_eq!(
        itm::p2p(&p, &flat, 100.0)
            .unwrap()
            .intermediate
            .path_geometry,
        itm::PathGeometry::LineOfSight
    );
    // Une crête au milieu : un horizon commun
    let mut ridge = flat.clone();
    ridge[50] = 400.0;
    assert_eq!(
        itm::p2p(&p, &ridge, 100.0)
            .unwrap()
            .intermediate
            .path_geometry,
        itm::PathGeometry::SingleHorizon
    );
    // Deux crêtes : deux horizons
    let mut two = flat.clone();
    two[30] = 400.0;
    two[70] = 400.0;
    let r = itm::p2p(&p, &two, 100.0).unwrap();
    assert_eq!(
        r.intermediate.path_geometry,
        itm::PathGeometry::DoubleHorizon
    );
    assert!(r.loss_db > itm::p2p(&p, &flat, 100.0).unwrap().loss_db + 10.0);
}
