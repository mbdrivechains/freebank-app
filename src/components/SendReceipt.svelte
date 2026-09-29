<script lang="ts">
  // A receipt that can speed its send up: TxReceipt, plus "Speed up" while a send from this app's
  // Send tab is unconfirmed and has change to pay a higher fee from. Speed up shows the speeds again
  // with the new fee (speed_up_quote), and confirms inside withUnlock (send_speed_up). The receipt
  // then follows the new transaction and lists the old one as replaced. Any other transaction reads
  // as TxReceipt alone, so App.svelte's receipts list uses this for every receipt, and History's
  // details (TxDetails) use it too.
  import { createEventDispatcher } from "svelte";
  import TxReceipt from "./TxReceipt.svelte";
  import { receipts, type ReceiptRow, type TxStatus } from "../lib/receipts";
  import { withUnlock } from "../lib/wallet";
  import { nice } from "../lib/errors";
  import { bumpedRows, ecxText, loadSendLog, rateText, sendLog, sends, type BumpQuote, type Bumped, type Speed } from "../lib/send";

  export let txid: string;
  export let what: string;
  export let sentAt: number = Date.now();
  export let rows: ReceiptRow[] = [];
  export let dismissible = true;
  export let collapsed = false;
  /** Its id in $receipts, when it is one of App.svelte's receipts: a Speed up updates it there too. */
  export let receiptId: number | null = null;
  /** Said while it is unconfirmed and can't be sped up, for a send the log doesn't know (TxDetails). */
  export let note = "";

  const dispatch = createEventDispatcher<{ close: void; status: TxStatus; bumped: Bumped }>();

  $: entry = $sendLog.find((e) => e.txid === txid) ?? null;
  $: offer = sends.canSpeedUp && entry !== null && !entry.max && entry.change > 0 && !entry.replaced_by;
  $: why = !entry
    ? note
    : entry.max
      ? "A Max send has no change to pay a higher fee from, so it can't be sped up."
      : entry.change <= 0
        ? "This send left no change to pay a higher fee from, so it can't be sped up."
        : "";

  let quote: BumpQuote | null = null;
  let pick: Speed = "next";
  let busy = false;
  let err = "";
  $: chosen = quote ? ((quote.same ? quote.choices.find((c) => c.ok) : quote.choices.find((c) => c.speed === pick)) ?? null) : null;

  // A new transaction (after a Speed up) starts without a quote.
  let quotedFor = txid;
  $: if (txid !== quotedFor) {
    quotedFor = txid;
    quote = null;
    err = "";
  }

  async function openQuote() {
    busy = true;
    err = "";
    try {
      quote = await sends.speedUpQuote(txid);
      pick = quote.choices.find((c) => c.ok)?.speed ?? "next";
    } catch (e) {
      err = nice(e);
    }
    busy = false;
  }

  async function bump() {
    if (!chosen || !chosen.ok || busy) return;
    const old = txid;
    const speed = chosen.speed;
    busy = true;
    err = "";
    try {
      const b = await withUnlock(() => sends.speedUp(old, speed), { what: "speed up this send" });
      const next = bumpedRows(rows, b);
      if (b.log_error) next.push({ label: "Not in the send log", value: b.log_error });
      quote = null;
      if (receiptId !== null) {
        receipts.update((list) => list.map((r) => (r.id === receiptId ? { ...r, txid: b.txid, rows: next } : r)));
      }
      rows = next;
      txid = b.txid;
      await loadSendLog();
      dispatch("bumped", b);
    } catch (e) {
      err = nice(e);
    }
    busy = false;
  }
</script>

<TxReceipt {txid} {what} {sentAt} {rows} {dismissible} {collapsed} on:close on:status>
  <svelte:fragment slot="actions" let:confirmations let:replaced>
    {#if confirmations === 0 && !replaced}
      {#if quote}
        <div class="su su-wide">
          <p class="su-title">Speed up with a higher fee, from your change</p>
          {#if quote.same && chosen}
            <p class="su-one">New fee <strong>{ecxText(chosen.fee)}</strong><span class="su-fee">{rateText(chosen.sat_per_vb)}</span></p>
          {:else}
            <div class="su-choices" role="radiogroup" aria-label="New speed">
              {#each quote.choices as c (c.speed)}
                <label class="su-choice" class:on={pick === c.speed} class:off={!c.ok}>
                  <input type="radio" name="speed-up-{quote.txid}" value={c.speed} bind:group={pick} disabled={!c.ok} />
                  <span>{c.label}</span>
                  <span class="su-fee">{ecxText(c.fee)} · {rateText(c.sat_per_vb)}</span>
                </label>
              {/each}
            </div>
          {/if}
          <p class="hint su-hint">
            The fee was {ecxText(quote.old_fee)}. The faster transaction replaces this one and gets a new transaction ID.
          </p>
          <div class="su-actions">
            <button type="button" on:click={bump} disabled={busy || !chosen?.ok}>{busy ? "Speeding up…" : "Confirm new fee"}</button>
            <button type="button" class="secondary" on:click={() => (quote = null)} disabled={busy}>Cancel</button>
          </div>
        </div>
      {:else if offer}
        <button type="button" class="secondary" on:click={openQuote} disabled={busy}>{busy ? "Working out the fee…" : "Speed up"}</button>
      {:else if why}
        <p class="hint su-wide su-why">{why}</p>
      {/if}
      {#if err}<p class="soft-error su-wide" role="alert">{err}</p>{/if}
    {/if}
  </svelte:fragment>
</TxReceipt>

<style>
  .su-wide {
    flex-basis: 100%;
  }
  .su {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
    background: var(--bg-inset);
  }
  .su-title {
    font-weight: 600;
    font-size: 14px;
  }
  .su-choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .su-choice,
  .su-one {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
  }
  .su-choice {
    padding: 8px 10px;
    border: 1px solid var(--border-color);
    border-radius: 8px;
    cursor: pointer;
  }
  .su-choice.on {
    border-color: var(--accent-color);
    background: var(--accent-tint);
  }
  .su-choice.off {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .su-choice input {
    accent-color: var(--accent-color);
    margin: 0;
  }
  .su-fee {
    margin-left: auto;
    color: var(--text-secondary);
    font-size: 12.5px;
    white-space: nowrap;
  }
  .su-hint,
  .su-why {
    margin: 0;
  }
  .su-actions {
    display: flex;
    gap: 8px;
  }
</style>
