<script lang="ts">
  // The receipt for a transaction the app just made, in the page (alert() does nothing on macOS):
  // what was done, the txid with Copy, "View on explorer" in the system browser, and a live status,
  // Unconfirmed, then 1, 2 and 3 confirmations (Confirmed), each with its block's height and the time
  // since sending. It asks gettransaction on each new block ($tip) and stops once confirmed.
  //
  //   <TxReceipt {txid} what="Sent 1.5 ECX to X…" rows={[{ label: "Fee", value: "0.00000226 ECX" }]}
  //       on:close={…} on:status={(e) => …}>
  //     <svelte:fragment slot="rows">…more <div><dt>…</dt><dd>…</dd></div>…</svelte:fragment>
  //     <svelte:fragment slot="actions" let:confirmed let:replaced>
  //       {#if !confirmed && !replaced}<button class="secondary" on:click={speedUp}>Speed up</button>{/if}
  //     </svelte:fragment>
  //   </TxReceipt>
  //
  // Give it a new `txid` (after a Speed up) and it follows that one from Unconfirmed. Both slots get
  // confirmations, confirmed and replaced. `status` fires on every change (TxStatus, lib/receipts.ts).
  // `collapsed` folds it to its headline; the chevron opens and folds it again.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { api, type WalletTx } from "../lib/api";
  import { RPC, rpcCode } from "../lib/errors";
  import { openUrl } from "../lib/node";
  import { CONFIRMED_AT, explorerTxUrl, tip, type ReceiptRow, type TxStatus } from "../lib/receipts";

  /** The transaction to follow. */
  export let txid: string;
  /** What was done, in words. */
  export let what: string;
  /** When it was sent, ms since the epoch; by default when the receipt first showed. */
  export let sentAt: number = Date.now();
  /** Extra lines under the transaction ID. */
  export let rows: ReceiptRow[] = [];
  /** Show the × that dispatches `close`. */
  export let dismissible = true;
  /** Only the headline (what, status, time); the chevron shows the rest. */
  export let collapsed = false;

  const dispatch = createEventDispatcher<{ close: void; status: TxStatus }>();
  const STEPS = Array.from({ length: CONFIRMED_AT }, (_, i) => i + 1);

  let confirmations = 0;
  let replaced = false;
  let missing = false;
  let baseHeight: number | null = null; // the block the transaction went into
  let baseHash = "";
  // When confirmation k was seen (index k - 1): ms, 0 if it was already there at the first look.
  let seenAt: (number | undefined)[] = [];
  let firstLook = true;
  let following = "";
  let now = Date.now();
  let destroyed = false;

  $: confirmed = confirmations >= CONFIRMED_AT;
  $: live = !confirmed && !replaced && !missing;
  $: status = missing
    ? "Not in this wallet"
    : replaced
      ? "Replaced"
      : confirmed
        ? "Confirmed"
        : confirmations === 0
          ? "Unconfirmed"
          : `${confirmations} confirmation${confirmations === 1 ? "" : "s"}`;

  // A new txid starts from Unconfirmed.
  $: if (txid !== following) start(txid);

  function start(id: string) {
    following = id;
    confirmations = 0;
    replaced = false;
    missing = false;
    baseHeight = null;
    baseHash = "";
    seenAt = [];
    firstLook = true;
    check();
  }

  // Look again on each new block while the transaction is on its way; stop once it's settled.
  let unsubTip: (() => void) | null = null;
  $: if (live && !unsubTip && !destroyed) unsubTip = tip.subscribe((h) => h !== null && check());
  $: if (!live && unsubTip) stopTip();
  function stopTip() {
    unsubTip?.();
    unsubTip = null;
  }

  let checking = false;
  let again = false;
  async function check() {
    if (checking) {
      again = true;
      return;
    }
    checking = true;
    try {
      do {
        again = false;
        await look();
      } while (again && !destroyed);
    } finally {
      checking = false;
    }
  }

  // A transaction this wallet isn't part of (a batch lock the house's keys co-signed: the mint's wallet pays it) is
  // followed on the chain instead: in the mempool through getrawtransaction, then through its unspent outputs.
  let outs = 4;
  async function fromChain(id: string): Promise<{ confirmations: number; blockhash?: string } | null> {
    try {
      const r = await api.getRawTransaction(id);
      outs = Math.max(1, Math.min(r.vout?.length ?? outs, 8));
      return { confirmations: Number(r.confirmations) || 0, blockhash: r.blockhash };
    } catch {
      // Not in the mempool, and no -txindex: ask its outputs.
    }
    for (let n = 0; n < outs; n++) {
      try {
        const o = await api.getTxOut(id, n);
        if (o) return { confirmations: Number(o.confirmations) || 0 };
      } catch {
        return null;
      }
    }
    return null;
  }

  async function look() {
    const id = txid;
    let t: WalletTx;
    try {
      t = await api.getTransaction(id);
    } catch (e) {
      if (id !== txid || rpcCode(e) !== RPC.INVALID_ADDRESS_OR_KEY) return; // a busy or restarting node: try again on the next block
      const c = await fromChain(id);
      if (id !== txid || destroyed) return;
      if (!c) {
        missing = true;
        emit();
        return;
      }
      t = { txid: id, confirmations: c.confirmations, blockhash: c.blockhash, time: 0, amount: 0 };
    }
    if (id !== txid || destroyed) return;
    const c = Number(t.confirmations) || 0;
    if (c > 0 && t.blockhash && t.blockhash !== baseHash) {
      try {
        const h = await api.getBlockHeader(t.blockhash);
        if (id !== txid) return;
        baseHash = t.blockhash;
        baseHeight = h.height;
      } catch {
        // The height shows on the next block.
      }
    } else if (c <= 0) {
      baseHash = "";
      baseHeight = null;
    }
    const at = Date.now();
    seenAt = STEPS.map((k, i) => (c >= k ? seenAt[i] ?? (firstLook ? 0 : at) : undefined));
    firstLook = false;
    replaced = c < 0;
    missing = false;
    confirmations = Math.max(0, c);
    emit();
  }

  let emitted = "";
  function emit() {
    const st: TxStatus = { txid, confirmations, confirmed: confirmations >= CONFIRMED_AT, replaced, missing };
    const key = JSON.stringify(st);
    if (key === emitted) return;
    emitted = key;
    dispatch("status", st);
  }

  function span(ms: number): string {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return `${s} s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m} min`;
    const h = Math.floor(m / 60);
    // History's details open old transactions too.
    if (h >= 48) return `${Math.floor(h / 24)} days`;
    return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  }

  let copied = false;
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let idBox: HTMLElement;
  async function copy() {
    try {
      await navigator.clipboard.writeText(txid);
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = false), 2000);
    } catch {
      // No clipboard: select it, so Ctrl+C / Cmd+C copies it.
      const sel = window.getSelection();
      if (sel && idBox) sel.selectAllChildren(idBox);
    }
  }

  let clock: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    clock = setInterval(() => (now = Date.now()), 1000);
  });
  onDestroy(() => {
    destroyed = true;
    clearInterval(clock);
    clearTimeout(copyTimer);
    stopTip();
  });
