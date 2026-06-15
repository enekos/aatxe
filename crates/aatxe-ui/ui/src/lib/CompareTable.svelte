<script>
  import { formatNs } from "./format.js";
  let { report } = $props();
  const rows = $derived((report.diffs || []).map((d) => ({
    name: d.name,
    base: d.base ? formatNs(d.base.median) : "—",
    head: d.head ? formatNs(d.head.median) : "—",
    delta: d.deltaPct != null ? `${d.deltaPct > 0 ? "+" : ""}${(d.deltaPct * 100).toFixed(1)}%` : "—",
    p: d.pValue != null ? d.pValue.toFixed(4) : "—",
    verdict: d.verdict,
  })));
</script>

<table>
  <thead>
    <tr>
      <th>bench</th><th class="num">base</th><th class="num">head</th>
      <th class="num">Δ median</th><th class="num">p</th><th>verdict</th>
    </tr>
  </thead>
  <tbody>
    {#each rows as r}
      <tr>
        <td>{r.name}</td>
        <td class="num">{r.base}</td>
        <td class="num">{r.head}</td>
        <td class="num">{r.delta}</td>
        <td class="num">{r.p}</td>
        <td><span class="badge {r.verdict}">{r.verdict}</span></td>
      </tr>
    {/each}
  </tbody>
</table>
