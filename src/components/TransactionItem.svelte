<script lang="ts">
  import type { Transaction } from "../lib/api";
  import { BASE_TICKER } from "../lib/brand";
  import { CONFIRMED_AT } from "../lib/receipts";

  export let tx: Transaction;
  /** A faster send replaced it (Speed up). */
  export let replaced = false;

  // A block this wallet won by bidding (its coinbase): not a send. It pays the block's fees, often nothing on beta.
  $: won = tx.category === "generate" || tx.category === "immature" || tx.category === "orphan";
  $: isReceive = tx.category === "receive" || (won && tx.amount > 0);
  $: wonLabel =
    tx.category === "orphan" ? "Won block, not kept" : `Won FreeBank block${tx.blockheight ? ` ${tx.blockheight}` : ""}`;
  $: formattedAmount = won && tx.amount === 0 ? "no fees" : (isReceive ? "+" : "") + tx.amount.toFixed(8);
  $: formattedDate = new Date(tx.time * 1000).toLocaleDateString();
</script>

<div class="tx-item" class:receive={isReceive && !won} class:send={!isReceive && !won} class:won>
  <div class="tx-icon">
    {won ? "★" : isReceive ? "↓" : "↑"}
  </div>
  <div class="tx-details">
    <div class="tx-type">{won ? wonLabel : isReceive ? "Received" : "Sent"}</div>
    {#if tx.category === "immature" && tx.amount > 0}<div class="tx-date">Spendable after 100 blocks</div>{/if}
    <div class="tx-date">{formattedDate}</div>
    {#if tx.address}
      <div class="tx-address">{tx.address.slice(0, 12)}...</div>
    {/if}
  </div>
  <div class="tx-amount" class:positive={isReceive} class:negative={!isReceive && !won}>
    {formattedAmount}{won && tx.amount === 0 ? "" : ` ${BASE_TICKER}`}
  </div>
  <!-- Confirmed at 3, as on the receipts (CONFIRMED_AT) -->
  <div class="tx-confirmations" class:unconfirmed={!replaced && tx.confirmations < CONFIRMED_AT}>
    {replaced ? "Replaced" : tx.confirmations < CONFIRMED_AT ? `${tx.confirmations} conf` : "Confirmed"}
  </div>
</div>

<style>
  .tx-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px;
    border-bottom: 1px solid var(--border-color, #333);
  }

  .tx-icon {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
  }

  .tx-item.receive .tx-icon {
    background: rgba(76, 175, 80, 0.2);
    color: #4caf50;
  }

  .tx-item.send .tx-icon {
    background: rgba(244, 67, 54, 0.2);
    color: #f44336;
  }

  .tx-item.won .tx-icon {
    background: rgba(255, 193, 7, 0.18);
    color: #c79100;
  }

  .tx-item.won .tx-amount {
    color: var(--text-secondary);
  }

  .tx-details {
    flex: 1;
    min-width: 0;
  }

  .tx-type {
    font-weight: 500;
  }

  .tx-date {
    font-size: 12px;
    opacity: 0.6;
  }

  .tx-address {
    font-size: 11px;
    font-family: monospace;
    opacity: 0.5;
  }

  .tx-amount {
    font-family: monospace;
    font-weight: 500;
    text-align: right;
  }

  .tx-amount.positive {
    color: #4caf50;
  }

  .tx-amount.negative {
    color: #f44336;
  }

  .tx-confirmations {
    font-size: 11px;
    opacity: 0.6;
    min-width: 60px;
    text-align: right;
  }

  .tx-confirmations.unconfirmed {
    color: #ff9800;
  }
</style>
