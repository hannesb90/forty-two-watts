// <ftw-progress-bar> — horizontal fill bar.
//
// Attributes:
//   value   — current value (default 0)
//   max     — scale (default 100)
//   mode    — "solid" (default) or "gradient"
//               solid:    uses var(--ftw-progress-color) or the status color
//               gradient: green → yellow → red as value/max approaches 1
//   status  — "ok" | "warn" | "bad" | "neutral" (default "ok")
//             picks the solid color when mode=solid
//
// Pure bar — no label. Compose labels/values alongside in the parent card.
//
// Example:
//   <ftw-progress-bar value="72" max="100" mode="gradient"></ftw-progress-bar>
//   <ftw-progress-bar value="18" max="25" status="warn"></ftw-progress-bar>

import { FtwElement } from "./ftw-element.js";

class FtwProgressBar extends FtwElement {
  static styles = `
    :host {
      display: block;
      height: var(--ftw-progress-height, 8px);
      background: var(--surface2);
      border-radius: 999px;
      overflow: hidden;
    }
    .fill {
      height: 100%;
      width: 0%;
      transition: width 0.25s ease, background 0.25s ease;
      border-radius: inherit;
    }
    .solid-ok       { background: var(--green); }
    .solid-warn     { background: var(--yellow); }
    .solid-bad      { background: var(--red); }
    .solid-neutral  { background: var(--text-dim); }
  `;

  static get observedAttributes() {
    return ["value", "max", "mode", "status"];
  }

  attributeChangedCallback() {
    this.update();
  }

  render() {
    const value = Number(this.getAttribute("value") || 0);
    const max = Number(this.getAttribute("max") || 100) || 100;
    const mode = this.getAttribute("mode") || "solid";
    const status = this.getAttribute("status") || "ok";
    const pct = Math.max(0, Math.min(100, (value / max) * 100));

    let styleExtra = "";
    let cls = "fill";
    if (mode === "gradient") {
      // Interpolate hue from green (120°) through yellow (60°) to red (0°)
      // as fraction climbs from 0 → 1. Saturation + lightness match the
      // token palette closely without having to pick exact stops.
      const hue = Math.max(0, 120 - (pct / 100) * 120);
      styleExtra = `background: hsl(${hue.toFixed(0)}, 70%, 45%);`;
    } else {
      cls += ` solid-${status}`;
    }

    return `<div class="${cls}" style="width: ${pct.toFixed(1)}%; ${styleExtra}"></div>`;
  }
}

customElements.define("ftw-progress-bar", FtwProgressBar);
