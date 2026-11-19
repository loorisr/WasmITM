// Web Worker : une instance WebAssembly d'itm-wasm, avec ses propres tuiles.
//
// Messages reçus : { id, op, ... } ; réponse : { id, ok: true, result } ou { id, ok: false, error }.
//   op 'call'      : appel d'une fonction exportée, { fn, args }
//   op 'tiles'     : charge les tuiles { tiles: [{lat, lon, bytes: ArrayBuffer|null}] } absentes
//   op 'needed'    : mémorise le masque de points polaires de la couverture en cours
//   op 'radials'   : { body, j0, j1 } → { values: Float32Array, calls, errors }
import init, * as wasm from './pkg/itm_wasm.js';

const ready = init({ module_or_path: new URL('./pkg/itm_wasm_bg.wasm', import.meta.url) });

// Tuiles chargées dans ce worker, de la plus ancienne à la plus récente utilisation.
const MAX_RESIDENT = 16;
const resident = new Map(); // "lat,lon" -> {lat, lon}
let needed = new Uint8Array(0);

function useTiles(list) {
  for (const t of list) {
    const k = `${t.lat},${t.lon}`;
    if (resident.has(k)) {
      const v = resident.get(k);
      resident.delete(k);
      resident.set(k, v);
      continue;
    }
    if (t.have) continue;
    wasm.load_tile(t.lat, t.lon, t.bytes ? new Uint8Array(t.bytes) : undefined);
    resident.set(k, { lat: t.lat, lon: t.lon });
  }
  const keep = new Set(list.map(t => `${t.lat},${t.lon}`));
  for (const [k, v] of resident) {
    if (resident.size <= MAX_RESIDENT) break;
    if (keep.has(k)) continue;
    wasm.unload_tile(v.lat, v.lon);
    resident.delete(k);
  }
}

function transferables(x) {
  return ArrayBuffer.isView(x) ? [x.buffer] : x && x.values && ArrayBuffer.isView(x.values) ? [x.values.buffer] : [];
}

self.onmessage = async ({ data: m }) => {
  try {
    await ready;
    let result;
    switch (m.op) {
      case 'call': result = wasm[m.fn](...(m.args || [])); break;
      case 'tiles': useTiles(m.tiles); result = true; break;
      case 'missing': result = m.tiles.filter(t => !resident.has(`${t.lat},${t.lon}`)); break;
      case 'needed': needed = new Uint8Array(m.needed); result = true; break;
      case 'radials': {
        const values = wasm.coverage_radials(m.body, needed, m.j0, m.j1);
        const [calls, errors] = wasm.last_counts();
        result = { values, calls, errors };
        break;
      }
      default: throw new Error(`unknown operation: ${m.op}`);
    }
    self.postMessage({ id: m.id, ok: true, result }, transferables(result));
  } catch (e) {
    self.postMessage({ id: m.id, ok: false, error: typeof e === 'string' ? e : (e && e.message) || String(e) });
  }
};
