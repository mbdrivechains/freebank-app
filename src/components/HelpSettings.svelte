<script lang="ts">
  // Settings > Help: report a problem, suggest something, or report a security problem privately.
  import { openReport } from "../lib/report";
  import { waysOpen } from "../lib/ways";
</script>

<div class="card">
  <h2>Help</h2>
  <p class="muted small">
    Found something wrong, or have an idea? Tell FreeBank here: no account needed, or on GitHub if you have one.
  </p>
  <div class="row-actions">
    <button class="secondary" on:click={() => openReport("problem")}>Report a problem</button>
    <button class="secondary" on:click={() => openReport("idea")}>Suggest something</button>
    <button class="secondary" on:click={() => openReport("security")}>Security problem (private)</button>
  </div>
  <p class="muted small ways-line">What can you do with FreeBank? A guide for each goal: who lends, the risks, the steps.</p>
  <div class="row-actions">
    <button class="secondary" on:click={() => waysOpen.set("")}>Ways to use FreeBank</button>
  </div>
</div>

<style>
  .ways-line {
    margin-top: 16px;
  }
</style>
