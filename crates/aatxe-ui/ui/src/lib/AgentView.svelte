<script>
  import { formatNs, agentStatus } from "./format.js";
  import { view } from "./store.js";
  import Chart from "./Chart.svelte";
  import CompareTable from "./CompareTable.svelte";
  import SummaryChips from "./SummaryChips.svelte";

  let { agentId } = $props();

  // Always read the agent fresh out of the published view, so in-place
  // reducer mutations (new outputs, compares, council verdict) surface. The
  // spread makes a new top-level reference every publish so the template and
  // the deriveds below recompute (nested arrays stay shared, read live).
  const agent = $derived({ ...$view.agents.get(agentId) });
  const status = $derived(agentStatus(agent));
  const last = $derived(agent.compares[agent.compares.length - 1]);

  // bench name -> {points:[{x,y,verdict,label}], baseY, lastDelta, deltaTxt, deltaCls}
  const series = $derived.by(() => {
    const map = new Map();
    for (const c of agent.compares) {
      for (const d of c.report.diffs || []) {
        if (!d.head) continue;
        if (!map.has(d.name)) map.set(d.name, { points: [], baseY: null });
        const s = map.get(d.name);
        if (d.base) s.baseY = d.base.median;
        const deltaTxt = d.deltaPct != null ? ` (${(d.deltaPct * 100).toFixed(1)}%)` : "";
        s.points.push({
          x: c.iteration, y: d.head.median, verdict: d.verdict,
          label: `#${c.iteration}: ${formatNs(d.head.median)}${deltaTxt} — ${d.verdict}`,
        });
      }
    }
    return [...map.entries()].map(([name, s]) => {
      const lastPt = s.points[s.points.length - 1];
      const delta = s.baseY && lastPt ? (lastPt.y - s.baseY) / s.baseY : null;
      return {
        name, points: s.points, baseY: s.baseY,
        deltaCls: delta == null ? "" : delta > 0 ? "up" : "down",
        deltaTxt: delta == null ? "" : `${delta > 0 ? "+" : ""}${(delta * 100).toFixed(1)}%`,
      };
    });
  });

  // ---- diff card ----
  let showDiff = $state(false);
  let diffBody = $state("loading…");

  async function loadDiff() {
    showDiff = true;
    diffBody = "loading…";
    try {
      const r = await fetch(`/api/agents/${agent.id}/diff`);
      diffBody = r.ok ? (await r.text()) || "(empty diff)" : `error: ${await r.text()}`;
    } catch (e) {
      diffBody = `error: ${e}`;
    }
  }

  // Auto-scroll the transcript to the bottom as lines arrive.
  function autoscroll(node) {
    const scroll = () => { node.scrollTop = node.scrollHeight; };
    scroll();
    return { update: scroll };
  }
</script>

<div class="card">
  <div class="agent-head">
    <span class="dot {status}"></span>
    <span class="name">{agent.name}</span>
    <span class="meta">{agent.id} · {agent.branch}</span>
    {#if agent.exit}
      <span class="meta">exit {agent.exit.exitCode ?? "?"} · {agent.exit.iterations} iteration(s)</span>
    {/if}
    <button class="mini" onclick={loadDiff}>diff</button>
  </div>
  <div class="task-text">{agent.task}</div>
  {#if agent.failed}
    <div class="iter-failed">agent failed: {agent.failed}</div>
  {/if}
</div>

<div class="card">
  <h3>trajectories
    <span class="right">{agent.benching ? "benching…" : last ? `iter #${last.iteration}` : "waiting for first edit"}</span>
  </h3>
  {#if series.length === 0}
    <div class="notices">no benchmark data yet — first iteration runs after the agent's first edit</div>
  {:else}
    <div class="charts">
      {#each series as s}
        <div class="chart-box">
          <div class="chart-title">{s.name}<span class="delta {s.deltaCls}">{s.deltaTxt}</span></div>
          <Chart points={s.points} baseY={s.baseY} />
        </div>
      {/each}
    </div>
  {/if}
  {#each agent.failures.slice(-3) as f}
    <div class="iter-failed">iter #{f.iteration} bench failed: {f.error}</div>
  {/each}
</div>

{#if last}
  <div class="card">
    <h3>latest compare <span class="right"><SummaryChips summary={last.report.summary} /></span></h3>
    <CompareTable report={last.report} />
  </div>
{/if}

{#if agent.councilState}
  <div class="card">
    <h3>council</h3>
    {#if agent.councilState === "running"}
      <div class="notices">council reviewing…</div>
    {:else if agent.councilState === "failed"}
      <div class="iter-failed">council failed: {agent.council.error}</div>
    {:else}
      <div class="council-chips">
        <span class="badge crit">{agent.council.critical} critical</span>
        <span class="badge removed">{agent.council.major} major</span>
        <span class="badge neutral">{agent.council.shippable} shippable</span>
      </div>
      <pre class="council">{agent.council.markdown || "(empty)"}</pre>
    {/if}
  </div>
{/if}

<div class="card">
  <h3>transcript <span class="right">{agent.outputs.length} lines</span></h3>
  <div class="transcript" use:autoscroll={agent.outputs.length}>
    {#each agent.outputs as o}
      <div class="line k-{o.kind}">{o.text}</div>
    {/each}
  </div>
</div>

{#if showDiff}
  <div class="card">
    <h3>diff vs base</h3>
    <pre class="diffview">{diffBody}</pre>
  </div>
{/if}
