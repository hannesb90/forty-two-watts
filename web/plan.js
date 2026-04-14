// plan.js — MPC plan + prices + forecast visualization.
// Renders a stacked canvas chart: price bars on top, battery+grid bars in
// the middle, SoC + PV line on bottom. Refreshes every 30s.

(function () {
  'use strict';

  const PLAN_REFRESH_MS = 30000;

  const state = {
    prices: null,
    forecast: null,
    plan: null,
    lastUpdate: null,
  };

  async function fetchAll() {
    const [p, f, m] = await Promise.all([
      fetch('/api/prices').then(r => r.json()).catch(() => ({})),
      fetch('/api/forecast').then(r => r.json()).catch(() => ({})),
      fetch('/api/mpc/plan').then(r => r.json()).catch(() => ({})),
    ]);
    state.prices = (p && p.items) || [];
    state.forecast = (f && f.items) || [];
    state.plan = (m && m.plan) || null;
    state.enabled = {
      prices: p && p.enabled,
      forecast: f && f.enabled,
      mpc: m && m.enabled,
    };
    state.lastUpdate = new Date();
    render();
  }

  async function replan() {
    try {
      const r = await fetch('/api/mpc/replan', { method: 'POST' });
      const j = await r.json();
      if (j && j.plan) state.plan = j.plan;
      render();
    } catch (e) { /* ignore */ }
  }

  function fmtHHMM(ts) {
    const d = new Date(ts);
    return d.getHours().toString().padStart(2, '0') + ':' +
           d.getMinutes().toString().padStart(2, '0');
  }

  function render() {
    const canvas = document.getElementById('plan-chart');
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    const cssW = canvas.clientWidth || 800;
    const cssH = 260;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = cssW * dpr;
    canvas.height = cssH * dpr;
    canvas.style.height = cssH + 'px';
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    const pad = { l: 44, r: 44, t: 16, b: 28 };
    const plotW = cssW - pad.l - pad.r;
    const plotH = cssH - pad.t - pad.b;

    // X range = now → +24h
    const now = Date.now();
    const tMin = now - 30 * 60 * 1000; // 30 min look-back so in-progress slot is visible
    const tMax = now + 24 * 60 * 60 * 1000;
    const xScale = t => pad.l + (t - tMin) / (tMax - tMin) * plotW;

    // Price range
    const prices = (state.prices || []).filter(p => p.slot_ts_ms >= tMin && p.slot_ts_ms <= tMax);
    const totals = prices.map(p => p.total_ore_kwh);
    const priceMin = totals.length ? Math.min(0, ...totals) : 0;
    const priceMax = totals.length ? Math.max(...totals, 1) : 200;
    const priceRange = priceMax - priceMin;

    // Price band on top third
    const priceY0 = pad.t;
    const priceH = plotH * 0.32;
    const priceY = v => priceY0 + priceH - (v - priceMin) / priceRange * priceH;

    // Power band in middle — covers battery + grid
    const powerY0 = priceY0 + priceH + 10;
    const powerH = plotH * 0.42;
    // Scale based on plan battery + PV magnitudes
    const plan = state.plan;
    let pMagMax = 1000;
    if (plan && plan.actions) {
      for (const a of plan.actions) {
        pMagMax = Math.max(pMagMax, Math.abs(a.battery_w), Math.abs(a.grid_w), Math.abs(a.pv_w));
      }
    } else {
      for (const f of state.forecast || []) {
        if (f.pv_w_estimated) pMagMax = Math.max(pMagMax, f.pv_w_estimated);
      }
    }
    const powerYCenter = powerY0 + powerH / 2;
    const powerY = w => powerYCenter - (w / pMagMax) * (powerH / 2);

    // SoC line on bottom band
    const socY0 = powerY0 + powerH + 4;
    const socH = plotH * 0.18;
    const socY = p => socY0 + socH - (p / 100) * socH;

    // ---- Grid ticks (hours) ----
    ctx.strokeStyle = 'rgba(255,255,255,0.08)';
    ctx.lineWidth = 1;
    ctx.fillStyle = 'rgba(255,255,255,0.45)';
    ctx.font = '11px system-ui, sans-serif';
    ctx.textAlign = 'center';
    for (let h = 0; h <= 24; h += 3) {
      const t = now + h * 3600 * 1000;
      const x = xScale(t);
      ctx.beginPath();
      ctx.moveTo(x, pad.t);
      ctx.lineTo(x, pad.t + plotH);
      ctx.stroke();
      ctx.fillText(fmtHHMM(t), x, cssH - 10);
    }
    // Now-line
    const xNow = xScale(now);
    ctx.strokeStyle = '#ef4444';
    ctx.lineWidth = 1.2;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(xNow, pad.t);
    ctx.lineTo(xNow, pad.t + plotH);
    ctx.stroke();
    ctx.setLineDash([]);

    // ---- Price bars ----
    // Color cheap (low) → green, expensive → red.
    const sortedTotals = [...totals].sort((a, b) => a - b);
    const p25 = sortedTotals[Math.floor(sortedTotals.length * 0.25)] || priceMin;
    const p75 = sortedTotals[Math.floor(sortedTotals.length * 0.75)] || priceMax;
    for (const p of prices) {
      const x0 = xScale(p.slot_ts_ms);
      const x1 = xScale(p.slot_ts_ms + p.slot_len_min * 60 * 1000);
      const y = priceY(p.total_ore_kwh);
      const zero = priceY(Math.max(0, priceMin));
      let color;
      if (p.total_ore_kwh <= p25) color = 'rgba(34,197,94,0.55)';        // cheap = green
      else if (p.total_ore_kwh >= p75) color = 'rgba(239,68,68,0.55)';   // expensive = red
      else color = 'rgba(148,163,184,0.45)';                             // mid = slate
      ctx.fillStyle = color;
      ctx.fillRect(x0, Math.min(y, zero), Math.max(1, x1 - x0 - 1), Math.abs(y - zero));
    }
    // Price axis labels
    ctx.fillStyle = 'rgba(255,255,255,0.55)';
    ctx.textAlign = 'right';
    ctx.fillText(priceMax.toFixed(0) + ' öre', pad.l - 6, priceY0 + 10);
    ctx.fillText(priceMin.toFixed(0), pad.l - 6, priceY0 + priceH);
    ctx.textAlign = 'left';
    ctx.fillText('Price', pad.l + 4, priceY0 + 12);

    // ---- Forecast PV line (negative = generation, site sign) ----
    ctx.strokeStyle = 'rgba(34,197,94,0.9)';
    ctx.lineWidth = 2;
    ctx.beginPath();
    let first = true;
    for (const f of state.forecast || []) {
      if (f.slot_ts_ms > tMax || !f.pv_w_estimated) continue;
      const x = xScale(f.slot_ts_ms);
      const y = powerY(-f.pv_w_estimated); // site sign
      if (first) { ctx.moveTo(x, y); first = false; }
      else ctx.lineTo(x, y);
    }
    ctx.stroke();

    // Load forecast from the plan's per-slot predictions (twin-driven).
    // Rendered above the PV curve as a pale-yellow dashed line so we can
    // see what the optimizer expects the house to consume each slot.
    if (plan && plan.actions && plan.actions.length) {
      ctx.strokeStyle = '#fde68a';
      ctx.lineWidth = 1.8;
      ctx.setLineDash([4, 5]);
      ctx.beginPath();
      let f2 = true;
      for (const a of plan.actions) {
        if (a.slot_start_ms > tMax) break;
        if (a.load_w == null) continue;
        const x = xScale(a.slot_start_ms);
        const y = powerY(a.load_w);
        if (f2) { ctx.moveTo(x, y); f2 = false; }
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // Power zero-line
    ctx.strokeStyle = 'rgba(255,255,255,0.25)';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(pad.l, powerYCenter);
    ctx.lineTo(pad.l + plotW, powerYCenter);
    ctx.stroke();
    ctx.fillStyle = 'rgba(255,255,255,0.55)';
    ctx.textAlign = 'right';
    ctx.fillText('+' + (pMagMax / 1000).toFixed(1) + 'kW', pad.l - 6, powerY(pMagMax) + 4);
    ctx.fillText((-pMagMax / 1000).toFixed(1) + 'kW', pad.l - 6, powerY(-pMagMax) + 4);
    ctx.textAlign = 'left';
    ctx.fillText('Power', pad.l + 4, powerY0 + 12);

    // ---- Plan battery bars ----
    if (plan && plan.actions) {
      for (const a of plan.actions) {
        if (a.slot_start_ms > tMax) break;
        const x0 = xScale(a.slot_start_ms);
        const x1 = xScale(a.slot_start_ms + a.slot_len_min * 60 * 1000);
        const y = powerY(a.battery_w);
        const color = a.battery_w >= 0 ? 'rgba(245,158,11,0.65)' : 'rgba(139,92,246,0.65)';
        ctx.fillStyle = color;
        ctx.fillRect(x0, Math.min(y, powerYCenter), Math.max(1, x1 - x0 - 1), Math.abs(y - powerYCenter));
      }
      // SoC line
      ctx.strokeStyle = 'rgba(96,165,250,0.95)';
      ctx.lineWidth = 2;
      ctx.beginPath();
      first = true;
      // Anchor at start SoC at now
      if (plan.initial_soc_pct != null) {
        ctx.moveTo(xScale(now), socY(plan.initial_soc_pct));
        first = false;
      }
      for (const a of plan.actions) {
        if (a.slot_start_ms > tMax) break;
        const x = xScale(a.slot_start_ms + a.slot_len_min * 60 * 1000);
        const y = socY(a.soc_pct);
        if (first) { ctx.moveTo(x, y); first = false; }
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
      ctx.fillStyle = 'rgba(255,255,255,0.55)';
      ctx.textAlign = 'right';
      ctx.fillText('100%', cssW - pad.r + 40, socY(100) + 4);
      ctx.fillText('0%', cssW - pad.r + 40, socY(0) + 4);
      ctx.textAlign = 'left';
      ctx.fillText('SoC', pad.l + 4, socY0 + 12);
    }

    // ---- Summary ----
    const summary = document.getElementById('plan-summary');
    if (summary) {
      if (!state.enabled || !state.enabled.mpc) {
        summary.textContent = 'MPC planner disabled';
      } else if (!plan) {
        summary.textContent = state.prices && state.prices.length
          ? 'Waiting for first plan…'
          : 'Waiting for price data…';
      } else {
        const hh = plan.horizon_slots * (plan.actions[0] ? plan.actions[0].slot_len_min : 15) / 60;
        const cost = plan.total_cost_ore / 100;
        summary.textContent =
          `${plan.mode} · ${hh.toFixed(0)}h horizon · ${plan.horizon_slots} slots · ` +
          `SoC ${plan.initial_soc_pct.toFixed(0)}% → plan cost ${cost.toFixed(2)} SEK`;
      }
    }
  }

  function init() {
    fetchAll();
    setInterval(fetchAll, PLAN_REFRESH_MS);
    window.addEventListener('resize', render);
    const btn = document.getElementById('plan-replan');
    if (btn) btn.addEventListener('click', replan);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
