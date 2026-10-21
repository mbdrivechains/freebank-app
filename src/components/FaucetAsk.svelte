<script lang="ts">
  // "Get easynet coins" (v0.6.1): on a network with a faucet, Receive asks it to pay this address, as "Get play money"
  // opens the bot on beta. Home's Receive gets sECX on FreeBank, the eCash tab's ECX on easynet itself. Rust asks
  // (faucet.rs); only the address goes.
  import { tauriInvoke } from "../lib/api";
  import { network } from "../lib/network";
  import { openUrl } from "../lib/node";

  export let address: string;
  /** What this address gets: "sECX" (a FreeBank address) or "ECX" (an easynet one). */
  export let coin: "sECX" | "ECX";

  interface Paid {
    txid: string;
    amount: string;
    coin: string;
  }

  let busy = false;
  let error = "";
  let paid: Paid | null = null;

  async function ask() {
    busy = true;
    error = "";
    paid = null;
    try {
      paid = (await tauriInvoke("faucet_ask", { address })) as Paid;
    } catch (e) {
      error = String(e);
    }
    busy = false;
  }

  // FreeBank's explorer shows sECX payments; easynet's own coin shows in the eCash tab's list.
  $: link = paid && paid.coin === "sECX" && $network.explorer ? `${$network.explorer}/tx/${paid.txid}` : null;
</script>

<div class="play-money" data-testid="faucet">
  <p class="small">
    <strong>Test coins:</strong> the {$network.name} faucet sends a little {coin} to this address, once a day. They have
    no value.
  </p>
  <button class="secondary" on:click={ask} disabled={busy || !address}>{busy ? "Asking…" : `Get ${$network.name} coins`}</button>
  {#if paid}
    <p class="small" data-testid="faucet-paid">
      Sent {paid.amount} {paid.coin}. It arrives with the next block or two.
      {#if link}<button class="link-btn inline" on:click={() => link && openUrl(link)}>See it ↗</button>{/if}
    </p>
  {/if}
  {#if error}<p class="soft-error" data-testid="faucet-error">{error}</p>{/if}
  <p class="small muted">This sends the address, nothing else, to the faucet.</p>
</div>

<style>
  /* As ReceivePanel's "Get play money" (its style is that component's own). */
  .play-money {
    margin-top: 0.75rem;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
  }
</style>
