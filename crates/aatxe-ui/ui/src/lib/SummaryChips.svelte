<script>
  // Compare-summary chips: reg / imp / neutral / new. Renders nothing when a
  // count is zero (matching the original summaryChips()).
  let { summary } = $props();
  const chips = $derived(summary
    ? [
        { n: summary.regressions, cls: "regression", label: "reg" },
        { n: summary.improvements, cls: "improvement", label: "imp" },
        { n: summary.neutrals, cls: "neutral", label: "neutral" },
        { n: summary.new, cls: "new", label: "new" },
      ].filter((c) => c.n)
    : []);
</script>

{#each chips as c}
  <span class="badge {c.cls}">{c.n} {c.label}</span>{" "}
{/each}
