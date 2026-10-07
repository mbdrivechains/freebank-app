<script lang="ts">
  // Home, Deposit (v0.3.0 "In and out"): at par from the app's own eCash wallet (built and signed here,
  // src-tauri/src/ecash/deposit.rs), with the deposits in flight; and, below, from any other eCash wallet.
  //
  // From another wallet (v0.2.0): how to move ECX from the eCash chain into FreeBank with BitWindow.
  // The address comes from freebankd's getdepositaddress in the wrapped form s130_<address>_<checksum>
  // (lib/deposit.ts checks the slot and checksum), with Copy and a QR code, and the plain address for other
  // tools. It is made when the panel first opens and kept for the session.
  //
  // `canShowAddresses` is false until the wallet has its passphrase: setting one replaces the wallet's seed,
  // so an address shown before would belong to the old seed. App.svelte wires it to the wallet's state.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import Notice from "./Notice.svelte";
  import QrCode from "./QrCode.svelte";
  import { fmtEcx, parseEcx } from "../lib/amount";
  import { depositAddress, depositOpen, getDepositAddress } from "../lib/deposit";
  import { ecashStatus, type EcashStatus } from "../lib/ecash";
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
    FREEBANK_DEPOSIT_FEE,
    depositConfirm,
    depositLine,
    depositList,
    depositPrepare,
    type Deposit,
    type DepositQuote,
  } from "../lib/inout";
  import { withUnlock } from "../lib/wallet";

  export let canShowAddresses = true;

  const dispatch = createEventDispatcher<{ ecash: void }>();
  const coins = (sats: number) => `${fmtEcx(sats)} eCash`;

  // At par from the app's eCash wallet.
  let ec: EcashStatus | null = null;
  let amount = "";
  let quote: DepositQuote | null = null;
  let pass = "";
  let parBusy = false;
  let parError = "";
  let sentNote = "";
  let deposits: Deposit[] = [];
  let timer: ReturnType<typeof setInterval> | undefined;
  // The changer (Settings): buy FreeBank ECX below par with eCash, beside the deposit at par.
  let changer: ChangerInfo | null = null;
  let cq: ChangerQuote | null = null;
  let cqError = "";
  let orders: ChangerOrder[] = [];

  async function loadPar() {
    try {
      ec = await ecashStatus();
    } catch {
      ec = null;
    }
    await loadDeposits();
    if (changer) {
      try {
        orders = (await changerOrders()).filter((o) => o.side === "in");
      } catch {
        // as above
      }
    }
  }

  async function loadDeposits() {
    try {
      deposits = await depositList();
      // Notes about the last deposit go once it's settled; "already waiting" once nothing waits.
      const waiting = deposits.some((d) => d.state === "signed" || d.state === "sent");
      if (sentNote && deposits.length && ["credited", "failed"].includes(deposits[0].state)) sentNote = "";
      if (!waiting && /already waiting/.test(parError)) parError = "";
    } catch {
      // The list waits for the next look.
    }
  }

  onMount(async () => {
    try {
      changer = await changerInfo();
    } catch {
      changer = null;
    }
    loadPar();
    // The deposits and what the eCash wallet can use, every 15 seconds.
    timer = setInterval(loadPar, 15_000);
  });
  onDestroy(() => clearInterval(timer));

  async function prepareDeposit(max = false) {
    parError = "";
    sentNote = "";
    if (!max && parseEcx(amount) === null) {
      parError = "Enter an amount in eCash above zero, with at most 8 decimal places.";
      return;
    }
    parBusy = true;
    cq = null;
    cqError = "";
    try {
      // The FreeBank deposit address may need the wallet open (an empty key pool on a locked wallet).
      quote = await withUnlock(() => depositPrepare(max ? null : amount.trim(), max), { what: "make a deposit address" });
      if (max && quote) amount = fmtEcx(quote.sats);
      if (changer && !changer.paused && quote) {
        try {
          cq = await changerQuote("in", fmtEcx(quote.sats), null);
        } catch (e) {
          cqError = nice(e);
        }
      }
    } catch (e) {
      parError = nice(e);
    }
    parBusy = false;
  }

  async function buyFromChanger() {
    if (!cq) return;
    parBusy = true;
    parError = "";
    try {
      await changerPay(cq.id, pass);
      sentNote = `${coins(cq.amount)} paid to the changer. It pays ${fmtEcx(cq.payout)} ECX once your payment is in an eCash block.`;
      quote = null;
      cq = null;
      pass = "";
      amount = "";
      await loadPar();
    } catch (e) {
      parError = nice(e);
      if (!/passphrase/i.test(parError)) cq = null;
    }
    parBusy = false;
  }

  async function confirmDeposit() {
    if (!quote) return;
    parBusy = true;
    parError = "";
    try {
      await depositConfirm(quote.id, pass);
      sentNote = `${coins(quote.sats)} sent. FreeBank credits it after the next eCash block and a FreeBank block.`;
      quote = null;
      pass = "";
      amount = "";
      await loadPar();
    } catch (e) {
      parError = nice(e);
      // A prepared deposit is used once, unless only the passphrase was wrong.
      if (!/passphrase/i.test(parError)) quote = null;
      await loadPar();
    }
    parBusy = false;
  }

  function cancelDeposit() {
    quote = null;
    cq = null;
    pass = "";
    parError = "";
  }

  $: inFlight = deposits.filter((d) => d.state !== "credited" || Date.now() / 1000 - d.time < 86_400);

  let busy = false;
  let error = "";
  let copied: "" | "wrapped" | "plain" = "";
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  async function fetchAddress() {
    busy = true;
    error = "";
    try {
      // A locked wallet with an empty key pool answers -12; withUnlock asks for the passphrase then.
      depositAddress.set(await withUnlock(() => getDepositAddress(), { what: "make a deposit address" }));
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }

  function show() {
    depositOpen.set(true);
    if (canShowAddresses && !$depositAddress && !busy) fetchAddress();
  }

  async function copy(text: string, which: "wrapped" | "plain") {
    try {
      await navigator.clipboard.writeText(text);
      copied = which;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = ""), 1500);
    } catch {
      error = "Couldn't copy it; select the address and copy it by hand.";
    }
  }