</script>

<section class="card receipt" aria-label="Receipt">
  <div class="rc-head">
    <span class="rc-mark" class:ok={confirmed} class:bad={replaced || missing} aria-hidden="true">
      {confirmed ? "✓" : replaced || missing ? "!" : "↑"}
    </span>
    <div class="rc-title">
      <div class="rc-what">{what}</div>
      <div class="rc-status" aria-live="polite">
        <strong>{status}</strong> · sent {span(now - sentAt)} ago
      </div>
    </div>
    <button
      type="button"
      class="ghost rc-x rc-fold"
      aria-expanded={!collapsed}
      aria-label={collapsed ? "Show details" : "Hide details"}
      on:click={() => (collapsed = !collapsed)}>{collapsed ? "▸" : "▾"}</button
    >
    {#if dismissible}
      <button type="button" class="ghost rc-x" aria-label="Close this receipt" on:click={() => dispatch("close")}>×</button>
    {/if}
  </div>

  {#if !collapsed}
    {#if replaced}
      <p class="hint rc-note">A different transaction spending the same coins confirmed instead, so this one won't.</p>
    {:else if missing}
      <p class="hint rc-note">This wallet doesn't know this transaction. The explorer may still show it.</p>
    {:else}
      <ol class="rc-steps" aria-label="Confirmations" style="--cols: {STEPS.length + 1}">
        <li class="done">
          <span class="rc-dot" aria-hidden="true"></span>
          <span class="rc-lbl">Sent</span>
        </li>
        {#each STEPS as k}
          <li class:done={confirmations >= k} class:next={confirmations === k - 1}>
            <span class="rc-dot" aria-hidden="true"></span>
            <span class="rc-lbl">{k} conf</span>
            {#if confirmations >= k}
              {#if baseHeight !== null}<span class="rc-sub">block {(baseHeight + k - 1).toLocaleString()}</span>{/if}
              {#if seenAt[k - 1]}<span class="rc-sub">after {span((seenAt[k - 1] ?? 0) - sentAt)}</span>{/if}
            {/if}
          </li>
        {/each}
      </ol>
      {#if !confirmed}
        <p class="hint rc-note">
          Each confirmation is a new FreeBank block. They follow eCash blocks, so expect minutes, not seconds.
        </p>
      {/if}
    {/if}

    <div class="rc-id">
      <span class="rc-k">Transaction ID</span>
      <div class="address-display">
        <code bind:this={idBox}>{txid}</code>
        <button type="button" on:click={copy}>{copied ? "Copied" : "Copy"}</button>
      </div>
    </div>

    {#if rows.length || $$slots.rows}
      <dl class="facts rc-rows">
        {#each rows as r}
          <div><dt>{r.label}</dt><dd class:mono={r.mono}>{r.value}</dd></div>
        {/each}
        <slot name="rows" {confirmations} {confirmed} {replaced} />
      </dl>
    {/if}

    <div class="rc-actions">
      <button type="button" class="secondary" on:click={() => openUrl(explorerTxUrl(txid))}>View on explorer ↗</button>
      <slot name="actions" {confirmations} {confirmed} {replaced} />
    </div>
  {/if}
</section>

<style>
  .receipt {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .rc-head {
    display: flex;
    align-items: flex-start;
    gap: 10px;
  }
  .rc-mark {
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 700;
    color: var(--accent-color);
    background: var(--accent-tint);
  }
  .rc-mark.ok {
    color: var(--success-color);
    background: rgba(92, 196, 138, 0.12);
  }
  .rc-mark.bad {
    color: var(--error-color);
    background: rgba(229, 116, 106, 0.1);
  }
  .rc-title {
    flex: 1;
    min-width: 0;
  }
  .rc-what {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .rc-status {
    font-size: 13px;
    color: var(--text-secondary);
  }
  .rc-status strong {
    color: var(--text-color);
    font-weight: 600;
  }
  .rc-x {
    padding: 0 6px;
    font-size: 18px;
    line-height: 1.2;
  }
  .rc-fold {
    font-size: 14px;
  }
  .rc-steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(var(--cols), 1fr);
    gap: 4px;
    position: relative;
  }
  .rc-steps::before {
    content: "";
    position: absolute;
    top: 6px;
    left: calc(50% / var(--cols));
    right: calc(50% / var(--cols));
    height: 2px;
    background: var(--border-color);
  }
  .rc-steps li {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 2px;
    font-size: 12px;
    color: var(--text-secondary);
  }
  .rc-dot {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--border-color);
    background: var(--bg-secondary);
    margin-bottom: 2px;
  }
  .rc-steps li.done .rc-dot {
    background: var(--success-color);
    border-color: var(--success-color);
  }
  .rc-steps li.next .rc-dot {
    border-color: var(--accent-color);
    animation: rc-pulse 1.6s ease-in-out infinite;
  }
  .rc-steps li.done .rc-lbl {
    color: var(--text-color);
  }
  .rc-sub {
    font-size: 11px;
  }
  .rc-note {
    margin: 0;
  }
  .rc-id {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .rc-id .address-display {
    margin-bottom: 0;
  }
  .rc-k {
    font-size: 12.5px;
    color: var(--text-secondary);
  }
  .rc-rows {
    margin-top: 0;
  }
  .rc-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .rc-actions :global(button) {
    flex: 1;
    white-space: nowrap;
    padding: 9px 14px;
    font-size: 14px;
  }
  @keyframes rc-pulse {
    50% {
      box-shadow: 0 0 0 4px var(--accent-tint);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rc-steps li.next .rc-dot {
      animation: none;
    }
  }
</style>
