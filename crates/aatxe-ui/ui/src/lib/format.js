// Nanosecond formatter shared by the chart and the compare tables.
export function formatNs(ns) {
  if (!isFinite(ns)) return "—";
  const abs = Math.abs(ns);
  if (abs >= 1e9) return (ns / 1e9).toFixed(2) + " s";
  if (abs >= 1e6) return (ns / 1e6).toFixed(2) + " ms";
  if (abs >= 1e3) return (ns / 1e3).toFixed(2) + " µs";
  return ns.toFixed(1) + " ns";
}

// Agent lifecycle → status class (drives the colored dot).
export function agentStatus(a) {
  if (a.failed) return "failed";
  if (a.exit) return "done";
  if (a.benching) return "benching";
  return "running";
}

// Verdict → color, matching the badge palette in app.css.
export const VERDICT_COLORS = {
  regression: "#e0452f",
  improvement: "#3fb96b",
  neutral: "#8a91a3",
  new: "#4f8fe0",
  removed: "#d8a23a",
  "out-of-scope": "#d8a23a",
};
