<script lang="ts">
  // The header's Ask (v0.4.6): Ask FreeBank from any screen. Esc or Close shuts it.
  import { createEventDispatcher } from "svelte";
  import AskBox from "./AskBox.svelte";

  const dispatch = createEventDispatcher<{ close: void }>();
</script>

<svelte:window on:keydown={(e) => e.key === "Escape" && dispatch("close")} />

<div class="ask-backdrop" on:click|self={() => dispatch("close")} role="presentation">
  <div class="ask-dialog" role="dialog" aria-modal="true" aria-label="Ask FreeBank">
    <div class="ask-head">
      <h2>Ask FreeBank</h2>
      <button class="ghost" on:click={() => dispatch("close")}>Close</button>
    </div>
    <p class="muted small">
      Ask about any screen: sending, deposits, notes, houses, bills, your phone.
    </p>
    <AskBox focus placeholder="How does a deposit work?" />
  </div>
</div>

<style>
  .ask-backdrop {
    position: fixed;
    inset: 0;
    z-index: 45; /* above the pages and the watermark, under the phone's prompts (50) and an unlock (60) */
    background: rgba(5, 7, 10, 0.6);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 64px 12px 12px;
  }
  .ask-dialog {
    width: 100%;
    max-width: 420px;
    max-height: calc(100vh - 80px);
    overflow-y: auto;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius);
    padding: 18px;
  }
  .ask-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .ask-head h2 {
    font-size: 17px;
    margin: 0;
  }
</style>
