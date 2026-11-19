// Exécute une requête de l'API dans Node avec le module wasm (même découpage que backend.js).
// Usage : node scripts/node_run.mjs <dossier de tuiles OpenTopography> <p2p|coverage> <requête.json> <sortie> [workers]
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const wasm = await import(path.join(root, 'web/pkg/itm_wasm.js'));
wasm.initSync({ module: readFileSync(path.join(root, 'web/pkg/itm_wasm_bg.wasm')) });

function loadTiles(dir, keysJson) {
  for (const k of JSON.parse(keysJson)) {
    const f = path.join(dir, `${k.name}.tif`);
    wasm.load_tile(k.lat, k.lon, existsSync(f) ? readFileSync(f) : undefined);
  }
}

if (!isMainThread) {
  const { dir, body, keys, needed } = workerData;
  loadTiles(dir, keys);
  const mask = new Uint8Array(needed);
  parentPort.on('message', ({ j0, j1 }) => {
    const values = wasm.coverage_radials(body, mask, j0, j1);
    const [calls, errors] = wasm.last_counts();
    parentPort.postMessage({ j0, values, calls, errors }, [values.buffer]);
  });
} else {
  const [dir, kind, reqFile, out, nw = '1'] = process.argv.slice(2);
  const body = readFileSync(reqFile, 'utf8');
  const t0 = performance.now();
  if (kind === 'p2p') {
    loadTiles(dir, wasm.p2p_tiles(body));
    writeFileSync(out, wasm.p2p(body));
  } else {
    const keys = wasm.coverage_tiles(body);
    loadTiles(dir, keys);
    const tTiles = performance.now();
    const plan = JSON.parse(wasm.coverage_plan(body));
    const needed = wasm.coverage_needed();
    const n = plan.radials, nk = plan.nk;
    const polar = new Float32Array(n * nk).fill(NaN);
    let calls = 0, errors = 0;
    const W = +nw;
    const workers = Array.from({ length: W }, () => new Worker(fileURLToPath(import.meta.url), { workerData: { dir, body, keys, needed } }));
    const chunk = Math.max(4, Math.ceil(n / (W * 24)));
    let next = 0;
    const tItm = performance.now();
    await Promise.all(workers.map(w => new Promise(resolve => {
      const go = () => {
        if (next >= n) { w.terminate(); return resolve(); }
        const j0 = next; next = Math.min(n, next + chunk);
        w.postMessage({ j0, j1: next });
      };
      w.on('message', r => { polar.set(r.values, r.j0 * nk); calls += r.calls; errors += r.errors; go(); });
      go();
    })));
    const tEnd = performance.now();
    const data = wasm.coverage_finish(body, polar, calls, errors, tEnd - t0);
    const meta = JSON.parse(wasm.coverage_meta());
    writeFileSync(out, Buffer.from(data.buffer, data.byteOffset, data.byteLength));
    writeFileSync(out + '.json', JSON.stringify(meta));
    console.error(`tuiles ${((tTiles - t0) / 1000).toFixed(2)} s, ITM ${((tEnd - tItm) / 1000).toFixed(2)} s (${W} workers), total ${((performance.now() - t0) / 1000).toFixed(2)} s, ${calls} appels`);
  }
}
