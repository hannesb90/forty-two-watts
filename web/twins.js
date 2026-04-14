// twins.js — advanced-mode ML diagnostics panel.
// Renders a small card per twin (PV, load, price forecaster) with
// sample count, MAE, quality bar, and last-updated time. Refreshes
// every 10s. Only mounted when body.advanced is active, but the
// fetches run regardless so switching to advanced is instant.

(function () {
  'use strict';

  const REFRESH_MS = 10000;

  async function fetchAll() {
    const [pv, load] = await Promise.all([
      fetch('/api/pvmodel').then(r => r.json()).catch(() => ({ enabled: false })),
      fetch('/api/loadmodel').then(r => r.json()).catch(() => ({ enabled: false })),
    ]);
    render(pv, load);
  }

  function fmtAge(ms) {
    if (!ms) return '—';
    const s = Math.round((Date.now() - ms) / 1000);
    if (s < 60) return s + 's ago';
    if (s < 3600) return Math.round(s / 60) + 'm ago';
    return Math.round(s / 3600) + 'h ago';
  }

  function twinCard(title, d) {
    if (!d || !d.enabled) return `<div class="twin-card"><h3>${title}</h3><div class="twin-row"><span>disabled</span></div></div>`;
    const q = Math.max(0, Math.min(1, d.quality || 0));
    const qPct = (q * 100).toFixed(0);
    const qColor = q >= 0.7 ? '#22c55e' : q >= 0.3 ? '#fbbf24' : '#ef4444';
    const rows = [];
    rows.push(`<div class="twin-row"><span>samples</span><b>${d.samples || 0}</b></div>`);
    if (d.mae_w != null) rows.push(`<div class="twin-row"><span>MAE</span><b>${d.mae_w.toFixed(0)} W</b></div>`);
    if (d.peak_w != null) rows.push(`<div class="twin-row"><span>peak ref</span><b>${(d.peak_w/1000).toFixed(1)} kW</b></div>`);
    if (d.rated_w != null) rows.push(`<div class="twin-row"><span>rated</span><b>${(d.rated_w/1000).toFixed(1)} kW</b></div>`);
    if (d.heating_w_per_degc != null && d.heating_w_per_degc > 0) {
      rows.push(`<div class="twin-row"><span>heating</span><b>${d.heating_w_per_degc.toFixed(0)} W/°C</b></div>`);
    }
    if (d.buckets_warm != null) rows.push(`<div class="twin-row"><span>buckets warm</span><b>${d.buckets_warm}/${d.buckets_total}</b></div>`);
    rows.push(`<div class="twin-row"><span>last update</span><b>${fmtAge(d.last_ms)}</b></div>`);
    rows.push(`<div class="twin-row"><span>quality</span><b>${qPct}%</b></div>`);
    rows.push(`<div class="twin-quality"><div class="twin-quality-fill" style="width:${qPct}%;background:${qColor}"></div></div>`);
    return `<div class="twin-card"><h3>${title}</h3>${rows.join('')}</div>`;
  }

  function render(pv, load) {
    const grid = document.getElementById('twins-grid');
    if (!grid) return;
    grid.innerHTML = twinCard('PV twin', pv) + twinCard('Load twin', load);
    const sub = document.getElementById('twins-subtitle');
    if (sub) sub.textContent = 'self-learning digital twins — feed MPC + UI forecasts';
  }

  function init() {
    fetchAll();
    setInterval(fetchAll, REFRESH_MS);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
