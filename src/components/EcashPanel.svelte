<script lang="ts">
  // The eCash tab (v0.2.6). Two wallets of the
  // app's own in the eCash node (BitWindow's, or one run by hand), both from the recovery words:
  // - the main eCash wallet, locked with the wallet passphrase: Receive, Send, and Move into the bidding wallet, each
  //   payment confirmed with the passphrase (and "Approve sends on my phone" when it's on: PhoneAlerts shows the wait);
  // - the bidding wallet, with no passphrase, so bids can go out unattended: only what is moved in, and Move back.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import QrCode from "./QrCode.svelte";
  import { L1_TICKER } from "../lib/brand";
  import { ecxInput, fmtEcx, parseEcx } from "../lib/amount";
  import {
    bmmSet,
    bmmStatus,
    type BmmStatus,
    ecashBidsWithdrawPrepare,
    ecashBidsWithdrawConfirm,
    ecashConfirm,
    ecashHistory,
    ecashPrepare,
    ecashReceive,
    ecashSetup,
    ecashStatus,
    type EcashQuote,
    type EcashStatus,
    type EcashTx,
  } from "../lib/ecash";

  const dispatch = createEventDispatcher<{ settings: string }>();

  // Wallet | Bidding (v0.2.6, the UX walk-through).
  let seg: "wallet" | "bidding" = "wallet";
  let loginMore = false;

  let st: EcashStatus | null = null;
  let loading = false;
  let error = "";
  let history: EcashTx[] = [];

  let setupPass = "";
  let settingUp = false;

  let address = "";
  let copied = false;

  // One payment form at a time: to an address, or into the bidding wallet.
  let mode: "" | "send" | "bids" = "";
  let to = "";
  let amount = "";
  let everything = false;
  let quote: EcashQuote | null = null;
  let pass = "";
  let busy = false;
  let payError = "";
  let sent = "";

  let backBusy = false;
  let backDone = "";
  /** Move back, prepared: what arrives and the fee, before it goes. */
  let backQuote: EcashQuote | null = null;

  // Bidding for FreeBank blocks.
  let bmm: BmmStatus | null = null;
  let bidText = "";
  let capText = "";
  let bmmEdited = false;
  let bmmBusy = false;
  let bmmError = "";
  const OUTCOME: Record<string, string> = {
    live: "waiting for the next eCash block",
    won: "won",
    lost: "went to another bidder",
    replaced: "went to another bidder; its coin went into the next bid",
    rejected: "won, but the FreeBank node refused the block",
    failed: "couldn't be settled",
  };

  const say = (e: unknown) => String(e).replace(/^Error: /, "");
  const coins = (sats: number) => `${fmtEcx(sats)} ${L1_TICKER}`;

  async function load() {
    loading = true;
    error = "";
    try {
      st = await ecashStatus();
      history = st.state === "ready" ? await ecashHistory() : [];
      if (st.state === "ready") {
        bmm = await bmmStatus();
        if (!bmmEdited) {
          bidText = ecxInput(bmm.bid);
          capText = ecxInput(bmm.daily_cap);
        }
      }
    } catch (e) {
      error = say(e);
    }
    loading = false;
  }

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    load();
    timer = setInterval(() => !busy && !settingUp && load(), 30_000);
  });
  onDestroy(() => clearInterval(timer));

  async function setup() {
    settingUp = true;
    error = "";
    try {
      st = await ecashSetup(setupPass);
      setupPass = "";
      await load();
    } catch (e) {
      error = say(e);
    }
    settingUp = false;
  }

  async function receive() {
    error = "";
    close();
    sent = "";
    try {
      address = await ecashReceive();
    } catch (e) {
      error = say(e);
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      error = "Couldn't copy it; select the address and copy it by hand.";
    }
  }

  function open(m: "send" | "bids") {
    mode = m;
    address = "";
    to = "";
    amount = "";
    everything = false;
    quote = null;
    pass = "";
    payError = "";
    sent = "";
  }

  function close() {
    mode = "";
    quote = null;
    pass = "";
  }

  async function prepare() {
    payError = "";
    if (!everything && parseEcx(amount) === null) {
      payError = "Enter an amount in ECX above zero, with at most 8 decimal places.";
      return;
    }
    if (mode === "send" && !to.trim()) {
      payError = "Enter the eCash address to send to.";
      return;
    }
    busy = true;
    try {
      quote = await ecashPrepare(to, everything ? null : amount.trim(), mode === "bids");
    } catch (e) {
      payError = say(e);
    }
    busy = false;
  }

  async function confirm() {
    if (!quote) return;
    busy = true;
    payError = "";
    try {
      sent = await ecashConfirm(quote.id, pass);
      pass = "";
      quote = null;
      mode = "";
      await load();
    } catch (e) {
      payError = say(e);
      // A prepared payment is used once: a failed confirm needs a new one.
      if (!/passphrase/i.test(payError)) quote = null;
    }
    busy = false;
  }

  async function moveBack() {
    backBusy = true;
    backDone = "";
    error = "";
    try {
      backQuote = await ecashBidsWithdrawPrepare(null);
    } catch (e) {
      error = say(e);
    }
    backBusy = false;
  }

  async function moveBackConfirm() {
    if (!backQuote) return;
    backBusy = true;
    error = "";
    try {
      backDone = await ecashBidsWithdrawConfirm(backQuote.id);
      // With nothing left to bid from, bidding goes off rather than waiting on a coin.
      if (bmm?.on) bmm = await bmmSet(false, ecxInput(bmm.bid), ecxInput(bmm.daily_cap));
      await load();
    } catch (e) {
      error = say(e);
    }
    backQuote = null;
    backBusy = false;
  }

  async function saveBmm(on: boolean) {
    bmmBusy = true;
    bmmError = "";
    try {
      bmm = await bmmSet(on, bidText, capText);
      bmmEdited = false;
      bidText = ecxInput(bmm.bid);
      capText = ecxInput(bmm.daily_cap);
    } catch (e) {
      bmmError = say(e);
    }
    bmmBusy = false;
  }

  // A move between the two wallets is one payment, not a send here and a receive there.
  type Line = EcashTx & { moved?: "in" | "back" };
  function merged(list: EcashTx[]): Line[] {
    const out: Line[] = [];
    const other = (w: string) => (w === "main" ? "bids" : "main");
    for (const t of list) {
      const pair = (x: EcashTx) => x.txid === t.txid && x.wallet === other(t.wallet);
      if (t.category === "receive" && list.some((x) => pair(x) && x.category === "send")) continue;
      const moved = t.category === "send" && list.some((x) => pair(x) && x.category === "receive");
      out.push(moved ? { ...t, moved: t.wallet === "main" ? "in" : "back" } : t);
    }
    return out;
  }

  function txLine(t: Line): string {
    const fee = t.fee ? `, fee ${fmtEcx(t.fee)}` : "";
    if (t.moved === "in") return `Moved ${coins(-t.sats)} into the bidding wallet${fee}`;
    if (t.moved === "back") return `Moved ${coins(-t.sats)} back to the main wallet${fee}`;
    // A bid: nothing sent anywhere, the whole bid its fee.
    if (t.wallet === "bids" && t.category === "send" && t.sats === 0) return `Bid for a FreeBank block, ${coins(t.fee)}`;
    if (t.category === "send") return `Sent ${coins(-t.sats)}${fee}`;
    if (t.category === "immature") return `Mined ${coins(t.sats)}, not yet spendable`;
    if (t.category === "generate") return `Mined ${coins(t.sats)}`;
    return `Received ${coins(t.sats)}`;
  }
