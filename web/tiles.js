// Tuiles Copernicus GLO-90 : téléchargement depuis le miroir OpenTopography (qui autorise
// le CORS, contrairement au bucket AWS), cache persistant (Cache API) et import de fichiers
// locaux. Une tuile absente du jeu Copernicus (océan) est renvoyée comme `null` (mer, 0 m).

export const TILE_BASE = 'https://opentopography.s3.sdsc.edu/raster/COP90/COP90_hh/';
const CACHE_NAME = 'glo90-v1';
const MEM_MAX = 32;

const mem = new Map(); // nom -> ArrayBuffer | null (ordre = utilisation)
const inflight = new Map(); // nom -> Promise

export const tileUrl = name => `${TILE_BASE}${name}.tif`;

async function openCache() {
  try { return 'caches' in self ? await caches.open(CACHE_NAME) : null; } catch { return null; }
}

function remember(name, buf) {
  mem.delete(name);
  mem.set(name, buf);
  while (mem.size > MEM_MAX) mem.delete(mem.keys().next().value);
}

async function load(name, onEvent) {
  const url = tileUrl(name);
  const cache = await openCache();
  const hit = cache && await cache.match(url);
  if (hit) {
    onEvent?.('cache', name);
    return hit.headers.get('x-sea') ? null : await hit.arrayBuffer();
  }
  onEvent?.('download', name);
  let r;
  try {
    r = await fetch(url);
  } catch (e) {
    throw new Error(`cannot download ${name} (${e.message})`);
  }
  if (r.status === 404 || r.status === 403) {
    await cache?.put(url, new Response('', { headers: { 'x-sea': '1' } }));
    return null;
  }
  if (!r.ok) throw new Error(`downloading ${name}: HTTP ${r.status}`);
  const buf = await r.arrayBuffer();
  const magic = new Uint8Array(buf, 0, 2);
  if (!((magic[0] === 0x49 && magic[1] === 0x49) || (magic[0] === 0x4d && magic[1] === 0x4d))) {
    throw new Error(`${name}: invalid downloaded file`);
  }
  await cache?.put(url, new Response(buf.slice(0), { headers: { 'content-type': 'image/tiff' } })).catch(() => { });
  return buf;
}

/** Tuile `name` : ArrayBuffer (non détaché, à copier avant transfert) ou null (mer). */
export function getTile(name, onEvent) {
  if (mem.has(name)) {
    const b = mem.get(name);
    remember(name, b);
    return Promise.resolve(b);
  }
  if (!inflight.has(name)) {
    inflight.set(name, load(name, onEvent)
      .then(b => { remember(name, b); return b; })
      .finally(() => inflight.delete(name)));
  }
  return inflight.get(name);
}

/** Charge une liste de tuiles (4 téléchargements en parallèle au plus). */
export async function getTiles(keys, onProgress) {
  const out = new Array(keys.length);
  let next = 0, done = 0, downloads = 0;
  const ev = kind => { if (kind === 'download') downloads++; };
  async function run() {
    while (next < keys.length) {
      const i = next++;
      out[i] = { lat: keys[i].lat, lon: keys[i].lon, bytes: await getTile(keys[i].name, ev) };
      done++;
      onProgress?.(done, keys.length, downloads);
    }
  }
  await Promise.all(Array.from({ length: Math.min(4, keys.length) }, run));
  return out;
}

/** Enregistre une tuile importée (fichier local) sous son nom OpenTopography. */
export async function importTile(name, buf) {
  remember(name, buf);
  const cache = await openCache();
  await cache?.put(tileUrl(name), new Response(buf.slice(0), { headers: { 'content-type': 'image/tiff' } }));
}

/** Nombre de tuiles et taille du cache persistant. */
export async function cacheInfo() {
  const cache = await openCache();
  if (!cache) return { tiles: 0, bytes: 0 };
  const reqs = await cache.keys();
  let bytes = 0, tiles = 0;
  for (const q of reqs) {
    const r = await cache.match(q);
    if (r && !r.headers.get('x-sea')) { tiles++; bytes += (await r.clone().arrayBuffer()).byteLength; }
  }
  return { tiles, bytes };
}

export async function clearCache() {
  mem.clear();
  if ('caches' in self) await caches.delete(CACHE_NAME);
}
