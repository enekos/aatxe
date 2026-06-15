<script>
  import { agentStatus } from "./format.js";
  import { clickable } from "./clickable.js";
  import { view, selected, sessions, loadSessions, viewSession, spawn } from "./store.js";

  let task = $state("");
  let count = $state("1");
  let spawnError = $state("");

  const agentItems = $derived($view.agentOrder.map((id) => {
    const a = $view.agents.get(id);
    const iters = a.compares.length ? `#${a.compares[a.compares.length - 1].iteration}` : "";
    return { id, name: a.name, status: agentStatus(a), iters };
  }));

  const tournamentItems = $derived([...$view.tournaments.values()].map((t) => ({
    id: t.id,
    leader: t.standings[0] ? t.standings[0].name : `${t.agentIds.length} agents`,
  })));

  const externalCount = $derived($view.external.length + $view.ingested.length);

  function select(kind, id) {
    selected.set({ kind, id: id || null });
  }

  async function doSpawn() {
    spawnError = "";
    const t = task.trim();
    if (!t) { spawnError = "task is empty"; return; }
    try {
      await spawn(t, parseInt(count, 10));
      task = "";
    } catch (e) {
      spawnError = String(e.message ?? e);
    }
  }

  function fmtWhen(ms) {
    return ms ? new Date(ms).toLocaleString() : "?";
  }
</script>

<nav id="rail">
  <section class="spawn">
    <textarea
      rows="3"
      bind:value={task}
      placeholder={"Task for the agent(s)…\ne.g. speed up diff::parse_unified_diff without changing its API"}
    ></textarea>
    <div class="spawn-row">
      <select bind:value={count} title="1 = single agent · 2+ = tournament">
        <option value="1">1 agent</option>
        <option value="2">2 — tournament</option>
        <option value="3">3 — tournament</option>
        <option value="4">4 — tournament</option>
        <option value="6">6 — tournament</option>
      </select>
      <button id="spawn" onclick={doSpawn}>spawn</button>
    </div>
    {#if spawnError}<div class="spawn-error">{spawnError}</div>{/if}
  </section>

  <section>
    <h2>agents</h2>
    <ul class="nav-list">
      {#each agentItems as a}
        <li
          class={$selected.kind === "agent" && $selected.id === a.id ? "selected" : ""}
          use:clickable={() => select("agent", a.id)}
        >
          <span class="dot {a.status}"></span>{a.name}
          <span class="sub">{a.iters}</span>
        </li>
      {:else}
        <li class="sub">none yet</li>
      {/each}
    </ul>
  </section>

  <section>
    <h2>tournaments</h2>
    <ul class="nav-list">
      {#each tournamentItems as t}
        <li
          class={$selected.kind === "tournament" && $selected.id === t.id ? "selected" : ""}
          use:clickable={() => select("tournament", t.id)}
        >🏆 {t.id}<span class="sub">{t.leader}</span></li>
      {:else}
        <li class="sub">none yet</li>
      {/each}
    </ul>
  </section>

  <section>
    <h2>external</h2>
    <ul class="nav-list">
      {#if externalCount}
        <li
          class={$selected.kind === "external" ? "selected" : ""}
          use:clickable={() => select("external", "")}
        >perf-vs &amp; ingest <span class="sub">{externalCount}</span></li>
      {:else}
        <li class="sub">none yet</li>
      {/if}
    </ul>
  </section>

  <section>
    <h2>sessions <button class="mini" onclick={loadSessions}>↻</button></h2>
    <ul class="nav-list">
      {#if $sessions === null}
        <li class="sub">failed to load</li>
      {:else if $sessions.length}
        {#each $sessions.slice(0, 12) as s}
          <li use:clickable={() => viewSession(s.sessionId)}>
            {s.sessionId}
            <span class="sub">{fmtWhen(s.startedMs)} · {s.events}ev</span>
          </li>
        {/each}
      {:else}
        <li class="sub">none</li>
      {/if}
    </ul>
  </section>

  <footer class="rail-foot">score = imp − 2·reg − 1.5·crit</footer>
</nav>
