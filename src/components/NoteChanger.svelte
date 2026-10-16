<script lang="ts">
  // v0.4.8: a house's notes through the money changer (CASH_OUT_DESIGN.md §4.13), under Credit › Notes. Sell notes
  // for ECX (paid in with a note transfer from the FreeBank wallet), or buy them with ECX from the app's eCash wallet.
  // Shown only when a changer is set up (Settings) and deals in some house's notes. One note unit is one sat of sECX
  // at par, so note amounts are entered and shown as sECX, as everywhere on this screen.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { fmtEcx, parseEcx } from "../lib/amount";
  import { BASE_TICKER, L1_TICKER } from "../lib/brand";
  import { nice } from "../lib/errors";
  import {
    changerInfo,
    changerLine,
    changerOrders,
    changerPay,
    changerQuote,
    pct,
    type ChangerInfo,
    type ChangerNoteDesk,
    type ChangerOrder,
    type ChangerQuote,
  } from "../lib/inout";
  import type { NoteHolding } from "../lib/api";
  import { withUnlock } from "../lib/wallet";

  /** The wallet's note holdings (Credit › Notes reads them). */
  export let notes: NoteHolding[] = [];

  const dispatch = createEventDispatcher<{ changed: void }>();
  const ecx = (sats: number) => `${fmtEcx(sats)} ${L1_TICKER}`;
  const secx = (units: number) => `${fmtEcx(units)} ${BASE_TICKER}`;

  let info: ChangerInfo | null = null;
  let desks: ChangerNoteDesk[] = [];
  let orders: ChangerOrder[] = [];
  let mode: "sell" | "buy" | null = null;
  let house: number | null = null;
  let amount = "";
  let address = "";
  let pass = "";
  let cq: ChangerQuote | null = null;
  let busy = false;
  let error = "";
  let done = "";

  let timer: ReturnType<typeof setInterval> | undefined;
  // Each order's state at the last look: when one moves, the notes it paid in or out have moved too.
  let seen = "";

  async function load() {
    try {
      info = await changerInfo();
    } catch {
      info = null;
    }
    desks = info?.notes ?? [];
    if (desks.length) {
      try {
        orders = (await changerOrders()).filter((o) => o.classid);
      } catch {
        orders = [];
      }
    }
    const now = orders.map((o) => `${o.id}:${o.state}`).join(",");
    if (seen && now !== seen) {
      done = "";
      dispatch("changed");
    }
    seen = now;
  }
  onMount(() => {
    load();
    timer = setInterval(load, 20_000);
  });
  onDestroy(() => clearInterval(timer));

  const held = (h: number) => notes.find((n) => n.house_id === h);
  // Sell: houses the changer buys, of which the wallet holds undemanded notes. Buy: houses it sells.
  $: sellable = desks.filter((d) => !d.paused && d.sides.out.most_per_order > 0 && (held(d.house)?.units ?? 0) > (held(d.house)?.demanded_units ?? 0));
  $: buyable = desks.filter((d) => !d.paused && d.sides.in.most_per_order > 0);
  $: desk = desks.find((d) => d.house === house) ?? null;
  // Buying: the ECX to pay for the most notes it sells an order, and the least worth paying its fee for (the changer
  // takes no order whose fee is over a tenth of it). One unit of notes costs (1 - discount) of a sat.
  $: buyMost = desk && desk.note_fee !== null ? Math.floor(((desk.sides.in.most_per_order + desk.note_fee) * (10_000 - desk.buy_bps)) / 10_000) : null;
  $: buyLeast = desk && desk.note_fee !== null ? desk.note_fee * 10 : null;

  function start(m: "sell" | "buy") {
    mode = m;
    const list = m === "sell" ? sellable : buyable;
    house = list.length ? list[0].house : null;
    amount = "";
    address = "";
    cq = null;
    error = "";
    done = "";
  }

  async function ask() {
    if (!mode || house === null) return;
    const units = parseEcx(amount);
    if (units === null || units <= 0) {
      error = "Enter an amount, with at most 8 decimals.";
      return;
    }
    busy = true;
    error = "";
    cq = null;
    try {
      cq = await changerQuote(mode === "sell" ? "out" : "in", fmtEcx(units), mode === "sell" && address.trim() ? address.trim() : null, house);
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }

  async function pay() {
    if (!cq) return;
    busy = true;
    error = "";
    const q = cq;
    try {
      let msg: string;
      if (q.side === "out") {
        await withUnlock(() => changerPay(q.id, null), { what: "pay the changer" });
        msg = `${secx(q.amount)} of ${q.classid} notes sent to the changer. It pays ${ecx(q.payout)} once they're in a FreeBank block.`;
      } else {
        await changerPay(q.id, pass);
        msg = `${ecx(q.amount)} paid to the changer. It pays ${secx(q.payout)} of ${q.classid} notes once your payment is in an eCash block.`;
      }
      pass = "";
      cq = null;
      mode = null;
      dispatch("changed");
      // After the look that finds the new order, so it stays until that order moves on.
      await load();
      done = msg;
    } catch (e) {
      error = nice(e);
      if (q.side === "out" && /single holder|enough transferable/i.test(error))
        error += " Your notes of this house may sit at more than one of your addresses: gather them first (Send, with the address left empty), then try again.";
      if (!/passphrase/i.test(error)) cq = null;
    }
    busy = false;
  }
</script>

{#if desks.length}
  <div class="card" data-testid="note-changer">
    <h2>The money changer</h2>
    <p class="muted small">
      It buys and sells some houses' notes for {L1_TICKER}, a little below par. You trust it with one order at a time.
    </p>
    {#if done}<p class="small nc-done">{done}</p>{/if}
    {#if mode === null}
      <div class="row-actions">
        <button class="secondary" on:click={() => start("sell")} disabled={!sellable.length} data-testid="notes-sell">Sell notes for {L1_TICKER}</button>
        <button class="secondary" on:click={() => start("buy")} disabled={!buyable.length} data-testid="notes-buy">Buy notes with {L1_TICKER}</button>
      </div>
      {#if !sellable.length && !buyable.length}
        <p class="muted small">{desks.find((d) => d.paused)?.paused ?? "It isn't taking orders just now."}</p>
      {:else if !buyable.length}
        <p class="muted small">It has no notes to sell just now.</p>
      {:else if !sellable.length}
        <p class="muted small">You hold none of the notes it buys.</p>
      {/if}
    {:else}
      <form class="field" on:submit|preventDefault={ask}>
        <label class="field-label" for="nc-house">House</label>
        <select id="nc-house" bind:value={house} on:change={() => (cq = null)}>
          {#each mode === "sell" ? sellable : buyable as d (d.house)}
            <option value={d.house}>{d.classid} (House #{d.house})</option>
          {/each}
        </select>
        <label class="field-label" for="nc-amount">{mode === "sell" ? `Notes to sell (${BASE_TICKER})` : `${L1_TICKER} to pay`}</label>
        <input id="nc-amount" type="number" bind:value={amount} on:input={() => (cq = null)} placeholder="0.00000000" step="0.00000001" min="0" />
        {#if desk}
          <p class="hint">
            {#if mode === "sell"}
              {pct(desk.sell_bps)} below par; at most {secx(desk.sides.out.most_per_order)} an order. You hold {secx((held(desk.house)?.units ?? 0) - (held(desk.house)?.demanded_units ?? 0))} of its notes.
            {:else if buyMost !== null && buyLeast !== null}
              {#if buyMost < buyLeast}
                {pct(desk.buy_bps)} below par. It has too few of these notes to sell an order just now.
              {:else}
                {pct(desk.buy_bps)} below par; pay from {ecx(buyLeast)} to {ecx(buyMost)} an order.
              {/if}
            {:else}
              {pct(desk.buy_bps)} below par; at most {secx(desk.sides.in.most_per_order)} of notes an order.
            {/if}
          </p>
        {/if}
        {#if mode === "sell"}
          <label class="field-label" for="nc-to">{L1_TICKER} to (leave empty for your eCash wallet)</label>
          <input id="nc-to" type="text" bind:value={address} on:input={() => (cq = null)} spellcheck="false" autocomplete="off" />
        {/if}
        {#if !cq}
          <div class="row-actions">
            <button type="button" class="secondary" on:click={() => (mode = null)}>Cancel</button>
            <button type="submit" disabled={busy || !amount} data-testid="notes-quote">{busy ? "Asking…" : "Get the changer's price"}</button>
          </div>
        {/if}
      </form>
      {#if cq}
        <div class="nc-quote" data-testid="notes-quote-shown">
          {#if cq.side === "out"}
            <p class="small">
              You get <strong>{ecx(cq.payout)}</strong> for {secx(cq.amount)} of {cq.classid} notes: {pct(cq.discount_bps)} below par, less
              the {L1_TICKER} fee of {fmtEcx(cq.fee)}. You pay a small FreeBank fee too.
            </p>
            <p class="muted small mono">To {cq.payout_to}</p>
            <p class="muted small">Send within {cq.blocks_left} FreeBank blocks, or it sends your notes back, less the transfer fee.</p>
          {:else}
            <p class="small">
              You get <strong>{secx(cq.payout)}</strong> of {cq.classid} notes for {ecx(cq.amount)}: {pct(cq.discount_bps)} below par, less
              FreeBank's transfer fee of {secx(cq.fee)}. Once your payment is in an eCash block.
            </p>
            <p class="muted small">Pay within {cq.blocks_left} eCash blocks, or it refunds you.</p>
            <label class="field-label" for="nc-pass">Wallet passphrase</label>
            <input id="nc-pass" type="password" bind:value={pass} autocomplete="current-password" />
          {/if}
          <div class="row-actions">
            <button type="button" class="secondary" on:click={() => (cq = null)}>Cancel</button>
            <button on:click={pay} disabled={busy || (cq.side === "in" && !pass)} data-testid="notes-pay">
              {busy ? "Paying…" : cq.side === "out" ? "Sell to the changer" : "Buy from the changer"}
            </button>
          </div>
        </div>
      {/if}
    {/if}
    {#if error}<p class="soft-error">{error}</p>{/if}
    {#if orders.length}
      <h4 class="nc-list-title">Note orders</h4>
      <ul class="nc-list" data-testid="note-orders">
        {#each orders as o (o.id)}
          <li>
            <span>
              {o.side === "out" ? `${secx(o.amount)} of ${o.classid} notes for ${ecx(o.payout)}` : `${ecx(o.amount)} for ${secx(o.payout)} of ${o.classid} notes`}
            </span>
            <span class="muted small" class:soft-error={o.state === "overdue" || o.state === "held"}>{changerLine(o)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .nc-quote {
    margin-top: 0.5rem;
  }
  .nc-done {
    margin: 0.5rem 0 0.75rem;
  }
  .nc-list-title {
    margin-top: 1rem;
  }
  .nc-list {
    list-style: none;
    padding: 0;
    margin: 0.25rem 0 0;
  }
  .nc-list li {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    padding: 0.35rem 0;
    border-top: 1px solid var(--border, rgba(127, 127, 127, 0.2));
  }
  select {
    width: 100%;
  }
</style>
