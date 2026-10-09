<script lang="ts">
  // Home, Withdraw (v0.3.0 "In and out"): FreeBank sECX back to eCash
  // through the peg, at par. Trustless but very slow, so a warning screen comes before it (if
  // they choose it, they wait), with Cancel first. To a fresh address of the app's own eCash
  // wallet, or a pasted one with the lookalike warning. A withdrawal can be cancelled while it waits for a bundle.
  // With a money changer set up (Settings), a second card: sell to the changer, fast, below par.
  import { onDestroy, onMount } from "svelte";
  import { fmtEcx, parseEcx } from "../lib/amount";
  import { ecashStatus } from "../lib/ecash";
  import { nice } from "../lib/errors";
  import {
    changerInfo,
    changerLine,
    changerOrders,
    changerPay,
    changerQuote,
    pct,
    type ChangerInfo,
    type ChangerOrder,
    type ChangerQuote,
    withdrawCancel,
    withdrawConfirm,
    withdrawList,
    withdrawPrepare,
    withdrawalLine,
    type Withdrawal,
    type WithdrawQuote,
  } from "../lib/inout";
  import { withUnlock } from "../lib/wallet";

  /** The FreeBank balance, sECX (App.svelte's number in coins). */
  export let balance: number | null = null;

  const ecx = (sats: number) => `${fmtEcx(sats)} sECX`;

  let amount = "";
  let toOwn = true;
  let ownReady = false;
  let pasted = "";
  let quote: WithdrawQuote | null = null;
  let warned = false;
  let busy = false;
  let error = "";
  let done = "";
  let list: Withdrawal[] = [];
  let cancelling = "";
  let timer: ReturnType<typeof setInterval> | undefined;
  // The changer: what it takes now, its quote for this amount, and its orders.
  let changer: ChangerInfo | null = null;
  let cq: ChangerQuote | null = null;
  let cqError = "";
  let orders: ChangerOrder[] = [];

  async function load() {
    try {
      list = await withdrawList();
      // The note about the last one goes once it's settled one way or the other.
      if (done && list.length && !["pending", "waiting"].includes(list[0].state)) done = "";
    } catch {
      // The list waits for the next look.
    }
    if (changer) {
      try {
        orders = (await changerOrders()).filter((o) => o.side === "out");
      } catch {
        // as above
      }
    }
  }

  onMount(async () => {
    try {
      ownReady = (await ecashStatus()).state === "ready";
    } catch {
      ownReady = false;
    }
    toOwn = ownReady;
    try {
      changer = await changerInfo();
    } catch {
      changer = null;
    }
    load();
    timer = setInterval(load, 20_000);
  });
  onDestroy(() => clearInterval(timer));

  async function prepare() {
    error = "";
    done = "";
    if (parseEcx(amount) === null) {
      error = "Enter an amount in sECX above zero, with at most 8 decimal places.";
      return;
    }
    if (!toOwn && !pasted.trim()) {
      error = "Paste the eCash address to withdraw to.";
      return;
    }
    busy = true;
    cq = null;
    cqError = "";
    const to = toOwn ? null : pasted.trim();
    const fast =
      changer && !changer.paused
        ? changerQuote("out", amount.trim(), to).then(
            (q) => (cq = q),
            (e) => (cqError = nice(e)),
          )
        : Promise.resolve();
    try {
      quote = await withdrawPrepare(amount.trim(), to);
      warned = false;
    } catch (e) {
      error = nice(e);
    }
    await fast;
    busy = false;
  }

  async function sellFast() {
    if (!cq) return;
    busy = true;
    error = "";
    try {
      const id = cq.id;
      await withUnlock(() => changerPay(id, null), { what: "pay the changer" });
      done = `${ecx(cq.amount)} paid to the changer. It pays ${fmtEcx(cq.payout)} ECX once your payment is in a FreeBank block.`;
      quote = null;
      cq = null;
      amount = "";
      await load();
    } catch (e) {
      error = nice(e);
      cq = null;
    }
    busy = false;
  }

  async function confirm() {
    if (!quote) return;
    busy = true;
    error = "";
    try {
      const id = quote.id;
      await withUnlock(() => withdrawConfirm(id), { what: "withdraw" });
      done = `${ecx(quote.sats)} is on its way to eCash. It waits for a bundle first; you can cancel it until then.`;
      quote = null;
      amount = "";
      await load();
    } catch (e) {
      error = nice(e);
      quote = null;
    }
    busy = false;
  }

  async function cancel(id: string) {
    cancelling = id;
    error = "";
    try {
      await withUnlock(() => withdrawCancel(id), { what: "cancel the withdrawal" });
      await load();
    } catch (e) {
      error = nice(e);
    }
    cancelling = "";
  }

  function back() {
    quote = null;
    cq = null;
    warned = false;
  }
