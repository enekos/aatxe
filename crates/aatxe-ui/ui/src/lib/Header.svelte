<script>
  import { connection, viewingHistory, backToLive } from "./store.js";
  let { session } = $props();

  const meta = $derived(session
    ? `${session.sessionId} · ${session.repoRoot} · base ${session.baseRef}@${(session.baseSha || "").slice(0, 8)} · bench ${session.benchLabel}`
    : "waiting for session…");
</script>

<header>
  <div class="brand">aatxe <span class="brand-ui">ui</span></div>
  <div class="session-meta">{meta}</div>
  <div class="header-right">
    {#if $viewingHistory}
      <span class="history-banner">viewing past session
        <button onclick={backToLive}>back to live</button>
      </span>
    {/if}
    <span class="conn {$connection}" title="event stream">●</span>
  </div>
</header>
