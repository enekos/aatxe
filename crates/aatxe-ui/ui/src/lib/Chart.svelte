<script>
  // Tiny dependency-free SVG trajectory chart, ported from chart.js to a
  // declarative Svelte template.
  //   points: [{x, y, verdict, label}]   iteration -> median ns
  //   baseY:  number|null                dashed baseline (base median)
  import { formatNs, VERDICT_COLORS } from "./format.js";

  let { points = [], baseY = null } = $props();

  const W = 340;
  const H = 130;
  const padL = 56, padR = 10, padT = 8, padB = 18;

  // y range across the points plus the baseline, with 8% headroom.
  const range = $derived.by(() => {
    const ys = points.map((p) => p.y).concat(baseY !== null ? [baseY] : []);
    if (ys.length === 0) return null;
    let yMin = Math.min(...ys), yMax = Math.max(...ys);
    if (yMin === yMax) { yMin *= 0.95; yMax *= 1.05; }
    const span = yMax - yMin;
    yMin -= span * 0.08; yMax += span * 0.08;
    const xs = points.map((p) => p.x);
    const xMin = Math.min(1, ...xs), xMax = Math.max(2, ...xs);
    return { yMin, yMax, xMin, xMax };
  });

  const sx = (x) => padL + ((x - range.xMin) / (range.xMax - range.xMin)) * (W - padL - padR);
  const sy = (y) => padT + (1 - (y - range.yMin) / (range.yMax - range.yMin)) * (H - padT - padB);

  const ticks = $derived(range
    ? [0, 1, 2].map((i) => {
        const v = range.yMin + ((range.yMax - range.yMin) * i) / 2;
        return { v, y: sy(v) };
      })
    : []);

  const path = $derived(range && points.length > 1
    ? points.map((p, i) => `${i === 0 ? "M" : "L"}${sx(p.x).toFixed(1)},${sy(p.y).toFixed(1)}`).join(" ")
    : "");
</script>

{#if !range}
  <div style="color:#8a91a3;font-size:11px">no data yet</div>
{:else}
  <svg width={W} height={H} role="img">
    {#each ticks as t}
      <line x1={padL} y1={t.y} x2={W - padR} y2={t.y} stroke="#262b38" stroke-width="1" />
      <text x={padL - 5} y={t.y + 3} text-anchor="end" fill="#8a91a3" font-size="9">{formatNs(t.v)}</text>
    {/each}

    {#if baseY !== null}
      <line x1={padL} y1={sy(baseY)} x2={W - padR} y2={sy(baseY)} stroke="#8a91a3" stroke-width="1" stroke-dasharray="5 4" />
      <text x={W - padR} y={sy(baseY) - 3} text-anchor="end" fill="#8a91a3" font-size="9">base {formatNs(baseY)}</text>
    {/if}

    {#if path}
      <path d={path} fill="none" stroke="#4f8fe0" stroke-width="1.5" opacity="0.7" />
    {/if}

    {#each points as p}
      <circle cx={sx(p.x).toFixed(1)} cy={sy(p.y).toFixed(1)} r="4" fill={VERDICT_COLORS[p.verdict] || VERDICT_COLORS.neutral}>
        <title>{p.label || `#${p.x}: ${formatNs(p.y)}`}</title>
      </circle>
      <text x={sx(p.x).toFixed(1)} y={H - 5} text-anchor="middle" fill="#8a91a3" font-size="9">{p.x}</text>
    {/each}
  </svg>
{/if}
