<script lang="ts">
  import { BASE_TICKER } from "../lib/brand";

  export let balance: number;
  export let onRefresh: () => Promise<void>;
  export let gramRate = 0; // sats per gram (launch scale); 0 = unknown

  let refreshing = false;

  async function handleRefresh() {
    refreshing = true;
    await onRefresh();
    refreshing = false;
  }

  // Money made metric: lead with the gram, settle in ECX.
  $: grams = gramRate > 0 ? (balance * 1e8) / gramRate : 0;
</script>

<div class="balance-card">
  <div class="balance-label">Balance</div>
  {#if gramRate > 0}
    <div class="balance-amount">
      <span class="sym">☉</span><span class="value">{grams.toFixed(6)}</span><span class="unit">g</span>
    </div>
    <div class="balance-sub">{balance.toFixed(8)} {BASE_TICKER} · launch scale, not enforced</div>
  {:else}
    <div class="balance-amount">
      <span class="value">{balance.toFixed(8)}</span><span class="unit">{BASE_TICKER}</span>
    </div>
  {/if}
  <button class="refresh-btn" on:click={handleRefresh} disabled={refreshing}>
    {refreshing ? "..." : "Refresh"}
  </button>
</div>

<style>
  .balance-card {
    background: radial-gradient(130% 150% at 0% 0%, #3b2e12 0%, #1b1e24 62%);
    border: 1px solid #3f3318;
    border-radius: 16px;
    padding: 24px;
    color: white;
    text-align: center;
    margin-bottom: 16px;
  }

  .balance-label {
    font-size: 14px;
    opacity: 0.8;
    margin-bottom: 8px;
  }

  .balance-amount {
    font-size: 32px;
    font-weight: bold;
    margin-bottom: 6px;
  }

  .balance-amount .sym {
    margin-right: 8px;
    opacity: 0.95;
  }

  .balance-amount .value {
    font-family: monospace;
  }

  .balance-amount .unit {
    font-size: 18px;
    opacity: 0.8;
    margin-left: 8px;
  }

  .balance-sub {
    font-size: 13px;
    opacity: 0.75;
    font-family: monospace;
    margin-bottom: 16px;
  }

  .refresh-btn {
    background: rgba(255, 255, 255, 0.2);
    border: 1px solid rgba(255, 255, 255, 0.3);
    color: white;
    padding: 8px 16px;
    border-radius: 8px;
    cursor: pointer;
  }

  .refresh-btn:hover {
    background: rgba(255, 255, 255, 0.3);
  }

  .refresh-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
