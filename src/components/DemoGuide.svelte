<script lang="ts">
  // Home in demo mode (v0.4.5): what the demo is for and where each step happens. On the beta the Telegram bot is
  // the cashier: it gives the play money and the houses lend through it; Send to your app brings its notes here.
  // "Got it" folds the card to one line, remembered on this computer only (a convenience: it opens again if lost).
  import { BETA_BOT_URL } from "../lib/brand";
  import { openUrl } from "../lib/node";
  import AskBox from "./AskBox.svelte";

  const KEY = "freebank.demoGuide.folded";
  let folded = false;
  try {
    folded = localStorage.getItem(KEY) === "1";
  } catch {
    /* no storage: the card shows */
  }
  function fold(on: boolean) {
    folded = on;
    try {
      if (on) localStorage.setItem(KEY, "1");
      else localStorage.removeItem(KEY);
    } catch {
      /* remembered for this session only */
    }
  }
</script>

{#if folded}
  <button class="link-btn dg-open" on:click={() => fold(false)} data-testid="demo-guide-open">How the demo works ›</button>
{:else}
  <div class="dg" data-testid="demo-guide">
    <strong>How the demo works</strong>
    <p>On the beta, FreeBank's Telegram bot is the cashier: it gives the play money, and the houses lend there.</p>
    <ol>
      <li><b>Play money:</b> Receive › Get play money.</li>
      <li><b>Borrow:</b> in the bot, Borrow. A house buys your bill and pays you in its notes.</li>
      <li><b>Bring them here:</b> in the bot, Wallet › Send to your app, with the address Receive shows.</li>
      <li><b>Use them:</b> Credit › Notes. Pay someone with them, or Demand: the house pays you sECX at par, usually within the hour.</li>
    </ol>
    <div class="dg-actions">
      <button on:click={() => openUrl(BETA_BOT_URL)}>Open the bot ↗</button>
      <button class="ghost" on:click={() => fold(true)}>Got it</button>
    </div>
    <div class="dg-ask">
      <b>Stuck? Ask FreeBank:</b>
      <AskBox placeholder="What do I do after Get play money?" />
    </div>
  </div>
{/if}

<style>
  .dg {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 14px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(217, 164, 65, 0.35);
    background: var(--accent-tint);
    font-size: 13.5px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .dg strong {
    color: var(--text-color);
    font-size: 14.5px;
  }
  .dg b {
    color: var(--text-color);
    font-weight: 600;
  }
  .dg ol {
    margin: 0;
    padding-left: 20px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .dg-actions {
    display: flex;
    gap: 8px;
  }
  .dg-actions button {
    font-size: 13.5px;
    padding: 8px 16px;
  }
  .dg-ask {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
    padding-top: 10px;
    border-top: 1px solid rgba(217, 164, 65, 0.25);
  }
  .dg-open {
    display: block;
    margin: 0 0 12px;
    font-size: 13px;
  }
</style>
