// Tiny dependency-free SVG trajectory chart.
//
// AatxeChart.trajectory(el, {
//   points:  [{x, y, verdict, label}],   // iteration → median ns
//   baseY:   number|null,                // dashed baseline (base median)
//   height:  number,
// })
//
// Verdict colors match the badge palette in style.css.
(function () {
  "use strict";

  const COLORS = {
    regression: "#e0452f",
    improvement: "#3fb96b",
    neutral: "#8a91a3",
    new: "#4f8fe0",
    removed: "#d8a23a",
    "out-of-scope": "#d8a23a",
  };

  function formatNs(ns) {
    if (!isFinite(ns)) return "—";
    const abs = Math.abs(ns);
    if (abs >= 1e9) return (ns / 1e9).toFixed(2) + " s";
    if (abs >= 1e6) return (ns / 1e6).toFixed(2) + " ms";
    if (abs >= 1e3) return (ns / 1e3).toFixed(2) + " µs";
    return ns.toFixed(1) + " ns";
  }

  function esc(s) {
    return String(s).replace(/[&<>"]/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])
    );
  }

  function trajectory(el, opts) {
    const points = opts.points || [];
    const baseY = opts.baseY ?? null;
    const W = el.clientWidth > 0 ? el.clientWidth : 340;
    const H = opts.height || 130;
    const padL = 56, padR = 10, padT = 8, padB = 18;

    const ys = points.map((p) => p.y).concat(baseY !== null ? [baseY] : []);
    if (ys.length === 0) {
      el.innerHTML = '<div style="color:#8a91a3;font-size:11px">no data yet</div>';
      return;
    }
    let yMin = Math.min(...ys), yMax = Math.max(...ys);
    if (yMin === yMax) { yMin *= 0.95; yMax *= 1.05; }
    const span = yMax - yMin;
    yMin -= span * 0.08; yMax += span * 0.08;

    const xs = points.map((p) => p.x);
    const xMin = Math.min(1, ...xs), xMax = Math.max(2, ...xs);

    const sx = (x) => padL + ((x - xMin) / (xMax - xMin)) * (W - padL - padR);
    const sy = (y) => padT + (1 - (y - yMin) / (yMax - yMin)) * (H - padT - padB);

    let svg = `<svg width="${W}" height="${H}" role="img">`;

    // y gridlines + labels (3 ticks)
    for (let i = 0; i <= 2; i++) {
      const v = yMin + ((yMax - yMin) * i) / 2;
      const y = sy(v);
      svg += `<line x1="${padL}" y1="${y}" x2="${W - padR}" y2="${y}" stroke="#262b38" stroke-width="1"/>`;
      svg += `<text x="${padL - 5}" y="${y + 3}" text-anchor="end" fill="#8a91a3" font-size="9">${esc(formatNs(v))}</text>`;
    }

    // baseline
    if (baseY !== null) {
      const y = sy(baseY);
      svg += `<line x1="${padL}" y1="${y}" x2="${W - padR}" y2="${y}" stroke="#8a91a3" stroke-width="1" stroke-dasharray="5 4"/>`;
      svg += `<text x="${W - padR}" y="${y - 3}" text-anchor="end" fill="#8a91a3" font-size="9">base ${esc(formatNs(baseY))}</text>`;
    }

    // path
    if (points.length > 1) {
      const d = points
        .map((p, i) => `${i === 0 ? "M" : "L"}${sx(p.x).toFixed(1)},${sy(p.y).toFixed(1)}`)
        .join(" ");
      svg += `<path d="${d}" fill="none" stroke="#4f8fe0" stroke-width="1.5" opacity="0.7"/>`;
    }

    // points
    for (const p of points) {
      const c = COLORS[p.verdict] || COLORS.neutral;
      svg += `<circle cx="${sx(p.x).toFixed(1)}" cy="${sy(p.y).toFixed(1)}" r="4" fill="${c}">` +
        `<title>${esc(p.label || `#${p.x}: ${formatNs(p.y)}`)}</title></circle>`;
      svg += `<text x="${sx(p.x).toFixed(1)}" y="${H - 5}" text-anchor="middle" fill="#8a91a3" font-size="9">${p.x}</text>`;
    }

    svg += "</svg>";
    el.innerHTML = svg;
  }

  window.AatxeChart = { trajectory, formatNs };
})();
