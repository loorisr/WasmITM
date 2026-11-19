# WasmITM

**Live: https://loorisr.github.io/WasmITM/**

Radio propagation with **ITM (Longley-Rice) v1.3** on the **Copernicus GLO-90** DEM, running
entirely in the browser (WebAssembly) with no server: a static site hosted on GitHub Pages.

WasmITM is the browser port of [RustITM](https://github.com/loorisr/RustITM) (from commit
`c3e7496`):
- the ITM model (`crates/itm`) is copied unchanged;
- the computation part of the RustITM server (DEM, profiles, coverage, fast mode) is adapted in `crates/itm-wasm`;
- the web UI is the same; it is in French.

What it offers:
- point-to-point links with a terrain profile;
- coverage maps on the native GLO-90 grid, in exact or fast mode (`fast`);
- GeoTIFF and PNG export, plus site import and export.

## How it works

```
index.html / app.js     UI (Leaflet), identical to RustITM except for the API calls
backend.js              /v1/info, /v1/elevation, /v1/p2p, /v1/coverage served locally
worker.js               one Web Worker = one wasm instance with its own tiles
tiles.js                tile download, browser cache, local file import
pkg/                    wasm module built by build.sh (crates/itm-wasm)
```

- **DEM tiles**
  - The AWS bucket `copernicus-dem-90m` used by RustITM does not allow CORS, so a browser cannot read it.
  - Tiles therefore come from the **OpenTopography** GLO-90 mirror (`opentopography.s3.sdsc.edu/raster/COP90/COP90_hh/`), which does.
  - They are kept in the browser cache (Cache API). Each tile is about 6 MB, and a 100 km radius coverage uses 8 of them.
  - A tile missing from the Copernicus set (open sea) counts as 0 m.
  - The *Radio* tab can import tiles already on disk (AWS or OpenTopography names) and clear the cache.
- **Parallelism**
  - A coverage is computed in three phases:
    1. **Plan.** One worker works out, for each pixel, the polar point (radial, step) whose value it takes.
    2. **ITM.** All the workers share the radials, taking blocks on demand. This is the expensive phase.
    3. **Assembly.** The first worker maps the polar values onto the grid.
  - WebAssembly threads are not used: they need COOP/COEP headers, which GitHub Pages cannot send.
  - The page starts `min(cores, 8)` workers by default; `?workers=N` in the URL overrides this. The cap exists because each worker holds its own copy of the tiles.
- **Same API as RustITM**
  - Requests are validated as in the server, and the JSON responses are the same.
  - Coverage `format` is `f32` or `geotiff`; `png` and `json` are not available.
  - The result does not depend on the number of workers: it is bit-identical.

## Comparison with the native server

Benchmark case: a 100 km coverage around 46.0° N 6.0° E. Settings and grid:
- 145 MHz, 30 m mast, 2 m receiver, `nearest` mode;
- native grid of 3109 × 2161 px, which takes 4.49 M ITM calls;
- the same OpenTopography tiles on both sides.

**Accuracy against the `itm-server` (RustITM) output:**
- **Coverage:** 15 of the 6.7 M pixels differ, by at most 1.5·10⁻⁵ dB (the last bit of an `f32`).
- **Point-to-point:** values differ at about 10⁻¹³ relative.
- **Cause:** the trigonometric functions of the WebAssembly runtime do not always round the last bit exactly like the native system library.

**Time** (idle 20-thread machine, best of 3 runs):

| Run | Exact | `fast: 50` |
|---|---:|---:|
| RustITM native, 1 thread | 12.4 s | 4.3 s |
| WasmITM, 1 worker (Node) | 16.7 s | 7.3 s |
| RustITM native, 20 threads | 0.76 s | 0.28 s |
| WasmITM in Chromium, 20 workers (`?workers=20`) | 1.41 s | 0.90 s |
| WasmITM in Chromium, 8 workers (default) | 2.49 s | 1.30 s |

Reading these numbers:
- **Single thread.** WebAssembly is 1.35× slower than native in exact mode and 1.7× slower in fast mode.
- **20 threads.** The gap widens, mostly in fast mode (3.2×). The ITM phase is then so short that the steps that do not run in parallel (plan, assembly, copies between workers, about 0.4 s in total) weigh more.
- **Browser rows.** They are measured after a warm-up run, so workers are started and the tiles are already in the cache.
- **First run on a page.** It also loads the tiles into each worker, which adds 0.5 to 1.5 s. It also downloads them if they are not cached yet: about 50 MB for 8 tiles.

## Building

Requirements:
- Rust stable with the `wasm32-unknown-unknown` target
- `wasm-bindgen-cli`, at the same version as the crate: `cargo install --locked wasm-bindgen-cli --version 0.2.129`
- optionally `wasm-opt` (binaryen)

```sh
./build.sh                               # builds crates/itm-wasm into web/pkg/
cargo test --release                     # ITM model and port tests
cd web && python3 -m http.server 8000    # then open http://localhost:8000/
```

ES modules do not load from `file://`, so serve `web/` over HTTP.

`scripts/node_run.mjs` runs an API request in Node (worker threads) from a folder of
OpenTopography tiles. It is used for the comparisons with the native server:

```sh
node scripts/node_run.mjs <tiles> coverage request.json out.f32 [workers]
node scripts/node_run.mjs <tiles> p2p request.json out.json
```

## Deployment

The workflow `.github/workflows/pages.yml` runs the tests, builds the wasm module and publishes
`web/` to GitHub Pages on every push to `main`.
- **Repository settings:** *Settings → Pages → Source* must be set to *GitHub Actions*.
- **Paths:** all paths are relative, so the site also works from a sub-folder or at the root of a domain.

## Licence and data

- **Code:** AGPL-3.0 (see `LICENSE`), like RustITM. Leaflet is BSD-2 (`web/vendor/leaflet/LICENSE`).
- **DEM:** Copernicus GLO-90. Credits:
  - © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018;
  - provided under the Copernicus programme by ESA and the European Union;
  - served here through OpenTopography.
- **Base maps:** © OpenStreetMap contributors, OpenTopoMap.
