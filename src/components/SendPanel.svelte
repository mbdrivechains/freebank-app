<script lang="ts">
  // The Send tab (v0.2.0): ECX to an address, or Max (everything that can be spent, with the fee
  // taken out of it), at a speed the user picks, with the fee and the total shown before Confirm.
  // Review builds and funds the send in Rust and holds it five minutes (send_prepare); Confirm signs
  // and sends exactly that inside withUnlock (send_confirm). The receipt then shows under the tabs,
  // with Speed up (SendReceipt), and `sent` tells App.svelte to go to Home.
  import { createEventDispatcher, onMount } from "svelte";
  import Notice from "./Notice.svelte";
  import { BASE_TICKER } from "../lib/brand";
  import { ECX_PROBLEM, fmtEcx, parseEcx } from "../lib/amount";
  import { nice } from "../lib/errors";
  import { withUnlock } from "../lib/wallet";
  import { showReceipt } from "../lib/receipts";
  import {
    ecxText,
    loadSendLog,
    rateText,
    sendRows,
    sends,
    shortAddr,
    type FeeChoices,
    type PreparedSend,
    type Speed,
  } from "../lib/send";

  /** The wallet's balance in ECX, for "Available". */
  export let balance = 0;

  const dispatch = createEventDispatcher<{ sent: { txid: string } }>();

  let address = "";
  let amountText = "";
  let max = false;
  let speed: Speed = "next";
  let fees: FeeChoices | null = null;
  let feesError = "";
  let feesLoading = false;
  let review: PreparedSend | null = null;
  let preparing = false;
  let confirming = false;
  let error = "";
  let expired = false;

  $: amountSats = max ? null : parseEcx(amountText);
  $: amountProblem = !max && amountText.trim() !== "" && amountSats === null ? ECX_PROBLEM : "";
  $: ready = address.trim() !== "" && (max || amountSats !== null);
  // All three speeds cost the same: one line, no choice.
  $: one = fees?.same ? fees.choices[0] : null;

  async function loadFees(refresh = false) {
    if (!sends.canChooseFee) return;
    feesLoading = true;
    try {
      fees = await sends.feeChoices(refresh);
      feesError = "";
    } catch (e) {
      feesError = nice(e) || "The network's fees couldn't be read.";
    }
    feesLoading = false;
  }

  onMount(() => {
    loadFees();
  });

  function toggleMax() {
    max = !max;
    if (max) amountText = "";
    error = "";
  }

  async function prepare() {
    if (!ready || preparing) return;
    preparing = true;
    error = "";
    expired = false;
    const r = { address, amount: amountSats, max, speed: (one ? one.speed : speed) as Speed };
    try {
      // Funding can need a fresh change address, and a locked wallet whose key pool has run dry
      // refills it on unlock: then this asks for the passphrase.
      review = await withUnlock(() => sends.prepare(r), { what: "prepare this send" });
    } catch (e) {
      error = nice(e);
    }
    preparing = false;
  }

  async function confirm() {
    if (!review || confirming) return;
    const r = review;
    confirming = true;
    error = "";
    try {
      const sent = await withUnlock(() => sends.confirm(r.id), { what: `send ${ecxText(r.amount)}` });
      const rows = sendRows(sent);
      if (sent.log_error) rows.push({ label: "Not in the send log", value: sent.log_error });
      await loadSendLog();
      showReceipt({
        txid: sent.txid,
        what: `Sent ${ecxText(sent.amount)} to ${shortAddr(sent.address)}`,
        rows,
        sentAt: sent.time * 1000,
      });
      review = null;
      address = "";
      amountText = "";
      max = false;
      dispatch("sent", { txid: sent.txid });
    } catch (e) {
      error = nice(e);
      expired = /expired/i.test(error);
    }
    confirming = false;
  }

  function change() {
    review = null;
    error = "";
    expired = false;
    loadFees();
  }
</script>

