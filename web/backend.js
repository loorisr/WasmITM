// « Serveur » local : les routes de l'API HTTP de RustITM, exécutées dans le navigateur par
// un groupe de Web Workers WebAssembly. `handle(path, opts)` renvoie une `Response` comme
// `fetch`, avec les mêmes en-têtes et corps que le serveur natif.
import { getTiles } from './tiles.js';

const params = new URLSearchParams(location.search);
export const POOL_SIZE = Math.max(1, Math.min(+params.get('workers') || Math.min(navigator.hardwareConcurrency || 4, 8), 32));

// ---------------------------------------------------------------- groupe de workers

class Worker1 {
  constructor() {
    this.w = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    this.seq = 0;
    this.pending = new Map();
    this.w.onmessage = ({ data }) => {
      const p = this.pending.get(data.id);
      if (!p) return;
      this.pending.delete(data.id);
      data.ok ? p.resolve(data.result) : p.reject(apiError(data.error));
    };
    this.w.onerror = e => {
      const err = new Error(`worker : ${e.message || 'erreur'}`);
      for (const p of this.pending.values()) p.reject(err);
      this.pending.clear();
    };
  }
  send(msg, transfer = []) {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.w.postMessage({ ...msg, id }, transfer);
    });
  }
  call(fn, ...args) { return this.send({ op: 'call', fn, args }); }
  async tiles(list) {
    const missing = await this.send({ op: 'missing', tiles: list.map(t => ({ lat: t.lat, lon: t.lon })) });
    const want = new Set(missing.map(t => `${t.lat},${t.lon}`));
    // Les octets ne sont envoyés que pour les tuiles absentes du worker.
    return this.send({
      op: 'tiles',
      tiles: list.map(t => want.has(`${t.lat},${t.lon}`) ? t : { lat: t.lat, lon: t.lon, have: true }),
    });
  }
  terminate() {
    this.w.terminate();
    const err = new DOMException('Calcul annulé', 'AbortError');
    for (const p of this.pending.values()) p.reject(err);
    this.pending.clear();
  }
}

let pool = [];
function getPool() {
  if (!pool.length) pool = Array.from({ length: POOL_SIZE }, () => new Worker1());
  return pool;
}
function resetPool() {
  pool.forEach(w => w.terminate());
  pool = [];
}

// ---------------------------------------------------------------- erreurs et réponses

class ApiError extends Error {
  constructor(status, message, itm_code = null) {
    super(message);
    this.status = status;
    this.itm_code = itm_code;
  }
}

/** Erreur renvoyée par le wasm (`{"status","error","itm_code"}`) ou par JavaScript. */
function apiError(e) {
  if (e instanceof ApiError) return e;
  const s = typeof e === 'string' ? e : e?.message;
  try {
    const j = JSON.parse(s);
    if (j && j.status) return new ApiError(j.status, j.error, j.itm_code);
  } catch { }
  return new ApiError(500, s || String(e));
}

const STATUS_TEXT = { 200: 'OK', 400: 'Bad Request', 404: 'Not Found', 422: 'Unprocessable Entity', 500: 'Internal Server Error', 502: 'Bad Gateway' };

function jsonResponse(obj, status = 200, headers = {}) {
  return new Response(typeof obj === 'string' ? obj : JSON.stringify(obj), {
    status, statusText: STATUS_TEXT[status] || '', headers: { 'content-type': 'application/json', ...headers },
  });
}

function errorResponse(e) {
  const a = apiError(e);
  return jsonResponse({ error: a.message, itm_code: a.itm_code }, a.status);
}

/** Tuiles nécessaires (`[{lat, lon, name}]`) → chargées dans les workers indiqués. */
async function withTiles(keysJson, workers, onProgress) {
  const keys = JSON.parse(keysJson);
  let list;
  try {
    list = await getTiles(keys, onProgress);
  } catch (e) {
    throw new ApiError(502, `accès MNE : ${e.message}`);
  }
  await Promise.all(workers.map(w => w.tiles(list)));
}

// ---------------------------------------------------------------- routes

function readBody(opts) {
  return typeof opts.body === 'string' ? opts.body : '{}';
}

async function elevation(points) {
  const w = getPool()[0];
  const body = JSON.stringify({ points });
  await withTiles(await w.call('elevation_tiles', body), [w]);
  return JSON.parse(await w.call('elevation', body));
}

async function p2p(body) {
  const w = getPool()[0];
  await withTiles(await w.call('p2p_tiles', body), [w]);
  return w.call('p2p', body);
}

// Derniers résultats de couverture (pour l'export GeoTIFF sans recalcul).
const covCache = new Map();
const COV_CACHE_MAX = 4;

let covBusy = false;

