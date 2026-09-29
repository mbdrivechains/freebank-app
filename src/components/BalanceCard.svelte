<script lang="ts">
  import { onDestroy } from "svelte";
  import { BASE_TICKER } from "../lib/brand";
  import { fmtEcx } from "../lib/amount";
  import { getPending, type Pending } from "../lib/deposit";

  export let balance: number;
  export let onRefresh: () => Promise<void>;

  let refreshing = false;

  async function handleRefresh() {
    refreshing = true;
    await onRefresh();
    refreshing = false;
    loadPending();
  }

  // The pending line (v0.2.0): getbalance counts spendable coins only, so a payment on its way in, or a
  // deposit in its first FreeBank block, shows here (getwalletinfo). Read with each balance, and every 15 s.
  let pending: Pending = { unconfirmed: 0, immature: 0 };
  async function loadPending() {
    try {
      pending = await getPending();
    } catch {
      // A busy or restarting node: keep the last answer.
    }
  }
  $: balance, loadPending();
  const timer = setInterval(loadPending, 15000);
  onDestroy(() => clearInterval(timer));
</script>

<div class="balance-card">
  <div class="balance-label">Balance</div>
  <div class="balance-amount">
    <span class="value">{fmtEcx(Math.round(balance * 1e8))}</span><span class="unit">{BASE_TICKER}</span>
  </div>
  {#if pending.unconfirmed > 0 || pending.immature > 0}
    <div class="pending">
      {#if pending.unconfirmed > 0}
        <div><span class="mono">+{fmtEcx(pending.unconfirmed)} {BASE_TICKER}</span> on its way: waiting for a FreeBank block</div>
      {/if}
      {#if pending.immature > 0}
        <div><span class="mono">+{fmtEcx(pending.immature)} {BASE_TICKER}</span> arriving: a deposit or new coins, spendable after the next block</div>
      {/if}
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
    margin-bottom: 16px;
  }

  .balance-amount .value {
    font-family: monospace;
  }

  .balance-amount .unit {
    font-size: 18px;
    opacity: 0.8;
    margin-left: 8px;
  }


  .pending {
    margin: -6px 0 16px;
    font-size: 12.5px;
    line-height: 1.5;
    opacity: 0.85;
  }

  .pending .mono {
    color: #f3d38b;
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
