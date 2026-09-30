<script lang="ts">
  // Settings > Help: report a problem, suggest something, or report a security problem privately.
  import { openReport } from "../lib/report";
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
</div>
