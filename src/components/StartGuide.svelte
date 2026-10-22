<script lang="ts">
  // Home on a network with a faucet (easynet, v0.6.2): where a newcomer's first steps are, in the place beta's demo
  // card had (DemoGuide.svelte). Coins come from the faucet in Receive, not from a bot. "Got it" folds the card to one
  // line, remembered on this computer only (a convenience: it opens again if lost).
  import { network } from "../lib/network";
  import AskBox from "./AskBox.svelte";

  const KEY = "freebank.startGuide.folded";
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
  <button class="link-btn dg-open" on:click={() => fold(false)} data-testid="start-guide-open">Getting started on {$network.name} ›</button>
{:else}
  <div class="dg" data-testid="start-guide">
    <strong>Getting started on {$network.name}</strong>
    <p>{$network.name} is a test network: its coins have no value, and its faucet gives you some.</p>
    <ol>
      <li><b>Test coins:</b> Receive › Get {$network.name} coins sends a little sECX here, once a day.</li>
      <li><b>Pay someone:</b> Send, to another FreeBank address on {$network.name}. The payment confirms with the next block, about ten minutes.</li>
      <li><b>eCash too:</b> the eCash tab › Receive › Get {$network.name} coins sends ECX, and Deposit brings ECX onto FreeBank at par.</li>
      <li><b>Houses:</b> Credit lists the houses on {$network.name}, their notes and their bills.</li>
    </ol>
    <div class="dg-actions">
      <button class="ghost" on:click={() => fold(true)}>Got it</button>
    </div>
    <div class="dg-ask">
      <b>Stuck? Ask FreeBank:</b>
      <AskBox placeholder="How do I get test coins?" />
    </div>
  </div>
{/if}

<style>
  /* As DemoGuide's (its style is that component's own). */
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
