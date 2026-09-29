<script lang="ts">
  // A transaction's details over the page, opened from a row on Home or in History: its receipt
  // (SendReceipt) with the live status, and Speed up for an unconfirmed send from this Send tab. It
  // follows a Speed up to the new transaction. The ×, Escape or a click outside closes it.
  import { createEventDispatcher } from "svelte";
  import SendReceipt from "./SendReceipt.svelte";
  import Notice from "./Notice.svelte";
  import { api, type WalletTx } from "../lib/api";
  import { nice } from "../lib/errors";
  import { unlockRequest } from "../lib/wallet";
  import { describeTx, sendLog, type Bumped } from "../lib/send";

  export let txid: string;

  const dispatch = createEventDispatcher<{ close: void; changed: void }>();

  let tx: WalletTx | null = null;
  let error = "";
  let asked = "";
  $: if (txid !== asked) load(txid);

  async function load(id: string) {
    asked = id;
    error = "";
    try {
      const t = await api.getTransaction(id);
      if (id === asked) tx = t;
    } catch (e) {
      if (id === asked) error = nice(e) || "This transaction couldn't be read.";
    }
  }

  // The last one read stays up while the next loads (after a Speed up), so nothing flickers.
  $: view = tx ? describeTx(tx, $sendLog) : null;

  function bumped(e: CustomEvent<Bumped>) {
    txid = e.detail.txid;
    dispatch("changed");
  }

  function onKey(e: KeyboardEvent) {
    // Escape in the passphrase prompt belongs to the prompt.
    if (e.key === "Escape" && !$unlockRequest) dispatch("close");
  }
</script>

<svelte:window on:keydown={onKey} />

<!-- svelte-ignore a11y-click-events-have-key-events -->
<div class="txd-back" role="presentation" on:click|self={() => dispatch("close")}>
  <div class="txd" role="dialog" aria-modal="true" aria-label="Transaction details">
    {#if view}
      <SendReceipt
        txid={view.txid}
        what={view.what}
        sentAt={view.sentAt}
        rows={view.rows}
        note={view.note}
        on:close={() => dispatch("close")}
        on:bumped={bumped}
      />
    {:else if error}
      <Notice kind="error" message={error} on:dismiss={() => dispatch("close")} />
    {:else}
      <div class="card"><p class="muted">Reading the transaction…</p></div>
    {/if}
  </div>
</div>

<style>
  .txd-back {
    position: fixed;
    inset: 0;
    z-index: 40; /* under the phone's prompts (50) and the passphrase prompt (60) */
    display: flex;
    justify-content: center;
    align-items: flex-start;
    padding: 24px 16px;
    overflow-y: auto;
    background: rgba(0, 0, 0, 0.6);
  }
  .txd {
    width: 100%;
    max-width: 488px;
  }
</style>