{#if review}
  <div class="card sp-review">
    <h2>Review your send</h2>
    <dl class="facts sp-facts">
      <div><dt>To</dt><dd class="mono">{review.address}</dd></div>
      <div><dt>Amount</dt><dd>{ecxText(review.amount)}</dd></div>
      {#if review.fee !== null}
        <div><dt>Fee</dt><dd>{ecxText(review.fee)}</dd></div>
        <div><dt>Speed</dt><dd>{review.label} · {rateText(review.sat_per_vb)}</dd></div>
        <div class="sp-total"><dt>Total</dt><dd>{ecxText(review.total ?? review.amount + review.fee)}</dd></div>
      {:else}
        <div><dt>Fee</dt><dd>Set by your node as it sends</dd></div>
      {/if}
    </dl>
    {#if review.max}
      <p class="hint">
        Max sends everything you can spend, with the fee taken out of it. It leaves no change, so it can't be sped up later.
      </p>
      {#if review.total !== null && review.total < Math.round(balance * 1e8)}
        <p class="hint">
          Coins still confirming aren't in it, such as the change of a send you sped up. They can be sent once they confirm.
        </p>
      {/if}
    {:else if review.fee !== null && review.change === 0}
      <p class="hint">This send leaves no change (it would be too small to keep), so it can't be sped up later.</p>
    {/if}
    {#if error}
      <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
    {/if}
    <div class="row-actions">
      {#if expired}
        <button type="button" on:click={prepare} disabled={preparing}>{preparing ? "Working out the fee…" : "Review again"}</button>
      {:else}
        <button type="button" on:click={confirm} disabled={confirming}>{confirming ? "Sending…" : "Confirm and send"}</button>
      {/if}
      <button type="button" class="secondary" on:click={change} disabled={confirming || preparing}>Change</button>
    </div>
  </div>
{:else}
  <div class="card">
    <h2>Send {BASE_TICKER}</h2>
    <form class="form" on:submit|preventDefault={prepare}>
      <label>
        Address
        <input type="text" bind:value={address} placeholder="X…" spellcheck="false" autocomplete="off" />
      </label>
      <div class="field">
        <label class="sp-label" for="send-amount">Amount ({BASE_TICKER})</label>
        <div class="input-with-btn">
          <input
            id="send-amount"
            type="text"
            inputmode="decimal"
            autocomplete="off"
            bind:value={amountText}
            placeholder={max ? "Everything you can spend" : "0.00"}
            disabled={max}
            aria-invalid={amountProblem ? "true" : undefined}
          />
          {#if sends.canChooseFee}
            <button type="button" class="secondary sp-max" class:on={max} aria-pressed={max} on:click={toggleMax}>Max</button>
          {/if}
        </div>
        <p class="hint sp-avail">
          {max ? "Everything goes, less the fee. " : ""}Available: {fmtEcx(Math.round(balance * 1e8))} {BASE_TICKER}
        </p>
        {#if amountProblem}<p class="field-problem">{amountProblem}</p>{/if}
      </div>

      {#if sends.canChooseFee}
        <div class="field">
          <span class="sp-label">Speed</span>
          {#if fees && one}
            <div class="sp-one"><strong>{one.label}</strong><span class="sp-rate">{rateText(one.sat_per_vb)}</span></div>
            <p class="hint">
              {fees.basis === "none"
                ? "Your node has no fee data yet, so this is the lowest fee. While the network is busy it may take more than one block."
                : "The network is quiet, so the lowest fee gets into the next block."}
            </p>
          {:else if fees}
            <div class="sp-choices" role="radiogroup" aria-label="Speed">
              {#each fees.choices as c (c.speed)}
                <label class="sp-choice" class:on={speed === c.speed}>
                  <input type="radio" name="send-speed" value={c.speed} bind:group={speed} />
                  <span>{c.label}</span>
                  <span class="sp-rate">{rateText(c.sat_per_vb)}</span>
                </label>
              {/each}
            </div>
            {#if fees.basis === "partial"}
              <p class="hint">Your node has fee data for only some speeds; the others use the lowest fee.</p>
            {/if}
          {:else if feesLoading}
            <p class="hint">Checking the network's fees…</p>
          {:else if feesError}
            <p class="hint">
              {feesError}
              <button type="button" class="link-btn" on:click={() => loadFees(true)}>Try again</button>
            </p>
          {/if}
          <p class="hint">You see the exact fee before you confirm.</p>
        </div>
      {:else}
        <p class="hint">
          The browser version sends with your node's default fee. The desktop app shows the fee first, and has Max and a
          choice of speed.
        </p>
      {/if}

      {#if error}
        <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
      {/if}
      <button type="submit" disabled={!ready || preparing}>{preparing ? "Working out the fee…" : "Review"}</button>
    </form>
  </div>
{/if}

<style>
  .sp-label {
    font-size: 13px;
    color: var(--text-secondary);
  }
  .field .sp-label {
    display: block;
  }
  .sp-max {
    width: auto;
    padding: 0 16px;
    font-size: 14px;
    font-weight: 600;
  }
  .sp-max.on {
    border-color: var(--accent-color);
    background: var(--accent-tint);
    color: var(--accent-color);
  }
  .sp-avail {
    margin-top: 0;
  }
  .sp-choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .sp-choice,
  .sp-one {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
    background: var(--bg-inset);
    font-size: 14px;
    color: var(--text-color);
  }
  .sp-choice {
    cursor: pointer;
  }
  .sp-choice.on {
    border-color: var(--accent-color);
    background: var(--accent-tint);
  }
  /* A radio, not a text field: undo the form's input style (app.css, ".form input"). */
  .sp-choice input {
    flex: none;
    width: auto;
    margin: 0;
    padding: 0;
    border: none;
    background: none;
    box-shadow: none;
    accent-color: var(--accent-color);
  }
  .sp-choice input:focus-visible {
    outline: 2px solid var(--accent-color);
    outline-offset: 2px;
  }
  .sp-one strong {
    font-weight: 600;
  }
  .sp-rate {
    margin-left: auto;
    color: var(--text-secondary);
    font-size: 13px;
    white-space: nowrap;
  }
  .sp-facts {
    margin-top: 0;
    margin-bottom: 12px;
  }
  .sp-total {
    padding-top: 8px;
    border-top: 1px solid var(--border-color);
    font-weight: 600;
  }
  .sp-review .hint {
    margin-bottom: 12px;
  }
</style>