async function coverage(bodyStr, signal, onProgress) {
  const req = JSON.parse(bodyStr);
  const format = req.format || 'png';
  delete req.format;
  const body = JSON.stringify(req);
  if (!['f32', 'geotiff'].includes(format)) {
    throw new ApiError(400, `format « ${format} » non disponible dans la version navigateur (f32 ou geotiff)`);
  }
  let res = covCache.get(body);
  if (!res) {
    if (covBusy) throw new ApiError(503, 'une couverture est déjà en cours de calcul');
    covBusy = true;
    const onAbort = () => resetPool();
    signal?.addEventListener('abort', onAbort);
    try {
      if (signal?.aborted) throw new DOMException('Calcul annulé', 'AbortError');
      res = await computeCoverage(body, onProgress);
    } finally {
      signal?.removeEventListener('abort', onAbort);
      covBusy = false;
    }
    covCache.set(body, res);
    while (covCache.size > COV_CACHE_MAX) covCache.delete(covCache.keys().next().value);
  }
  const m = res.meta, b = m.bounds;
  const headers = {
    'x-bounds': `${b.west},${b.south},${b.east},${b.north}`,
    'x-unit': m.unit,
    'x-coverage-stats': JSON.stringify(m.stats),
    'x-width': String(m.width),
    'x-height': String(m.height),
  };
  if (format === 'geotiff') {
    const tif = await getPool()[0].call('grid_geotiff', m.width, m.height, m.west, m.north, m.dlon, m.dlat, res.data.slice());
    return new Response(tif, { headers: { ...headers, 'content-type': 'image/tiff' } });
  }
  return new Response(res.data.slice().buffer, { headers: { ...headers, 'content-type': 'application/octet-stream' } });
}

async function computeCoverage(body, onProgress) {
  const t0 = performance.now();
  const workers = getPool();
  const w0 = workers[0];
  const keysJson = await w0.call('coverage_tiles', body); // validation + liste des tuiles
  onProgress?.({ phase: 'tiles', done: 0, total: JSON.parse(keysJson).length });
  await withTiles(keysJson, workers, (done, total, downloads) => onProgress?.({ phase: 'tiles', done, total, downloads }));

  // 1) Plan (un worker), puis masque des points utiles diffusé à tous.
  const plan = JSON.parse(await w0.call('coverage_plan', body));
  const needed = await w0.call('coverage_needed');
  await Promise.all(workers.map(w => w.send({ op: 'needed', needed })));

  // 2) Radiales, distribuées par blocs à la demande.
  const n = plan.radials, nk = plan.nk;
  const polar = new Float32Array(n * nk).fill(NaN);
  const chunk = Math.max(4, Math.ceil(n / (workers.length * 24)));
  let next = 0, done = 0, calls = 0, errors = 0;
  onProgress?.({ phase: 'itm', done: 0, total: n });
  await Promise.all(workers.map(async w => {
    while (next < n) {
      const j0 = next;
      next = Math.min(n, next + chunk);
      const r = await w.send({ op: 'radials', body, j0, j1: next });
      polar.set(r.values, j0 * nk);
      calls += r.calls;
      errors += r.errors;
      done += r.values.length / nk;
      onProgress?.({ phase: 'itm', done, total: n });
    }
  }));

  // 3) Report sur la grille (worker du plan).
  const data = await w0.call('coverage_finish', body, polar, calls, errors, performance.now() - t0);
  const meta = JSON.parse(await w0.call('coverage_meta'));
  return { data, meta };
}

/** Équivalent de `fetch(path, opts)` pour les routes de l'API. */
export async function handle(path, opts = {}) {
  const url = new URL(path, 'http://local/');
  const method = (opts.method || 'GET').toUpperCase();
  try {
    switch (`${method} ${url.pathname}`) {
      case 'GET /v1/info':
        return jsonResponse(await getPool()[0].call('info'));
      case 'GET /v1/elevation': {
        const lat = +url.searchParams.get('lat'), lon = +url.searchParams.get('lon');
        const j = await elevation([{ lat, lon }]);
        return jsonResponse(j.results[0]);
      }
      case 'POST /v1/elevation': {
        const b = JSON.parse(readBody(opts));
        return jsonResponse(await elevation(b.points || []));
      }
      case 'POST /v1/p2p':
        return jsonResponse(await p2p(readBody(opts)));
      case 'POST /v1/tile-key': // propre à la version navigateur : import de tuiles locales
        return jsonResponse(await getPool()[0].call('tile_key_of', new Uint8Array(opts.body)));
      case 'POST /v1/coverage':
        return await coverage(readBody(opts), opts.signal, opts.onProgress);
      default:
        return jsonResponse({ error: 'introuvable' }, 404);
    }
  } catch (e) {
    if (e?.name === 'AbortError') throw e;
    return errorResponse(e);
  }
}