</script>

<div class="card deposit-par" data-testid="deposit-par">
  <h3>Deposit from eCash</h3>
  {#if !canShowAddresses}
    <p class="hint">Deposits show here once your wallet has a passphrase.</p>
  {:else if ec?.state === "ready" && ec.main}
    <p class="muted small">
      From your eCash wallet into FreeBank, at par: {coins(ec.main.trusted)} you can use now{ec.main.pending
        ? `, and ${coins(ec.main.pending)} on its way (change from a deposit counts here until it confirms)`
        : ""}. FreeBank builds and signs the deposit on this computer; it's credited after the next eCash block and a
      FreeBank block.
    </p>
    {#if sentNote}<p class="hint ok-note">{sentNote}</p>{/if}
    {#if !quote}
      <form class="field" on:submit|preventDefault={() => prepareDeposit()}>
        <label class="field-label" for="dep-amount">Amount (eCash)</label>
        <input id="dep-amount" type="text" inputmode="decimal" bind:value={amount} placeholder="0.00000000" autocomplete="off" />
        {#if parError}<p class="soft-error">{parError}</p>{/if}
        <div class="row-actions">
          <button type="button" class="secondary" on:click={() => prepareDeposit(true)} disabled={parBusy} data-testid="deposit-max">Max</button>
          <button type="submit" disabled={parBusy} data-testid="deposit-next">{parBusy ? "Working out the fee…" : "Next"}</button>
        </div>
      </form>
    {:else}
      <h4 class="dep-or">Deposit at par</h4>
      <dl class="facts">
        <div><dt>Into FreeBank</dt><dd>{coins(quote.sats)}</dd></div>
        <div><dt>FreeBank credits</dt><dd>{fmtEcx(quote.credited)} ECX <span class="muted small">(it keeps {fmtEcx(FREEBANK_DEPOSIT_FEE)} for the block that credits it)</span></dd></div>
        <div><dt>eCash fee</dt><dd>{coins(quote.fee)}</dd></div>
        <div><dt>Leaves your eCash wallet</dt><dd>{coins(quote.total)}</dd></div>
        <div><dt>Credited to</dt><dd class="mono">{quote.fb_wallet ? `${quote.fb_wallet}: ` : ""}{quote.address}</dd></div>
      </dl>
      {#if cq}
        <div class="dep-option" data-testid="changer-in">
          <h4>Or buy from the changer: more ECX</h4>
          <p class="small">
            You get <strong>{fmtEcx(cq.payout)} ECX</strong> for {coins(cq.amount)}: {pct(cq.discount_bps)} below par, less
            its fee of {fmtEcx(cq.fee)}. Once your payment is in an eCash block.
          </p>
          <p class="muted small">You trust the changer with this order. Pay within {cq.blocks_left} eCash blocks, or it refunds you.</p>
        </div>
        
      {:else if cqError}
        <p class="muted small">The changer: {cqError}</p>
      {/if}
      <form class="field" on:submit|preventDefault={confirmDeposit}>
        <label class="field-label" for="dep-pass">Wallet passphrase</label>
        <input id="dep-pass" type="password" bind:value={pass} autocomplete="current-password" />
        {#if parError}<p class="soft-error">{parError}</p>{/if}
        <div class="row-actions">
          <button type="button" class="secondary" on:click={cancelDeposit}>Cancel</button>
          {#if cq}
            <button type="button" class="secondary" on:click={buyFromChanger} disabled={parBusy || !pass} data-testid="changer-buy">{parBusy ? "Paying…" : "Buy from the changer"}</button>
          {/if}
          <button type="submit" disabled={parBusy || !pass} data-testid="deposit-confirm">{parBusy ? "Depositing…" : "Deposit at par"}</button>
        </div>
      </form>
    {/if}
  {:else if ec}
    <p class="muted small">
      Set up your eCash wallet to deposit from it here, at par, in <button class="link-btn inline" on:click={() => dispatch("ecash")}>the eCash tab</button>.
    </p>
  {/if}
  {#if orders.length}
    <h4 class="dep-sub">Changer orders</h4>
    <ul class="dep-list" data-testid="changer-orders">
      {#each orders as o (o.id)}
        <li>
          <span class="dep-amt">{coins(o.amount)} for {fmtEcx(o.payout)} ECX</span>
          <span class="muted small" class:soft-error={o.state === "overdue" || o.state === "held"}>{changerLine(o)}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if inFlight.length}
    <h4 class="dep-sub">Deposits</h4>
    <ul class="dep-list" data-testid="deposit-list">
      {#each inFlight as d (d.txid)}
        <li>
          <span class="dep-amt">{coins(d.sats)}</span>
          <span class="muted small" class:soft-error={d.state === "failed"}>{depositLine(d)}</span>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<div class="card deposit" data-open={$depositOpen}>
  <div class="dep-head">
    <h3>From another eCash wallet</h3>
    {#if $depositOpen}
      <button class="link-btn" on:click={() => depositOpen.set(false)}>Hide</button>
    {/if}
  </div>
  <p class="muted small">Move ECX from the eCash chain into FreeBank with BitWindow.</p>

  {#if !$depositOpen}
    <button class="secondary dep-show" on:click={show}>Show how to deposit</button>
  {:else if !canShowAddresses}
    <p class="hint dep-wait">Your deposit address shows here once your wallet has a passphrase.</p>
  {:else}
    {#if error}<Notice kind="error" message={error} on:dismiss={() => (error = "")} />{/if}
    {#if !$depositAddress}
      <button class="dep-show" on:click={fetchAddress} disabled={busy}>{busy ? "Getting your address…" : "Get my deposit address"}</button>
    {:else}
      <ol class="dep-steps">
        <li>In BitWindow, open <strong>Sidechains</strong>, then <strong>FreeBank</strong>, then <strong>Deposit</strong>.</li>
        <li>Paste your FreeBank deposit address:</li>
      </ol>
      <div class="address-display">
        <code data-testid="deposit-wrapped">{$depositAddress.wrapped}</code>
        <button on:click={() => $depositAddress && copy($depositAddress.wrapped, "wrapped")}>{copied === "wrapped" ? "Copied" : "Copy"}</button>
      </div>
      <div class="dep-qr"><QrCode text={$depositAddress.wrapped} size={184} /></div>
      <ol class="dep-steps" start="3">
        <li>Enter the amount and send.</li>
      </ol>
      <p class="hint dep-plain-note">
        Use the wrapped form in BitWindow only; other tools take the plain address:
      </p>
      <div class="address-display">
        <code data-testid="deposit-plain">{$depositAddress.plain}</code>
        <button on:click={() => $depositAddress && copy($depositAddress.plain, "plain")}>{copied === "plain" ? "Copied" : "Copy"}</button>
      </div>
      <p class="hint">
        The deposit shows under your balance once its eCash payment is in a block and a FreeBank block takes it in.
      </p>
      <Notice kind="info" dismissible={false}>
        If your eCash node started from a snapshot, deposits show only after it finishes checking the whole chain,
        which can take days.
      </Notice>
      <button class="link-btn dep-new" on:click={fetchAddress} disabled={busy}>{busy ? "…" : "New deposit address"}</button>
    {/if}
  {/if}
</div>

<style>
  .dep-option {
    border: 1px solid var(--accent-color);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 10px 0;
  }
  .dep-option h4,
  .dep-or {
    margin: 0 0 6px;
    font-size: 13.5px;
  }
  .deposit-par .facts {
    margin: 10px 0;
  }
  .dep-sub {
    margin: 14px 0 6px;
    font-size: 13.5px;
  }
  .dep-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .dep-list li {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    border-top: 1px solid var(--border-color);
  }
  .dep-amt {
    font-variant-numeric: tabular-nums;
  }
  .link-btn.inline {
    display: inline;
    padding: 0;
    font-size: inherit;
  }
  .deposit-par .field-label {
    display: block;
    margin-top: 10px;
  }
  .dep-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  .dep-head h3 {
    margin-bottom: 0;
  }
  .deposit > .muted {
    margin-top: 6px;
  }
  .dep-show {
    width: 100%;
    margin-top: 12px;
  }
  .dep-wait {
    margin-top: 10px;
  }
  .dep-steps {
    margin: 12px 0 10px 20px;
    font-size: 13.5px;
    line-height: 1.5;
  }
  .dep-steps li + li {
    margin-top: 4px;
  }
  .dep-qr {
    display: flex;
    justify-content: center;
    margin: 4px 0 6px;
  }
  .dep-plain-note {
    margin: 6px 0 8px;
  }
  .deposit .address-display {
    margin-bottom: 12px;
  }
  .deposit .address-display code {
    user-select: all;
    -webkit-user-select: all;
  }
  .deposit :global(.notice) {
    margin: 12px 0 4px;
  }
  .dep-new {
    margin-top: 6px;
    padding-left: 0;
  }
</style>