</script>

<div class="card withdraw" data-testid="withdraw-panel">
  <h3>Withdraw to eCash</h3>
  {#if !quote}
    <p class="muted small">
      Back to eCash at par, through the peg: trustless, but it takes months{balance != null ? `. You have ${ecx(Math.round(balance * 1e8))}` : ""}.
    </p>
    {#if done}<p class="hint ok-note">{done}</p>{/if}
    <form class="field" on:submit|preventDefault={prepare}>
      <label class="field-label" for="wd-amount">Amount (sECX)</label>
      <input id="wd-amount" type="text" inputmode="decimal" bind:value={amount} placeholder="0.00000000" autocomplete="off" />
      <fieldset class="wd-to">
        <legend class="field-label">To</legend>
        <label class="radio"><input type="radio" bind:group={toOwn} value={true} disabled={!ownReady} /> My eCash wallet{ownReady ? "" : " (set it up in the eCash tab)"}</label>
        <label class="radio"><input type="radio" bind:group={toOwn} value={false} /> Another eCash address</label>
      </fieldset>
      {#if !toOwn}
        <input type="text" bind:value={pasted} placeholder="eCash address" spellcheck="false" autocomplete="off" />
        <p class="muted small">
          Check it carefully: eCash addresses look like Bitcoin addresses, so a Bitcoin address (an exchange's, say) is
          accepted too, and coins sent to it are lost.
        </p>
      {/if}
      {#if error}<p class="soft-error">{error}</p>{/if}
      <div class="row-actions">
        <button type="submit" disabled={busy} data-testid="withdraw-next">{busy ? "Working it out…" : "Next"}</button>
      </div>
    </form>
  {:else if !warned}
    {#if cq}
      <div class="wd-option" data-testid="changer-out">
        <h4>Sell to the changer (fast)</h4>
        <p class="small">
          You get <strong>{fmtEcx(cq.payout)} ECX</strong>: {pct(cq.discount_bps)} below par, less its fee of {fmtEcx(cq.fee)}.
          In a few blocks, once your payment is in a FreeBank block. You pay {ecx(cq.amount)}, plus a small FreeBank fee.
        </p>
        <p class="muted small mono wd-dest">To {toOwn ? "your eCash wallet: " : ""}{cq.payout_to}</p>
        <p class="muted small">
          You trust the changer with this order{changer ? ` (it takes at most ${ecx(changer.sides.out.most_per_order)})` : ""}.
          Pay within {cq.blocks_left} FreeBank blocks, or it refunds you.
        </p>
        {#if error}<p class="soft-error">{error}</p>{/if}
        <div class="row-actions">
          <button on:click={sellFast} disabled={busy} data-testid="changer-sell">{busy ? "Paying…" : "Cash out fast"}</button>
        </div>
      </div>
      <h4 class="wd-or">Or withdraw at par (slow)</h4>
    {:else if cqError}
      <p class="muted small">The changer: {cqError}</p>
    {/if}
    <dl class="facts">
      <div><dt>The eCash address gets</dt><dd>{fmtEcx(quote.sats)} ECX</dd></div>
      <div><dt>To</dt><dd class="mono">{quote.pasted ? quote.address : `your eCash wallet (a new address: ${quote.address})`}</dd></div>
      <div><dt>eCash fee</dt><dd>{ecx(quote.mainchain_fee)}</dd></div>
      <div><dt>FreeBank fee</dt><dd>{ecx(quote.fee)}</dd></div>
      <div><dt>Leaves your FreeBank wallet</dt><dd>{ecx(quote.total)}</dd></div>
    </dl>
    {#if quote.pasted}<p class="muted small wd-gap">A pasted address: check it once more.</p>{/if}
    <div class="row-actions">
      <button class="secondary" on:click={back}>Back</button>
      <button on:click={() => (warned = true)} data-testid="withdraw-continue">Withdraw</button>
    </div>
  {:else}
    <div class="wd-warning" role="alert">
      <p><strong>A withdrawal through the peg is very slow.</strong> {ecx(quote.sats)} to {quote.address}.</p>
      <p>
        FreeBank pays withdrawals out in batches (bundles), and eCash miners must approve each batch: 3 to 6 months or
        more on mainnet. On the beta, it will probably never complete.
      </p>
      <p>
        You can cancel it only until it joins a bundle, which can happen in any block. On the beta, a bundle that can't
        be paid holds up every other FreeBank withdrawal for about 6 months.
      </p>
    </div>
    {#if error}<p class="soft-error">{error}</p>{/if}
    <div class="row-actions">
      <button on:click={back} disabled={busy} data-testid="withdraw-cancel-warning">Cancel</button>
      <button class="secondary" on:click={confirm} disabled={busy} data-testid="withdraw-anyway">{busy ? "Withdrawing…" : "Withdraw anyway, I'll wait"}</button>
    </div>
  {/if}

  {#if orders.length}
    <h4 class="wd-sub">Changer orders</h4>
    <ul class="wd-list" data-testid="changer-orders">
      {#each orders as o (o.id)}
        <li>
          <span>{ecx(o.amount)} for {fmtEcx(o.payout)} ECX</span>
          <span class="muted small" class:soft-error={o.state === "overdue" || o.state === "held"}>{changerLine(o)}</span>
        </li>
      {/each}
    </ul>
  {/if}

  {#if list.length}
    <h4 class="wd-sub">Withdrawals</h4>
    <ul class="wd-list" data-testid="withdraw-list">
      {#each list as w (w.id)}
        <li>
          <div class="wd-row">
            <span>{ecx(w.sats)}</span>
            {#if w.state === "waiting"}
              <button class="link-btn" on:click={() => cancel(w.id)} disabled={!!cancelling}>{cancelling === w.id ? "Cancelling…" : "Cancel"}</button>
            {/if}
          </div>
          <span class="muted small">{withdrawalLine(w)}</span>
          {#if w.destination}<span class="muted small mono wd-dest">{w.destination}</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .withdraw .facts {
    margin: 10px 0;
  }
  .wd-to {
    border: 0;
    margin: 10px 0 4px;
    padding: 0;
  }
  .withdraw .field-label {
    display: block;
    margin-top: 10px;
  }
  .wd-gap {
    margin-bottom: 10px;
  }
  .wd-to input[type="radio"] {
    accent-color: var(--accent-color);
  }
  .wd-to .radio {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13.5px;
    margin: 4px 0;
  }
  .wd-option {
    border: 1px solid var(--accent-color);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 6px 0 10px;
  }
  .wd-option h4,
  .wd-or {
    margin: 0 0 6px;
    font-size: 13.5px;
  }
  .wd-or {
    margin-top: 12px;
  }
  .wd-warning {
    border: 1px solid var(--warning-color, #c98a00);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 6px 0 12px;
    font-size: 13.5px;
  }
  .wd-warning p {
    margin: 0 0 6px;
  }
  .wd-sub {
    margin: 14px 0 6px;
    font-size: 13.5px;
  }
  .wd-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .wd-list li {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    border-top: 1px solid var(--border-color);
  }
  .wd-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .wd-dest {
    overflow-wrap: anywhere;
  }
</style>
