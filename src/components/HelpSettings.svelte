<script lang="ts">
  // Settings > Help: report a problem, suggest something, or report a security problem privately; the Ways guides; and
  // About and legal, the full notice (v0.7.0, legal's November 2026 text, the same as NOTICE.md).
  import { openReport } from "../lib/report";
  import { waysOpen } from "../lib/ways";
  import { NOTICE_F } from "../lib/notice";
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
  <details class="about" data-testid="about-legal">
    <summary>About and legal</summary>
    {#each NOTICE_F as s}
      <h4>{s.title}</h4>
      {#each s.text as t}<p>{t}</p>{/each}
      {#if s.list}<ul>{#each s.list as l}<li>{l}</li>{/each}</ul>{/if}
      {#if s.after}<p>{s.after}</p>{/if}
    {/each}
  </details>
</div>

<style>
  .ways-line {
    margin-top: 16px;
  }
  .about {
    margin-top: 16px;
  }
  .about summary {
    cursor: pointer;
    color: var(--text-secondary);
    font-size: 14px;
  }
  .about h4 {
    margin: 14px 0 4px;
    font-size: 14px;
  }
  .about p,
  .about li {
    margin: 0 0 6px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .about ul {
    margin: 0 0 6px;
    padding-left: 20px;
  }
</style>
