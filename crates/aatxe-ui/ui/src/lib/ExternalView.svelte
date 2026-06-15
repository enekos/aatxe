<script>
  import { formatNs } from "./format.js";
  import { view } from "./store.js";
  import CompareTable from "./CompareTable.svelte";
  import SummaryChips from "./SummaryChips.svelte";

  const compares = $derived($view.external.slice(-10).reverse());
  const runs = $derived($view.ingested.slice(-10).reverse());
  const empty = $derived(compares.length === 0 && runs.length === 0);
</script>

{#if empty}
  <div class="empty"><p>nothing external yet — run <code>aatxe perf-vs</code> or POST a RunReport to /api/runs</p></div>
{:else}
  {#each compares as e}
    <div class="card">
      <h3>{e.source} <span class="right"><SummaryChips summary={e.report.summary} /></span></h3>
      <CompareTable report={e.report} />
    </div>
  {/each}
  {#each runs as e}
    <div class="card">
      <h3>{e.source} <span class="right">{e.report.ref}</span></h3>
      <table>
        <thead>
          <tr><th>bench</th><th class="num">median</th><th class="num">p95</th><th class="num">cv</th></tr>
        </thead>
        <tbody>
          {#each e.report.runs || [] as r}
            <tr>
              <td>{r.name}</td>
              <td class="num">{formatNs(r.median)}</td>
              <td class="num">{formatNs(r.p95)}</td>
              <td class="num">{(r.cv * 100).toFixed(1)}%</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/each}
{/if}
