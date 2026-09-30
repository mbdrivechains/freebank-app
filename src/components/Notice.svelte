<script lang="ts">
  // A message in the page, instead of alert() (which does nothing on macOS). Info is announced
  // politely (role status), an error at once (role alert). The parent decides when it goes:
  //   {#if error}<Notice kind="error" message={error} on:dismiss={() => (error = "")} />{/if}
  // Richer content goes in the default slot instead of `message`.
  // An error given as `message` offers "Report this", which opens the report dialog with it (lib/report.ts).
  import { createEventDispatcher } from "svelte";
  import { api } from "../lib/api";
  import { openReport } from "../lib/report";

  export let kind: "info" | "error" = "info";
  export let message = "";
  /** Show the × that dispatches `dismiss`. */
  export let dismissible = true;
  /** Offer "Report this" on an error. */
  export let reportable = true;

  const canReport = !api.isPWA();

  const dispatch = createEventDispatcher<{ dismiss: void }>();
</script>

<div class="notice notice-{kind}" role={kind === "error" ? "alert" : "status"}>
  <div class="notice-text">
    <slot>{message}</slot>
    {#if kind === "error" && message && reportable && canReport}
      <button
        type="button"
        class="link-btn notice-report"
        on:click={() => openReport("problem", `FreeBank said: “${message}”\n\nWhat I was doing: `)}>Report this</button
      >
    {/if}
  </div>
  {#if dismissible}
    <button type="button" class="ghost notice-x" aria-label="Dismiss" on:click={() => dispatch("dismiss")}>×</button>
  {/if}
</div>

<style>
  .notice-report {
    margin-left: 6px;
    font-size: 12.5px;
  }
  .notice {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 14px;
    padding: 10px 12px;
    border-radius: 10px;
    font-size: 13.5px;
    line-height: 1.45;
  }
  .notice-error {
    color: #e9b3ad;
    background: rgba(229, 116, 106, 0.08);
    border: 1px solid rgba(229, 116, 106, 0.25);
  }
  .notice-info {
    color: var(--text-color);
    background: var(--accent-tint);
    border: 1px solid rgba(217, 164, 65, 0.3);
  }
  .notice-text {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .notice-x {
    padding: 0 4px;
    font-size: 16px;
    line-height: 1.2;
  }
</style>
