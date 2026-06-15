<script>
  import { agentStatus } from "./format.js";
  import { clickable } from "./clickable.js";
  import { view } from "./store.js";
  import Notices from "./Notices.svelte";

  let { tournamentId, onselect } = $props();

  // Fresh shallow copy each publish so standings/contestants stay live.
  const tournament = $derived({ ...$view.tournaments.get(tournamentId) });
  const notices = $derived($view.notices);

  const contestants = $derived(
    tournament.agentIds
      .map((id) => $view.agents.get(id))
      .filter(Boolean)
  );
</script>

<div class="card">
  <h3>tournament {tournament.id}</h3>
  <div class="task-text">{tournament.task}</div>
</div>

<div class="card">
  <h3>leaderboard</h3>
  {#if tournament.standings.length}
    <table>
      <thead>
        <tr>
          <th></th><th>agent</th><th class="num">score</th>
          <th class="num">imp</th><th class="num">reg</th>
          <th class="num">crit</th><th class="num">net Δ</th>
        </tr>
      </thead>
      <tbody>
        {#each tournament.standings as s}
          <tr class={s.rank === 1 ? "lb-first" : ""}>
            <td class="lb-rank">#{s.rank}</td>
            <td>{s.name} <span class="sub">{s.agentId}</span></td>
            <td class="num">{s.score.toFixed(1)}</td>
            <td class="num">{s.improvements}</td>
            <td class="num">{s.regressions}</td>
            <td class="num">{s.councilCritical ?? "…"}</td>
            <td class="num">{(s.medianDeltaSum * 100).toFixed(1)}%</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <div class="notices">standings appear after the first benched iteration</div>
  {/if}
</div>

<div class="card">
  <h3>contestants</h3>
  <ul class="nav-list">
    {#each contestants as a}
      <li use:clickable={() => onselect({ kind: "agent", id: a.id })}>
        <span class="dot {agentStatus(a)}"></span>{a.name}
      </li>
    {/each}
  </ul>
</div>

<Notices {notices} />