</script>

<div class="ecash">
  <div class:card={st?.state !== "ready"} class="ec-intro">
    <div class="ec-head">
      <h2>eCash</h2>
      <button class="link-btn" on:click={load} disabled={loading}>{loading ? "Checking…" : "Refresh"}</button>
    </div>
    <p class="muted small">
      Your ECX, kept in your eCash node (BitWindow's, or your own) in wallets of FreeBank's own. Your recovery words
      bring them back too.
    </p>
    {#if error}<p class="soft-error" data-testid="ecash-error">{error}</p>{/if}
    {#if st?.problem}
      <p class="soft-error" data-testid="ecash-problem">
        {st.problem}
        {#if /login|answering|data folder/i.test(st.problem)}
          <button class="link-btn inline" on:click={() => dispatch("settings", "node")}>Open the eCash login</button>
        {/if}
      </p>
    {/if}
    {#if st?.default_login}
      <p class="muted small" data-testid="ecash-default-login">
        Your eCash node uses BitWindow's default login.
        {#if loginMore}
          Any program on this computer can see your eCash wallets' balances and payments with it, but not spend them:
          FreeBank signs every payment itself, and the node holds no keys.
        {:else}
          <button class="link-btn inline" on:click={() => (loginMore = true)}>More</button>
        {/if}
      </p>
    {/if}
  </div>

  {#if st && !st.problem && st.state !== "ready"}
    <div class="card">
      {#if st}
        <h3>{st.state === "missing" ? "Your eCash node doesn't have FreeBank's eCash wallets" : "Set up your eCash wallet"}</h3>
        {#if !st.has_words}
          <p class="muted small">
            FreeBank needs your recovery words saved first:
            <button class="link-btn inline" on:click={() => dispatch("settings", "wallet")}>Settings › Wallet</button>.
          </p>
        {:else}
          <p class="muted small">
            {st.state === "missing"
              ? "If you changed eCash nodes, set them up again here: they come back from your recovery words, with what they hold."
              : "FreeBank makes two wallets from your recovery words: your eCash wallet, whose payments need your wallet passphrase, and a small bidding wallet for bidding on FreeBank blocks, which pays bids with nobody there. Your eCash node only watches them; FreeBank signs every payment itself."}
          </p>
          {#if st.earlier}
            <p class="muted small">
              Wallets FreeBank made for other recovery words stay in your eCash node, watch-only: those words bring them
              back (BIP85 XPRV, index 0, then BIP84).
            </p>
          {/if}
          <form class="field" on:submit|preventDefault={setup}>
            <label class="field-label" for="ec-setup-pass">Wallet passphrase</label>
            <input id="ec-setup-pass" type="password" bind:value={setupPass} autocomplete="current-password" />
            <div class="row-actions">
              <button type="submit" disabled={settingUp || !setupPass} data-testid="ecash-setup">
                {settingUp ? "Setting up… (this can take a minute)" : "Set up"}
              </button>
            </div>
          </form>
        {/if}
      {/if}
    </div>
  {/if}

  {#if st?.state === "ready" && st.main && st.bids}
    <nav class="segments" aria-label="eCash">
      <button class:active={seg === "wallet"} on:click={() => (seg = "wallet")}>Wallet</button>
      <button class:active={seg === "bidding"} on:click={() => (seg = "bidding")}>Bidding</button>
    </nav>
    {#if seg === "wallet"}
    <div class="card" data-testid="ecash-main">
      <h3>eCash wallet</h3>
      <p class="ec-balance">{coins(st.main.trusted)}</p>
      {#if st.main.pending}<p class="muted small">{coins(st.main.pending)} on its way</p>{/if}
      {#if st.main.trusted + st.main.pending === 0 && st.bids.trusted === 0}
        <p class="small" data-testid="ecash-start">
          Start by receiving ECX: Receive gives your address, and BitWindow or any eCash wallet can send to it.
        </p>
      {/if}
      {#if sent}
        <p class="small" data-testid="ecash-sent">Sent. Transaction <span class="mono">{sent.slice(0, 16)}…</span></p>
      {/if}
      <div class="home-actions">
        <button on:click={receive}>Receive</button>
        <button class:active={mode === "send"} on:click={() => open("send")}>Send</button>
        <button class:active={mode === "bids"} on:click={() => open("bids")} title="Move ECX into the bidding wallet">To bidding</button>
      </div>

      <p class="muted small">
        ECX and sECX are worth the same: a deposit turns ECX into sECX one for one, from this wallet (Home › Deposit),
        and a withdrawal turns it back (Home › Withdraw).
      </p>
      {#if address}
        <div class="ec-receive">
          <QrCode text={address} size={180} />
          <p class="mono small ec-addr">{address}</p>
          <button class="link-btn" on:click={() => copy(address)}>{copied ? "Copied" : "Copy"}</button>
        </div>
      {/if}

      {#if mode}
        <div class="ec-pay" data-testid="ecash-pay">
          <h4>{mode === "send" ? "Send ECX" : "Move into the bidding wallet"}</h4>
          {#if !quote}
            {#if mode === "send"}
              <div class="field">
                <label class="field-label" for="ec-to">To (eCash address)</label>
                <input id="ec-to" type="text" bind:value={to} spellcheck="false" autocomplete="off" />
              </div>
            {/if}
            <div class="field">
              <label class="field-label" for="ec-amount">Amount (ECX)</label>
              <input id="ec-amount" type="text" bind:value={amount} disabled={everything} inputmode="decimal" />
              <label class="small"><input type="checkbox" bind:checked={everything} /> All of it, less the fee</label>
            </div>
            {#if payError}<p class="soft-error">{payError}</p>{/if}
            <div class="row-actions">
              <button class="secondary" on:click={close}>Cancel</button>
              <button on:click={prepare} disabled={busy}>{busy ? "Working out the fee…" : "Next"}</button>
            </div>
          {:else}
            <p class="small">
              {quote.to_bids ? "Into the bidding wallet" : "To"} <span class="mono">{quote.address}</span>
            </p>
            <p class="small">{coins(quote.sats)} arrives; fee {fmtEcx(quote.fee)}; {coins(quote.total)} leaves this wallet.</p>
            {#if quote.to_bids}
              <p class="muted small">The bidding wallet's key is kept on this computer so bids can go out with nobody there: someone who gets into your account here could spend it.</p>
            {/if}
            <form class="field" on:submit|preventDefault={confirm}>
              <label class="field-label" for="ec-pass">Wallet passphrase</label>
              <input id="ec-pass" type="password" bind:value={pass} autocomplete="current-password" />
              {#if payError}<p class="soft-error">{payError}</p>{/if}
              <div class="row-actions">
                <button type="button" class="secondary" on:click={close}>Cancel</button>
                <button type="submit" disabled={busy || !pass} data-testid="ecash-confirm">
                  {busy ? "Sending…" : quote.to_bids ? "Move it" : "Send"}
                </button>
              </div>
            </form>
          {/if}
        </div>
      {/if}
    </div>

    <div class="card">
      <h3>Payments</h3>
      {#if history.length === 0}
        <p class="muted small">None yet.</p>
      {:else}
        <ul class="ec-history">
          {#each merged(history).slice(0, 50) as t (t.wallet + t.txid + t.category + t.sats)}
            <li>
              <span class="ec-tag">{t.moved ? "Move" : t.wallet === "bids" ? "Bidding" : "eCash"}</span>
              <span>{txLine(t)}</span>
              <span class="muted small">
                {t.confirmations > 0 ? `${t.confirmations} confirmation${t.confirmations === 1 ? "" : "s"}` : "unconfirmed"}
              </span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    {:else}
    <div class="card" data-testid="ecash-bids">
      <h3>Bidding wallet</h3>
      <p class="ec-balance">{coins(st.bids.trusted)}</p>
      {#if st.bids.pending}<p class="muted small">{coins(st.bids.pending)} on its way</p>{/if}
      <p class="muted small">
        Bids for FreeBank blocks are paid from here, with nobody there to type a passphrase. Keep in it only what you
        mean to bid.
      </p>
      {#if backDone}<p class="small">Moved back. Transaction <span class="mono">{backDone.slice(0, 16)}…</span></p>{/if}
      {#if backQuote}
        <p class="small" data-testid="ecash-back-quote">
          {coins(backQuote.sats)} goes back to your main wallet; fee {fmtEcx(backQuote.fee)}.{bmm?.on ? " Bidding turns off too." : ""}
        </p>
        <div class="row-actions">
          <button on:click={moveBackConfirm} disabled={backBusy}>{backBusy ? "Moving…" : "Move back"}</button>
          <button class="secondary" on:click={() => (backQuote = null)} disabled={backBusy}>Cancel</button>
        </div>
      {:else}
        <div class="row-actions">
          <button class="secondary" on:click={moveBack} disabled={backBusy || st.bids.trusted === 0}>
            {backBusy ? "Working it out…" : "Move it all back"}
          </button>
        </div>
      {/if}

      {#if bmm}
        <div class="ec-bmm" data-testid="ecash-bmm">
          <h4>Bid for FreeBank blocks</h4>
          <p class="muted small">
            On each new eCash block, FreeBank bids for the next FreeBank block from this wallet. A winning bid goes to
            the eCash miner, and the FreeBank block it wins pays your FreeBank wallet that block's fees, if any. A losing bid costs nothing.
          </p>
          <div class="ec-bmm-fields">
            <div class="field">
              <label class="field-label" for="ec-bid">Bid per block (ECX)</label>
              <input id="ec-bid" type="text" bind:value={bidText} on:input={() => (bmmEdited = true)} inputmode="decimal" />
            </div>
            <div class="field">
              <label class="field-label" for="ec-cap">At most per day (ECX)</label>
              <input id="ec-cap" type="text" bind:value={capText} on:input={() => (bmmEdited = true)} inputmode="decimal" />
            </div>
          </div>
          {#if st.bids.trusted === 0 && !bmm.on}
            <p class="small">First move some ECX into this wallet: Wallet › To bidding.</p>
          {/if}
          <p class="small" data-testid="ecash-bmm-state">
            {#if bmm.on}
              On. {fmtEcx(bmm.spent_today)} of {fmtEcx(bmm.daily_cap)} ECX bid today; {bmm.won_today}
              {bmm.won_today === 1 ? "block" : "blocks"} won.
            {:else}
              Off.
            {/if}
          </p>
          {#if bmm.on && bmm.last}<p class="muted small">{bmm.last[1]}</p>{/if}
          {#if bmmError}<p class="soft-error">{bmmError}</p>{/if}
          <div class="row-actions">
            {#if bmm.on}
              <button class="secondary" on:click={() => saveBmm(false)} disabled={bmmBusy}>Turn off</button>
              <button on:click={() => saveBmm(true)} disabled={bmmBusy || !bmmEdited}>Save</button>
            {:else}
              <button class:secondary={st.bids.trusted === 0} on:click={() => saveBmm(true)} disabled={bmmBusy} data-testid="ecash-bmm-on">Turn on</button>
            {/if}
          </div>
          {#if bmm.rounds.length}
            <ul class="ec-history">
              {#each bmm.rounds.slice(0, 10) as r (r.txid)}
                <li>
                  <span>Block {r.height}</span>
                  <span>{OUTCOME[r.outcome] ?? r.outcome}</span>
                  <span class="muted small">{fmtEcx(r.fee)} ECX</span>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
    </div>

    {/if}
  {/if}
</div>

<style>
  .ecash {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .ec-intro:not(.card) {
    padding: 0 4px;
  }
  .ec-intro:not(.card) h2 {
    font-size: 17px;
    margin: 0;
  }
  .ec-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }
  .ec-balance {
    font-size: 24px;
    font-weight: 600;
    margin: 4px 0;
  }
  .ec-receive {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    margin-top: 12px;
  }
  .ec-addr {
    word-break: break-all;
  }
  .ec-pay {
    margin-top: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .ec-bmm {
    margin-top: 16px;
    padding-top: 12px;
    border-top: 1px solid var(--border-color, #333);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ec-bmm-fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .ec-history {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ec-history li {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    align-items: baseline;
  }
  /* Bid rounds: block, outcome, fee in even columns. */
  .ec-bmm .ec-history li {
    display: grid;
    grid-template-columns: 5.5em 1fr auto;
    gap: 10px;
  }
  .ec-tag {
    font-size: 12px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 4px;
    border: 1px solid var(--border-color, #ccc);
  }
</style>
