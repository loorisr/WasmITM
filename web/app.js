/* ITM web UI — plain JavaScript (Leaflet only), no build step.
 * Browser version: the /v1/* routes are served locally by backend.js (WebAssembly). */
import { handle, POOL_SIZE } from './backend.js';
import { importTile, cacheInfo, clearCache } from './tiles.js';

(() => {
  // ---------------------------------------------------------------- utilitaires
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const STORE = 'itm-ui-v1';
  const fmt = (v, d = 1) => (v == null || !isFinite(v)) ? '—' : v.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d });
  const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const uid = () => Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  const num = (el) => parseFloat(el.value);

  // Palette identique à celle du serveur (faible → fort)
  const RAMP = [[0, [48, 18, 59]], [0.2, [70, 107, 227]], [0.4, [40, 187, 236]], [0.6, [100, 253, 106]], [0.8, [249, 189, 37]], [1, [205, 32, 8]]];
  const LUT = new Uint8ClampedArray(256 * 3);
  for (let k = 0; k < 256; k++) {
    const t = k / 255;
    let i = 0;
    while (i < RAMP.length - 2 && t > RAMP[i + 1][0]) i++;
    const [t0, c0] = RAMP[i], [t1, c1] = RAMP[i + 1];
    const f = (t - t0) / (t1 - t0);
    for (let c = 0; c < 3; c++) LUT[k * 3 + c] = c0[c] + (c1[c] - c0[c]) * f;
  }
  const GRADIENT = 'linear-gradient(to right,' + RAMP.map(([t, c]) => `rgb(${c}) ${t * 100}%`).join(',') + ')';

  const SITE_COLORS = ['#2563eb', '#dc2626', '#16a34a', '#9333ea', '#ea580c', '#0891b2', '#be185d', '#4d7c0f'];
  const BANDS = [
    ['FM broadcast (98 MHz)', 98, 'horizontal'], ['Aviation VHF (125 MHz)', 125], ['Amateur 2 m (145 MHz)', 145],
    ['Marine VHF (156.8 MHz)', 156.8], ['Professional VHF (160 MHz)', 160], ['TETRA (390 MHz)', 390],
    ['Amateur 70 cm (433 MHz)', 433], ['PMR446 (446.1 MHz)', 446.1], ['DVB-T (600 MHz)', 600, 'horizontal'],
    ['LTE 800 (806 MHz)', 806], ['LoRa EU868 (868 MHz)', 868], ['LoRa US915 (915 MHz)', 915], ['GSM 900 (942 MHz)', 942],
    ['GSM / LTE 1800 (1842 MHz)', 1842], ['UMTS / LTE 2100 (2140 MHz)', 2140], ['Wi-Fi 2.4 GHz (2437 MHz)', 2437],
    ['LTE 2600 (2655 MHz)', 2655], ['Wi-Fi 5 GHz (5500 MHz)', 5500],
  ];
  const DEFAULT_RADIO = {
    frequency_mhz: 446.1, polarization: 'vertical', climate: 'continental_temperate', epsilon: 15, sigma: 0.005,
    n0: 301, mdvar: 12, time: 50, location: 50, situation: 50, rx_gain_dbi: 0, default_eirp_dbm: 30,
  };
  const QUANTITIES = {
    rx_power: { label: 'Received power', unit: 'dBm', weak: -120, strong: -40 },
    field_strength: { label: 'Field strength', unit: 'dBµV/m', weak: 20, strong: 100 },
    path_loss: { label: 'Path loss', unit: 'dB', weak: 180, strong: 80 },
  };
  const MODE_LABEL = { line_of_sight: 'Line of sight (ITM curve)', diffraction: 'Diffraction', troposcatter: 'Troposcatter', not_set: '—' };
  const GEOM = { line_of_sight: ['Line of sight', 'ok'], single_horizon: ['One obstacle', 'warn'], double_horizon: ['Two horizons', 'bad'] };

  // ---------------------------------------------------------------- état persistant
  let state;
  try { state = JSON.parse(localStorage.getItem(STORE)) || {}; } catch { state = {}; }
  state.sites ??= [];
  state.radio = { ...DEFAULT_RADIO, ...(state.radio || {}) };
  state.view ??= { center: [46.6, 2.4], zoom: 6 };
  const save = () => { try { localStorage.setItem(STORE, JSON.stringify(state)); } catch { /* stockage indisponible */ } };

  let info = { limits: { max_radius_km: 150, max_grid_side: 6001, max_radials: 7200 } };
  let mode = null;            // null | 'add-site' | 'pick-rx'
  let selectedSite = null;
  let rxPoint = null;         // {lat, lon}
  const layers = [];          // couvertures calculées
  const markers = new Map();  // id de site -> marqueur Leaflet

  // ---------------------------------------------------------------- API
  async function api(path, opts = {}) {
    const r = await handle(path, { headers: { 'content-type': 'application/json' }, ...opts });
    if (!r.ok) {
      let msg = `${r.status} ${r.statusText}`;
      try { const t = await r.text(); try { msg = JSON.parse(t).error || t; } catch { msg = t || msg; } } catch { }
      throw new Error(msg);
    }
    return r;
  }
  function radioParams() {
    const p = state.radio;
    return {
      frequency_mhz: p.frequency_mhz, polarization: p.polarization, climate: p.climate, n0: p.n0,
      epsilon: p.epsilon, sigma: p.sigma, mdvar: +p.mdvar, time: p.time, location: p.location, situation: p.situation,
      rx_gain_dbi: p.rx_gain_dbi,
    };
  }

  function toast(msg, kind = '') {
    const el = document.createElement('div');
    el.className = 'toast ' + kind;
    el.textContent = msg;
    $('#toasts').append(el);
    setTimeout(() => el.remove(), kind === 'error' ? 9000 : 5000);
  }
  function download(blob, name) {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }

  // ---------------------------------------------------------------- carte
  const map = L.map('map', { zoomControl: true }).setView(state.view.center, state.view.zoom);
  const bases = {
    'OpenStreetMap': L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 19, attribution: '© OpenStreetMap contributors' }),
    'Terrain (OpenTopoMap)': L.tileLayer('https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png', { maxZoom: 17, attribution: '© OpenStreetMap, SRTM · style © OpenTopoMap (CC-BY-SA)' }),
  };
  bases['OpenStreetMap'].addTo(map);
  L.control.layers(bases, null, { position: 'topright' }).addTo(map);
  L.control.scale({ imperial: false }).addTo(map);
  map.attributionControl.addAttribution('DEM Copernicus GLO-90 © DLR/Airbus, ESA/EU, via <a href="https://opentopography.org">OpenTopography</a>');
  map.on('moveend', () => { const c = map.getCenter(); state.view = { center: [c.lat, c.lng], zoom: map.getZoom() }; save(); });

  function setMode(m) {
    mode = m;
    const b = $('#mode-banner');
    b.hidden = !m;
    b.textContent = m === 'add-site' ? 'Click on the map to place the site — Esc to cancel'
      : m === 'pick-rx' ? 'Click on the map to place the receiver — Esc to cancel' : '';
    map.getContainer().classList.toggle('picking', !!m);
  }
  document.addEventListener('keydown', e => { if (e.key === 'Escape') setMode(null); });

  map.on('click', e => {
    const p = { lat: e.latlng.lat, lon: e.latlng.lng };
    if (mode === 'add-site') { setMode(null); addSite(p); return; }
    if (mode === 'pick-rx') { setMode(null); setRxPoint(p); computeLink(); return; }
    if (e.originalEvent.shiftKey) {
      if (!state.sites.length) { toast('Add a transmitter site first.'); return; }
      showTab('link'); setRxMode('point'); setRxPoint(p); computeLink();
    }
  });

  // ---------------------------------------------------------------- onglets
  function showTab(name) {
    $$('#tabs button').forEach(b => b.classList.toggle('active', b.dataset.tab === name));
    $$('.tab').forEach(s => { s.hidden = s.id !== 'tab-' + name; });
  }
  $$('#tabs button').forEach(b => b.addEventListener('click', () => showTab(b.dataset.tab)));

  // ---------------------------------------------------------------- sites
  function siteIcon(s) {
    return L.divIcon({ className: '', html: `<div class="site-icon${s.id === selectedSite ? ' sel' : ''}" style="background:${s.color}"></div>`, iconSize: [18, 18], iconAnchor: [9, 9] });
  }
  function syncMarker(s) {
    let m = markers.get(s.id);
    if (!m) {
      m = L.marker([s.lat, s.lon], { draggable: true, icon: siteIcon(s), zIndexOffset: 1000 }).addTo(map);
      m.bindTooltip(esc(s.name), { permanent: true, direction: 'right', offset: [10, 0], className: 'site-label' });
      m.on('dragend', () => {
        const ll = m.getLatLng();
        Object.assign(s, { lat: ll.lat, lon: ll.lng });
        save(); renderSites();
      });
      m.on('click', () => { selectSite(s.id); showTab('sites'); });
      markers.set(s.id, m);
    }
    m.setLatLng([s.lat, s.lon]);
    m.setIcon(siteIcon(s));
    m.setTooltipContent(esc(s.name));
  }
  function addSite(p, extra = {}) {
    const n = state.sites.length;
    const s = { id: uid(), name: `Site ${n + 1}`, lat: p.lat, lon: p.lon, height_m: 20, eirp_dbm: state.radio.default_eirp_dbm, color: SITE_COLORS[n % SITE_COLORS.length], ...extra };
    state.sites.push(s);
    save(); selectSite(s.id);
    toast(`${s.name} added.`, 'ok');
  }
  function selectSite(id) {
    selectedSite = id;
    state.sites.forEach(syncMarker);
    renderSites();
    const el = $(`#site-list li[data-id="${id}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }
  function deleteSite(id) {
    const s = state.sites.find(x => x.id === id);
    if (!s || !confirm(`Delete “${s.name}”?`)) return;
    state.sites = state.sites.filter(x => x.id !== id);
    markers.get(id)?.remove(); markers.delete(id);
    save(); renderSites();
  }
  function renderSites() {
    const ul = $('#site-list');
    if (!state.sites.length) {
      ul.innerHTML = '<li class="empty">No sites. Add a transmitter on the map.</li>';
    } else {
      ul.innerHTML = state.sites.map(s => `
        <li data-id="${s.id}" class="${s.id === selectedSite ? 'selected' : ''}">
          <div class="card-head"><span class="swatch" style="background:${s.color}"></span>
            <input class="name" data-f="name" value="${esc(s.name)}" aria-label="Site name"></div>
          <div class="mono muted">${s.lat.toFixed(5)}, ${s.lon.toFixed(5)}</div>
          <div class="card-fields">
            <div><label>Height (m)</label><input type="number" data-f="height_m" value="${s.height_m}" min="0.5" max="3000" step="0.5"></div>
            <div><label>EIRP (dBm)</label><input type="number" data-f="eirp_dbm" value="${s.eirp_dbm}" step="0.1"></div>
            <div><label>Colour</label><input type="color" data-f="color" value="${s.color}"></div>
          </div>
          <div class="card-actions small">
            <button data-a="center">Centre</button><button data-a="link">Link</button>
            <button data-a="coverage">Coverage</button><button data-a="delete" class="danger">Delete</button>
          </div>
        </li>`).join('');
    }
    for (const sel of ['#link-tx', '#link-rx-sitesel', '#cov-site']) {
      const el = $(sel), prev = el.value;
      el.innerHTML = state.sites.map(s => `<option value="${s.id}">${esc(s.name)}</option>`).join('') || '<option value="">(no site)</option>';
      if (state.sites.some(s => s.id === prev)) el.value = prev;
      else if (selectedSite && sel !== '#link-rx-sitesel') el.value = selectedSite;
    }
    updateEstimate();
  }
  $('#site-list').addEventListener('change', e => {
    const li = e.target.closest('li[data-id]'), f = e.target.dataset.f;
    if (!li || !f) return;
    const s = state.sites.find(x => x.id === li.dataset.id);
    let v = e.target.value;
    if (f === 'height_m' || f === 'eirp_dbm') { v = parseFloat(v); if (!isFinite(v)) return; }
    s[f] = v;
    save(); syncMarker(s); renderSites();
  });
  $('#site-list').addEventListener('click', e => {
    const li = e.target.closest('li[data-id]');
    if (!li) return;
    const id = li.dataset.id, a = e.target.dataset.a, s = state.sites.find(x => x.id === id);
    if (!a) { if (selectedSite !== id && e.target.tagName !== 'INPUT') selectSite(id); return; }
    if (a === 'center') map.setView([s.lat, s.lon], Math.max(map.getZoom(), 12));
    if (a === 'delete') deleteSite(id);
    if (a === 'link') { $('#link-tx').value = id; showTab('link'); }
    if (a === 'coverage') { $('#cov-site').value = id; showTab('coverage'); updateEstimate(); }
  });
  $('#btn-add-site').addEventListener('click', () => setMode(mode === 'add-site' ? null : 'add-site'));
  $('#btn-export-sites').addEventListener('click', () => {
    download(new Blob([JSON.stringify({ format: 'itm-sites', version: 1, sites: state.sites, radio: state.radio }, null, 2)], { type: 'application/json' }), 'itm-sites.json');
  });
  $('#file-import').addEventListener('change', async e => {
    const f = e.target.files[0];
    e.target.value = '';
    if (!f) return;
    try {
      const j = JSON.parse(await f.text());
      const list = Array.isArray(j) ? j : j.sites;
      if (!Array.isArray(list)) throw new Error('file has no "sites" list');
      let n = 0;
      for (const s of list) {
        if (!isFinite(s.lat) || !isFinite(s.lon)) continue;
        state.sites.push({
          id: uid(), name: String(s.name || `Site ${state.sites.length + 1}`), lat: +s.lat, lon: +s.lon,
          height_m: isFinite(s.height_m) ? +s.height_m : 20, eirp_dbm: isFinite(s.eirp_dbm) ? +s.eirp_dbm : state.radio.default_eirp_dbm,
          color: /^#[0-9a-f]{6}$/i.test(s.color) ? s.color : SITE_COLORS[state.sites.length % SITE_COLORS.length],
        });
        n++;
      }
      if (j.radio && confirm('The file also contains radio parameters. Apply them?')) { state.radio = { ...DEFAULT_RADIO, ...j.radio }; loadRadioForm(); }
      save(); state.sites.forEach(syncMarker); renderSites();
      toast(`${n} site(s) imported.`, 'ok');
      if (n) map.fitBounds(L.latLngBounds(state.sites.map(s => [s.lat, s.lon])).pad(0.2));
    } catch (err) { toast('Import failed: ' + err.message, 'error'); }
  });

  // ---------------------------------------------------------------- liaison
  let rxMode = 'point', rxMarker = null, linkLine = null, profileCursor = null, profile = null;
  function setRxMode(m) {
    rxMode = m;
    $$('#link-rx-mode button').forEach(b => b.classList.toggle('active', b.dataset.v === m));
    $('#link-rx-point').hidden = m !== 'point';
    $('#link-rx-site').hidden = m !== 'site';
  }
  $$('#link-rx-mode button').forEach(b => b.addEventListener('click', () => setRxMode(b.dataset.v)));
  function setRxPoint(p) {
    rxPoint = p;
    $('#rx-coords').textContent = `${p.lat.toFixed(5)}, ${p.lon.toFixed(5)}`;
    const icon = L.divIcon({ className: '', html: '<div class="rx-icon"></div>', iconSize: [12, 12], iconAnchor: [6, 6] });
    if (!rxMarker) {
      rxMarker = L.marker([p.lat, p.lon], { icon, draggable: true }).addTo(map);
      rxMarker.on('dragend', () => { const ll = rxMarker.getLatLng(); setRxPoint({ lat: ll.lat, lon: ll.lng }); computeLink(); });
    } else rxMarker.setLatLng([p.lat, p.lon]);
  }
  $('#btn-pick-rx').addEventListener('click', () => setMode('pick-rx'));

  async function computeLink() {
    const tx = state.sites.find(s => s.id === $('#link-tx').value);
    if (!tx) { toast('Choose a transmitter site (Sites tab).', 'error'); return; }
    let rx;
    if (rxMode === 'site') {
      const s = state.sites.find(x => x.id === $('#link-rx-sitesel').value);
      if (!s || s.id === tx.id) { toast('Choose a receiver other than the transmitter.', 'error'); return; }
      rx = { lat: s.lat, lon: s.lon };
    } else {
      if (!rxPoint) { setMode('pick-rx'); return; }
      rx = rxPoint;
    }
    const hr = num($('#link-rx-h'));
    const body = { ...radioParams(), tx: { lat: tx.lat, lon: tx.lon, height_m: tx.height_m }, rx: { ...rx, height_m: hr }, eirp_dbm: tx.eirp_dbm, include_profile: true };
    const btn = $('#btn-link');
    btn.disabled = true;
    btn.innerHTML = '<span class="spinner"></span> Computing…';
    try {
      const j = await (await api('/v1/p2p', { method: 'POST', body: JSON.stringify(body) })).json();
      linkLine?.remove();
      linkLine = L.polyline(greatCircle(tx, rx, 64), { color: tx.color, weight: 3, opacity: 0.85, dashArray: '6 4' }).addTo(map);
      profile = { elev: j.profile.elevations_m, step: j.profile.step_m, ht: tx.height_m, hr, f: state.radio.frequency_mhz, tx, rx };
      const clr = fresnelClearance(profile);
      renderLinkResult(j, tx, clr);
      openProfile();
    } catch (err) {
      toast('Link: ' + err.message, 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = 'Compute the link';
    }
  }
  $('#btn-link').addEventListener('click', computeLink);

  function renderLinkResult(j, tx, clr) {
    const [gLabel, gCls] = GEOM[j.path_geometry] || [j.path_geometry, ''];
    const fCls = clr.min >= 0.6 ? 'ok' : clr.min >= 0 ? 'warn' : 'bad';
    const head = j.rx_power_dbm != null
      ? `<div class="muted">Received power</div><div class="v">${fmt(j.rx_power_dbm)} dBm</div><div class="muted">EIRP ${fmt(tx.eirp_dbm)} dBm · RX gain ${fmt(state.radio.rx_gain_dbi)} dBi</div>`
      : `<div class="muted">Path loss</div><div class="v">${fmt(j.path_loss_db)} dB</div>`;
    $('#link-result').innerHTML = `
      <div class="result">
        <div class="big">${head}</div>
        <table>
          <tr><td>Distance</td><td>${fmt(j.distance_km, 2)} km</td></tr>
          <tr><td>Bearing</td><td>${fmt(j.bearing_deg)}°</td></tr>
          <tr><td>ITM path loss</td><td>${fmt(j.path_loss_db)} dB</td></tr>
          <tr><td>Free space</td><td>${fmt(j.free_space_loss_db)} dB</td></tr>
          <tr><td>Excess loss</td><td>${fmt(j.excess_loss_db)} dB</td></tr>
          <tr><td>Field strength</td><td>${fmt(j.field_strength_dbuvm)} dBµV/m</td></tr>
          <tr><td>Path geometry</td><td><span class="badge ${gCls}">${gLabel}</span></td></tr>
          <tr><td>Min. Fresnel clearance</td><td><span class="badge ${fCls}">${clr.min < 0 ? 'Obstructed' : fmt(clr.min * 100, 0) + ' % F1'}</span></td></tr>
          <tr><td>ITM mode</td><td>${MODE_LABEL[j.mode] || j.mode}</td></tr>
          <tr><td>TX / RX ground elevation</td><td>${fmt(j.tx_ground_m, 0)} / ${fmt(j.rx_ground_m, 0)} m</td></tr>
          <tr><td>Terrain irregularity Δh</td><td>${fmt(j.intermediate.delta_h_m, 0)} m</td></tr>
        </table>
        ${j.warnings.messages.length ? `<div class="warnings">⚠ ${j.warnings.messages.map(esc).join('<br>⚠ ')}</div>` : ''}
      </div>`;
  }

  // Grand cercle (pour tracer la liaison et positionner le curseur du profil)
  function interp(a, b, f) {
    const r = Math.PI / 180, p1 = a.lat * r, l1 = a.lon * r, p2 = b.lat * r, l2 = b.lon * r;
    const d = 2 * Math.asin(Math.sqrt(Math.sin((p2 - p1) / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin((l2 - l1) / 2) ** 2));
    if (d < 1e-12) return [a.lat, a.lon];
    const A = Math.sin((1 - f) * d) / Math.sin(d), B = Math.sin(f * d) / Math.sin(d);
    const x = A * Math.cos(p1) * Math.cos(l1) + B * Math.cos(p2) * Math.cos(l2);
    const y = A * Math.cos(p1) * Math.sin(l1) + B * Math.cos(p2) * Math.sin(l2);
    const z = A * Math.sin(p1) + B * Math.sin(p2);
    return [Math.atan2(z, Math.hypot(x, y)) / r, Math.atan2(y, x) / r];
  }
  const greatCircle = (a, b, n) => Array.from({ length: n + 1 }, (_, i) => interp(a, b, i / n));

  // ---------------------------------------------------------------- profil de terrain
  const RE_EFF = 6371000 * 4 / 3;
  function profileSeries(p) {
    const n = p.elev.length - 1, D = n * p.step, lambda = 299.792458 / p.f;
    const z0 = p.elev[0] + p.ht, z1 = p.elev[n] + p.hr;
    const xs = [], ter = [], los = [], r1 = [];
    for (let i = 0; i <= n; i++) {
      const x = i * p.step;
      xs.push(x);
      ter.push(p.elev[i] + x * (D - x) / (2 * RE_EFF));
      los.push(z0 + (z1 - z0) * x / D);
      r1.push(Math.sqrt(lambda * x * (D - x) / D));
    }
    return { xs, ter, los, r1, D, n };
  }
  function fresnelClearance(p) {
    const s = profileSeries(p);
    let min = Infinity, at = 0;
    for (let i = 1; i < s.n; i++) {
      if (s.r1[i] <= 0) continue;
      const c = (s.los[i] - s.ter[i]) / s.r1[i];
      if (c < min) { min = c; at = s.xs[i]; }
    }
    return { min: isFinite(min) ? min : 1, at };
  }
  function openProfile() {
    $('#profile-panel').hidden = false;
    $('#mapwrap').classList.add('with-profile');
    map.invalidateSize();
    drawProfile();
  }
  $('#btn-profile-close').addEventListener('click', () => {
    $('#profile-panel').hidden = true;
    $('#mapwrap').classList.remove('with-profile');
    profileCursor?.remove(); profileCursor = null;
    map.invalidateSize();
  });
  function niceStep(range, target) {
    const raw = range / target, p = 10 ** Math.floor(Math.log10(raw)), m = raw / p;
    return (m < 1.5 ? 1 : m < 3 ? 2 : m < 7 ? 5 : 10) * p;
  }
  function drawProfile(hoverX) {
    if (!profile) return;
    const cv = $('#profile'), dpr = window.devicePixelRatio || 1;
    const W = cv.clientWidth, H = cv.clientHeight;
    if (!W || !H) return;
    cv.width = W * dpr; cv.height = H * dpr;
    const ctx = cv.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const s = profileSeries(profile);
    const pad = { l: 52, r: 14, t: 10, b: 26 };
    let ymin = Infinity, ymax = -Infinity;
    for (let i = 0; i <= s.n; i++) {
      ymin = Math.min(ymin, s.ter[i], s.los[i] - s.r1[i]);
      ymax = Math.max(ymax, s.ter[i], s.los[i] + s.r1[i]);
    }
    const span = Math.max(ymax - ymin, 10);
    ymin -= span * 0.08; ymax += span * 0.08;
    const X = x => pad.l + (x / s.D) * (W - pad.l - pad.r);
    const Y = y => pad.t + (1 - (y - ymin) / (ymax - ymin)) * (H - pad.t - pad.b);
    // grille
    ctx.font = '11px system-ui, sans-serif'; ctx.fillStyle = '#6b7280'; ctx.strokeStyle = '#eef1f4'; ctx.lineWidth = 1;
    const ys = niceStep(ymax - ymin, 5);
    for (let y = Math.ceil(ymin / ys) * ys; y <= ymax; y += ys) {
      ctx.beginPath(); ctx.moveTo(pad.l, Y(y)); ctx.lineTo(W - pad.r, Y(y)); ctx.stroke();
      ctx.textAlign = 'right'; ctx.fillText(`${Math.round(y)} m`, pad.l - 6, Y(y) + 4);
    }
    const xsStep = niceStep(s.D / 1000, 8);
    for (let x = 0; x <= s.D / 1000 + 1e-9; x += xsStep) {
      ctx.beginPath(); ctx.moveTo(X(x * 1000), pad.t); ctx.lineTo(X(x * 1000), H - pad.b); ctx.stroke();
      ctx.textAlign = 'center'; ctx.fillText(`${+x.toFixed(2)} km`, X(x * 1000), H - 8);
    }
    // zone de Fresnel
    ctx.beginPath();
    for (let i = 0; i <= s.n; i++) ctx[i ? 'lineTo' : 'moveTo'](X(s.xs[i]), Y(s.los[i] + s.r1[i]));
    for (let i = s.n; i >= 0; i--) ctx.lineTo(X(s.xs[i]), Y(s.los[i] - s.r1[i]));
    ctx.closePath(); ctx.fillStyle = 'rgba(37,99,235,.10)'; ctx.fill();
    ctx.strokeStyle = 'rgba(37,99,235,.45)'; ctx.setLineDash([4, 3]); ctx.stroke(); ctx.setLineDash([]);
    // terrain
    ctx.beginPath(); ctx.moveTo(X(0), H - pad.b);
    for (let i = 0; i <= s.n; i++) ctx.lineTo(X(s.xs[i]), Y(s.ter[i]));
    ctx.lineTo(X(s.D), H - pad.b); ctx.closePath();
    ctx.fillStyle = '#c8b99a'; ctx.fill(); ctx.strokeStyle = '#8a7650'; ctx.stroke();
    // obstructions (terrain dans 60 % de F1)
    ctx.fillStyle = 'rgba(220,38,38,.75)';
    for (let i = 1; i < s.n; i++) {
      if (s.ter[i] > s.los[i] - 0.6 * s.r1[i]) ctx.fillRect(X(s.xs[i]) - 1, Y(s.ter[i]) - 1, Math.max(2, X(profile.step) - X(0)), 3);
    }
    // ligne de visée et antennes
    ctx.strokeStyle = '#1d4ed8'; ctx.lineWidth = 1.5;
    ctx.beginPath(); ctx.moveTo(X(0), Y(s.los[0])); ctx.lineTo(X(s.D), Y(s.los[s.n])); ctx.stroke();
    ctx.strokeStyle = '#111'; ctx.lineWidth = 2;
    ctx.beginPath(); ctx.moveTo(X(0), Y(s.ter[0])); ctx.lineTo(X(0), Y(s.los[0])); ctx.moveTo(X(s.D), Y(s.ter[s.n])); ctx.lineTo(X(s.D), Y(s.los[s.n])); ctx.stroke();
    // curseur
    if (hoverX != null) {
      const i = Math.max(0, Math.min(s.n, Math.round(hoverX / profile.step)));
      const x = X(s.xs[i]);
      ctx.strokeStyle = 'rgba(0,0,0,.4)'; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(x, pad.t); ctx.lineTo(x, H - pad.b); ctx.stroke();
      const clr = s.r1[i] > 0 ? (s.los[i] - s.ter[i]) / s.r1[i] * 100 : null;
      const lines = [`${fmt(s.xs[i] / 1000, 2)} km`, `ground ${fmt(profile.elev[i], 0)} m`, `LOS ${fmt(s.los[i], 0)} m`, clr == null ? '' : `F1 ${fmt(clr, 0)} %`].filter(Boolean);
      const bw = 112, bh = lines.length * 15 + 8, bx = x + 8 + bw > W ? x - bw - 8 : x + 8;
      ctx.fillStyle = 'rgba(255,255,255,.95)'; ctx.strokeStyle = '#dfe3e8';
      ctx.fillRect(bx, pad.t + 4, bw, bh); ctx.strokeRect(bx, pad.t + 4, bw, bh);
      ctx.fillStyle = '#1d2430'; ctx.textAlign = 'left';
      lines.forEach((l, k) => ctx.fillText(l, bx + 6, pad.t + 18 + k * 15));
      const ll = interp(profile.tx, profile.rx, s.xs[i] / s.D);
      if (!profileCursor) profileCursor = L.circleMarker(ll, { radius: 5, color: '#111', weight: 2, fillColor: '#fff', fillOpacity: 1 }).addTo(map);
      else profileCursor.setLatLng(ll);
    }
  }
  $('#profile').addEventListener('mousemove', e => {
    if (!profile) return;
    const r = e.currentTarget.getBoundingClientRect();
    const D = (profile.elev.length - 1) * profile.step;
    const f = (e.clientX - r.left - 52) / (r.width - 52 - 14);
    drawProfile(Math.max(0, Math.min(1, f)) * D);
  });
  $('#profile').addEventListener('mouseleave', () => { drawProfile(); profileCursor?.remove(); profileCursor = null; });
  new ResizeObserver(() => drawProfile()).observe($('#profile'));

  // ---------------------------------------------------------------- couverture
  let covAbort = null;
  // Même géométrie que le serveur : grille calée sur les pixels GLO-90 (3" en latitude).
  const M_PER_DEG = 6371008.8 * Math.PI / 180;
  function glo90PixelDeg(lat) {
    const a = Math.abs(lat);
    const cols = a < 50 ? 1200 : a < 60 ? 800 : a < 70 ? 600 : a < 80 ? 400 : a < 85 ? 240 : 120;
    return [1 / 1200, 1 / cols];
  }
  function gridGeometry(lat, R, ds) {
    const [dlat0, dlon0] = glo90PixelDeg(lat);
    const dy = dlat0 * ds * M_PER_DEG, dx = dlon0 * ds * M_PER_DEG * Math.max(Math.cos(lat * Math.PI / 180), 0.01);
    const nLat = Math.ceil(R / dy), nLon = Math.ceil(R / dx);
    const radials = Math.min(Math.max(Math.ceil(2 * Math.PI * R / Math.min(dx, dy)), 8), info.limits.max_radials || 7200);
    return { w: 2 * nLon + 1, h: 2 * nLat + 1, dx, dy, radials, steps: nLat };
  }
  function updateEstimate() {
    const R = num($('#cov-radius')) * 1000, ds = +$('#cov-ds').value, lim = info.limits;
    const site = state.sites.find(s => s.id === $('#cov-site').value);
    const el = $('#cov-estimate'), btn = $('#btn-coverage');
    let err = '';
    if (!site) err = 'Add a site first.';
    else if (!(R > 0) || R / 1000 > lim.max_radius_km) err = `Radius: 1 to ${lim.max_radius_km} km.`;
    const g = site && !err ? gridGeometry(site.lat, R, ds) : null;
    if (!err && Math.max(g.w, g.h) > lim.max_grid_side) err = `Grid too large (${g.w} × ${g.h} px, max ${lim.max_grid_side}). Reduce the radius or pick a preview grid.`;
    if (err) { el.textContent = err; el.style.color = 'var(--danger)'; btn.disabled = true; return; }
    // Bilinéaire : ITM à tous les pas de chaque radiale. Par pixel : au plus un appel par
    // pixel du disque (un peu moins en bordure, où les radiales sont plus serrées que les pixels).
    const allSteps = g.radials * (g.steps - 1);
    const calls = $('#cov-interp').value === 'bilinear' ? allSteps : Math.min(allSteps, Math.PI * R * R / (g.dx * g.dy));
    el.style.color = '';
    el.textContent = `Pixel ${fmt(g.dy, 0)} × ${fmt(g.dx, 0)} m · grid ${g.w} × ${g.h} px · ${g.radials} radials · ~${calls >= 1e6 ? fmt(calls / 1e6, 1) + ' M' : fmt(calls / 1e3, 0) + ' k'} ITM calls`;
    btn.disabled = !!covAbort;
  }
  ['#cov-radius', '#cov-ds', '#cov-site', '#cov-interp'].forEach(s => $(s).addEventListener('input', updateEstimate));

  const mercY = lat => Math.log(Math.tan(Math.PI / 4 + lat * Math.PI / 360));
  const invMercY = y => (2 * Math.atan(Math.exp(y)) - Math.PI / 2) * 180 / Math.PI;

  // Colorise la grille (EPSG:4326, lignes linéaires en latitude) en la reprojetant
  // ligne par ligne en Web Mercator pour un calage exact sous Leaflet.
  function renderLayer(ly) {
    const { width: W, height: H, data, bounds: b } = ly;
    const cv = ly.canvas || (ly.canvas = document.createElement('canvas'));
    cv.width = W; cv.height = H;
    const ctx = cv.getContext('2d'), img = ctx.createImageData(W, H), px = img.data;
    const yN = mercY(b.north), yS = mercY(b.south), dlat = (b.north - b.south) / H, span = ly.strong - ly.weak;
    for (let j = 0; j < H; j++) {
      const lat = invMercY(yN - (j + 0.5) * (yN - yS) / H);
      const r = Math.min(H - 1, Math.max(0, Math.floor((b.north - lat) / dlat)));
      const src = r * W, dst = j * W * 4;
      for (let i = 0; i < W; i++) {
        const v = data[src + i], o = dst + i * 4;
        const t = (v - ly.weak) / span;
        if (!(t >= 0)) { px[o + 3] = 0; continue; } // NaN ou sous le seuil : transparent
        const k = (t >= 1 ? 255 : (t * 255) | 0) * 3;
        px[o] = LUT[k]; px[o + 1] = LUT[k + 1]; px[o + 2] = LUT[k + 2]; px[o + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
    cv.toBlob(blob => {
      const url = URL.createObjectURL(blob), old = ly.url;
      ly.url = url;
      if (ly.overlay) ly.overlay.setUrl(url);
      else {
        ly.overlay = L.imageOverlay(url, [[b.south, b.west], [b.north, b.east]], { opacity: ly.opacity, interactive: false });
        if (ly.visible) ly.overlay.addTo(map);
      }
      if (old) setTimeout(() => URL.revokeObjectURL(old), 1000);
    });
  }

  async function computeCoverage() {
    const site = state.sites.find(s => s.id === $('#cov-site').value);
    if (!site) return;
    const output = $('#cov-output').value, q = QUANTITIES[output];
    const body = {
      ...radioParams(), tx: { lat: site.lat, lon: site.lon, height_m: site.height_m }, rx_height_m: num($('#cov-rx-h')),
      radius_km: num($('#cov-radius')), downsample: +$('#cov-ds').value, interp: $('#cov-interp').value, fast: +$('#cov-fast').value, output, eirp_dbm: site.eirp_dbm, format: 'f32',
    };
    covAbort = new AbortController();
    const btn = $('#btn-coverage'), t0 = performance.now();
    btn.disabled = true;
    $('#btn-cov-cancel').hidden = false;
    let progress = 'Computing…';
    const tick = setInterval(() => { btn.innerHTML = `<span class="spinner"></span> ${progress} · ${((performance.now() - t0) / 1000).toFixed(0)} s`; }, 250);
    try {
      const onProgress = p => {
        progress = p.phase === 'tiles'
          ? `Tiles ${p.done}/${p.total}${p.downloads ? ` (${p.downloads} downloaded)` : ''}`
          : `ITM ${Math.round(100 * p.done / Math.max(1, p.total))} %`;
      };
      const r = await api('/v1/coverage', { method: 'POST', body: JSON.stringify(body), signal: covAbort.signal, onProgress });
      const [west, south, east, north] = r.headers.get('x-bounds').split(',').map(Number);
      const W = +r.headers.get('x-width'), H = +r.headers.get('x-height');
      const stats = JSON.parse(r.headers.get('x-coverage-stats') || '{}');
      const data = new Float32Array(await r.arrayBuffer());
      if (data.length !== W * H) throw new Error('incomplete response');
      let min = Infinity, max = -Infinity;
      for (const v of data) if (v === v) { if (v < min) min = v; if (v > max) max = v; }
      const ly = {
        id: uid(), name: `${site.name} · ${fmt(state.radio.frequency_mhz, 1)} MHz · ${body.radius_km} km${body.downsample > 1 ? ' · preview ×' + body.downsample : ''}`, site: site.name, color: site.color,
        req: body, quantity: output, unit: q.unit, bounds: { west, south, east, north }, width: W, height: H, data,
        weak: q.weak, strong: q.strong, opacity: 0.7, visible: true, min, max, stats, secs: (performance.now() - t0) / 1000,
      };
      layers.unshift(ly);
      renderLayer(ly);
      renderLayers();
      toast(`Coverage computed: ${fmt(stats.itm_calls / 1e6, 2)} M ITM calls in ${fmt(ly.secs, 1)} s.`, 'ok');
    } catch (err) {
      if (err.name === 'AbortError') toast('Computation cancelled.');
      else toast('Coverage: ' + err.message, 'error');
    } finally {
      clearInterval(tick);
      covAbort = null;
      btn.textContent = 'Compute coverage';
      $('#btn-cov-cancel').hidden = true;
      updateEstimate();
    }
  }
  $('#btn-coverage').addEventListener('click', computeCoverage);
  $('#btn-cov-cancel').addEventListener('click', () => covAbort?.abort());

  function renderLayers() {
    const ul = $('#layer-list');
    if (!layers.length) { ul.innerHTML = '<li class="empty">No coverage computed yet.</li>'; return; }
    ul.innerHTML = layers.map(ly => `
      <li data-id="${ly.id}">
        <div class="card-head"><input type="checkbox" data-a="visible" ${ly.visible ? 'checked' : ''} title="Show">
          <span class="swatch" style="background:${ly.color}"></span><strong>${esc(ly.name)}</strong></div>
        <div class="legend" style="background:${GRADIENT}"></div>
        <div class="legend-labels"><span>${fmt(ly.weak, 0)}</span><span>${QUANTITIES[ly.quantity].label} (${ly.unit})</span><span>${fmt(ly.strong, 0)}</span></div>
        <div class="layer-ctrl">
          <span>Opacity</span><input type="range" data-a="opacity" min="0" max="1" step="0.05" value="${ly.opacity}">
          <span>Palette</span><div class="pair"><input type="number" data-a="weak" value="${ly.weak}" step="1" title="Low value (below: transparent)"><input type="number" data-a="strong" value="${ly.strong}" step="1" title="High value"></div>
        </div>
        <div class="hint">Values ${fmt(ly.min, 1)} to ${fmt(ly.max, 1)} ${ly.unit} · ${ly.width}×${ly.height} px · ${fmt(ly.secs, 1)} s</div>
        <div class="card-actions small">
          <button data-a="zoom">Zoom</button><button data-a="geotiff">GeoTIFF</button><button data-a="png" title="Displayed rendering (Web Mercator)">PNG</button>
          <button data-a="delete" class="danger">Delete</button>
        </div>
      </li>`).join('');
  }
  const layerOf = el => layers.find(l => l.id === el.closest('li[data-id]')?.dataset.id);
  let paletteTimer = null;
  $('#layer-list').addEventListener('input', e => {
    const ly = layerOf(e.target), a = e.target.dataset.a;
    if (!ly) return;
    if (a === 'opacity') { ly.opacity = +e.target.value; ly.overlay?.setOpacity(ly.opacity); }
    if (a === 'visible') { ly.visible = e.target.checked; if (ly.visible) ly.overlay?.addTo(map); else ly.overlay?.remove(); }
    if (a === 'weak' || a === 'strong') {
      const v = parseFloat(e.target.value);
      if (!isFinite(v)) return;
      ly[a] = v;
      clearTimeout(paletteTimer);
      paletteTimer = setTimeout(() => {
        renderLayer(ly);
        const labels = e.target.closest('li').querySelector('.legend-labels').children;
        labels[0].textContent = fmt(ly.weak, 0); labels[2].textContent = fmt(ly.strong, 0);
      }, 200);
    }
  });
  $('#layer-list').addEventListener('click', async e => {
    const ly = layerOf(e.target), a = e.target.dataset.a;
    if (!ly || !a) return;
    const b = ly.bounds;
    if (a === 'zoom') map.fitBounds([[b.south, b.west], [b.north, b.east]]);
    if (a === 'delete') {
      ly.overlay?.remove();
      if (ly.url) URL.revokeObjectURL(ly.url);
      layers.splice(layers.indexOf(ly), 1);
      renderLayers();
    }
    if (a === 'png') ly.canvas.toBlob(bl => download(bl, `coverage_${ly.site}_${ly.quantity}.png`));
    if (a === 'geotiff') {
      e.target.disabled = true;
      e.target.innerHTML = '<span class="spinner" style="border-color:#ccc;border-top-color:#2563eb"></span>';
      try {
        const r = await api('/v1/coverage', { method: 'POST', body: JSON.stringify({ ...ly.req, format: 'geotiff' }) });
        download(await r.blob(), `coverage_${ly.site}_${ly.quantity}_${ly.req.frequency_mhz}MHz.tif`);
      } catch (err) { toast('GeoTIFF : ' + err.message, 'error'); }
      e.target.disabled = false;
      e.target.textContent = 'GeoTIFF';
    }
  });

  // Valeur sous le curseur (couche visible la plus haute) + altitude
  function layerValueAt(lat, lon) {
    for (const ly of layers) {
      if (!ly.visible) continue;
      const b = ly.bounds;
      if (lat < b.south || lat > b.north || lon < b.west || lon > b.east) continue;
      const r = Math.min(ly.height - 1, Math.floor((b.north - lat) / (b.north - b.south) * ly.height));
      const c = Math.min(ly.width - 1, Math.floor((lon - b.west) / (b.east - b.west) * ly.width));
      const v = ly.data[r * ly.width + c];
      if (v === v) return `${fmt(v, 1)} ${ly.unit}`;
    }
    return '';
  }
  let elevTimer = null, elevCtrl = null, lastMove = null;
  map.on('mousemove', e => {
    lastMove = e.latlng;
    $('#st-pos').textContent = `${e.latlng.lat.toFixed(5)}, ${e.latlng.lng.toFixed(5)}`;
    const v = layerValueAt(e.latlng.lat, e.latlng.lng);
    $('#st-val').textContent = v ? '· ' + v : '';
    clearTimeout(elevTimer);
    elevTimer = setTimeout(async () => {
      const ll = lastMove;
      elevCtrl?.abort();
      elevCtrl = new AbortController();
      try {
        const r = await api(`/v1/elevation?lat=${ll.lat}&lon=${ll.lng}`, { signal: elevCtrl.signal });
        const j = await r.json();
        if (ll === lastMove && j.elevation_m != null) $('#st-elev').textContent = `· elev. ${fmt(j.elevation_m, 0)} m`;
      } catch { }
    }, 300);
  });

  // ---------------------------------------------------------------- paramètres radio
  const RF = { '#p-freq': 'frequency_mhz', '#p-pol': 'polarization', '#p-climate': 'climate', '#p-eps': 'epsilon', '#p-sigma': 'sigma',
    '#p-n0': 'n0', '#p-mdvar': 'mdvar', '#p-time': 'time', '#p-location': 'location', '#p-situation': 'situation',
    '#p-rxgain': 'rx_gain_dbi', '#p-eirp': 'default_eirp_dbm' };
  $('#band-preset').innerHTML = '<option value="">— Choose —</option>' + BANDS.map((b, i) => `<option value="${i}">${b[0]}</option>`).join('');
  function loadRadioForm() {
    for (const [sel, k] of Object.entries(RF)) $(sel).value = state.radio[k];
    const g = `${state.radio.epsilon},${state.radio.sigma}`;
    $('#ground-preset').value = [...$('#ground-preset').options].some(o => o.value === g) ? g : 'custom';
    const bi = BANDS.findIndex(b => b[1] === state.radio.frequency_mhz);
    $('#band-preset').value = bi >= 0 ? String(bi) : '';
  }
  for (const [sel, k] of Object.entries(RF)) {
    $(sel).addEventListener('change', e => {
      const el = e.target;
      const v = el.tagName === 'SELECT' && k !== 'mdvar' ? el.value : parseFloat(el.value);
      if (typeof v === 'number' && !isFinite(v)) { el.value = state.radio[k]; return; }
      state.radio[k] = v;
      save(); loadRadioForm();
    });
  }
  $('#band-preset').addEventListener('change', e => {
    const b = BANDS[+e.target.value];
    if (!b) return;
    state.radio.frequency_mhz = b[1];
    state.radio.polarization = b[2] || 'vertical';
    save(); loadRadioForm();
  });
  $('#ground-preset').addEventListener('change', e => {
    if (e.target.value === 'custom') return;
    const [eps, sig] = e.target.value.split(',').map(Number);
    state.radio.epsilon = eps; state.radio.sigma = sig;
    save(); loadRadioForm();
  });
  $('#btn-reset-radio').addEventListener('click', () => { state.radio = { ...DEFAULT_RADIO }; save(); loadRadioForm(); });

  // ---------------------------------------------------------------- démarrage
  loadRadioForm();
  state.sites.forEach(syncMarker);
  renderSites();
  renderLayers();
  setRxMode('point');
  api('/v1/info').then(r => r.json()).then(j => {
    info = j;
    $('#version').textContent = `v${j.version} · WebAssembly · ${POOL_SIZE} workers`;
    $('#cov-radius').max = j.limits.max_radius_km;
    updateEstimate();
  }).catch(e => toast('WebAssembly module unavailable: ' + e.message, 'error'));

  // ---------------------------------------------------------------- MNE (cache, import)
  async function showDemCache() {
    try {
      const c = await cacheInfo();
      $('#dem-cache-info').textContent = `Cache: ${c.tiles} tile${c.tiles === 1 ? '' : 's'}, ${fmt(c.bytes / 1e6, 0)} MB`;
    } catch { $('#dem-cache-info').textContent = 'Browser cache unavailable.'; }
  }
  $('#dem-import').addEventListener('change', async e => {
    let ok = 0;
    for (const f of e.target.files) {
      try {
        const buf = await f.arrayBuffer();
        const [key] = JSON.parse(await handleTileKey(buf));
        await importTile(key.name, buf);
        ok++;
      } catch (err) { toast(`${f.name}: ${err.message}`, 'error'); }
    }
    e.target.value = '';
    if (ok) toast(`${ok} tile${ok === 1 ? '' : 's'} imported.`, 'ok');
    showDemCache();
  });
  async function handleTileKey(buf) {
    const r = await api('/v1/tile-key', { method: 'POST', body: buf });
    return r.text();
  }
  $('#btn-dem-clear').addEventListener('click', async () => {
    await clearCache();
    toast('Tile cache cleared.');
    showDemCache();
  });
  showDemCache();
  if (!state.sites.length) showTab('sites');
})();
