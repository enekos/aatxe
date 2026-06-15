<script>
  // Last 8 notices, newest at the bottom. Renders nothing when empty.
  let { notices = [] } = $props();
  const recent = $derived(notices.slice(-8));
</script>

{#if recent.length}
  <div class="card">
    <h3>notices</h3>
    <div class="notices">
      {#each recent as n}
        <div>{n.message}</div>
      {/each}
    </div>
  </div>
{/if}
