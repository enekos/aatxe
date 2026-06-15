<script>
  import { onMount } from "svelte";
  import { view, selected, connect, loadSessions } from "./lib/store.js";
  import Header from "./lib/Header.svelte";
  import Rail from "./lib/Rail.svelte";
  import AgentView from "./lib/AgentView.svelte";
  import TournamentView from "./lib/TournamentView.svelte";
  import ExternalView from "./lib/ExternalView.svelte";
  import Notices from "./lib/Notices.svelte";

  onMount(() => {
    connect();
    loadSessions();
  });

  // Resolve the current selection against the current view, mirroring the
  // original renderMain() dispatch (selection only renders if it still exists).
  const main = $derived.by(() => {
    const s = $selected;
    if (s.kind === "agent" && $view.agents.has(s.id)) {
      return { kind: "agent", id: s.id };
    }
    if (s.kind === "tournament" && $view.tournaments.has(s.id)) {
      return { kind: "tournament", id: s.id };
    }
    if (s.kind === "external") {
      return { kind: "external" };
    }
    return { kind: "empty" };
  });
</script>

<Header session={$view.session} />

<div class="layout">
  <Rail />

  <main>
    {#if main.kind === "agent"}
      {#key main.id}
        <AgentView agentId={main.id} />
      {/key}
    {:else if main.kind === "tournament"}
      <TournamentView
        tournamentId={main.id}
        onselect={(sel) => selected.set(sel)}
      />
    {:else if main.kind === "external"}
      <ExternalView />
    {:else}
      <div class="empty">
        <p>🐂 spawn an agent, or run <code>aatxe perf-vs</code> in a terminal and watch it appear.</p>
      </div>
      <Notices notices={$view.notices} />
    {/if}
  </main>
</div>
