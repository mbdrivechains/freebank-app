<script lang="ts">
  // What a phone asks of this computer, shown over any screen: "Allow this phone?" while pairing
  // (each request with its comparison code: allow only the one your phone shows), and sends
  // waiting for an answer ("Phone X wants to send Y ECX to Z"): over the phone's daily
  // limit, or made while the wallet is locked and phone sends are off. A locked wallet asks for
  // its passphrase for that one send. A send nobody answers stops waiting after 10 minutes and
  // its alert goes away.
  import { onDestroy, onMount } from "svelte";
  import {
    when,
    type Approval,
    type PhoneDevice,
    onPhoneEvent,
    paymentButton,
    paymentDone,
    paymentWhat,
    phone,
    type HeldSend,
    type PairAsk,
    type PhoneWallet,
  } from "../lib/phone";
  import { nice } from "../lib/errors";
  import { showReceipt } from "../lib/receipts";

  let asks: PairAsk[] = [];
  let held: HeldSend[] = [];
  let wallet: PhoneWallet | null = null;
  let busy = false;
  let error = "";
  let pass = "";
  let needPass = false;
  let shown: string | undefined;
  let now = Math.floor(Date.now() / 1000);

  // "Approve sends on my phone" (v0.2.5): what the desktop is waiting for a phone to approve.
  let approvals: Approval[] = [];
  async function cancelApproval(a: Approval) {
    try {
      await phone.approvalCancel(a.id);
    } catch (e) {
      error = nice(e);
    }
  }

  async function load() {
    try {
      const s = await phone.status();
      now = Math.floor(Date.now() / 1000);
      asks = s.pair_pending;
      held = s.held;
      approvals = (await phone.approveInfo()).waiting;
    } catch {
      // The phone relay isn't available; nothing to show.
      return;
    }
    try {
      wallet = held.length ? await phone.wallet() : null;
    } catch {
      wallet = null;
    }
  }

  let off: (() => void) | null = null;
  let tick: ReturnType<typeof setInterval>;
  onMount(async () => {
    await load();
    off = await onPhoneEvent((name) => {
      if (name !== "phone-send") load();
    });
    tick = setInterval(() => {
      now = Math.floor(Date.now() / 1000);
      // Past its time: the app has dropped it (and told the phone); look again.
      if (send && now >= send.expires) load();
    }, 1000);
  });
  onDestroy(() => {
    off?.();
    clearInterval(tick);
  });

  // A phone pairing again (a new browser, the Home Screen app, a reset) usually comes back under the same name: allowing
  // it can remove the older pairings of that name, so they don't pile up (v0.2.5, operator 2026-10-03).
  let paired: PhoneDevice[] = [];
  let replace: Record<string, boolean> = {};
  $: if (asks.length) phone.devices().then((d) => (paired = d), () => (paired = []));
  // `list` passed in, so the dialog redraws when the devices arrive.
  const olderOf = (a: PairAsk, list: PhoneDevice[]) => list.filter((d) => d.name === a.name);
  // Ticked only when none of them was seen in the last three days: names are often just "iPhone", so a second phone in
  // the house would otherwise be cut off (security review L2).
  const RECENT = 3 * 24 * 3600;
  const replaceByDefault = (older: PhoneDevice[]) =>
    older.every((d) => (d.last_seen ?? d.added) < Date.now() / 1000 - RECENT);

  async function answerPair(a: PairAsk, allow: boolean) {
    busy = true;
    error = "";
    const older = allow && (replace[a.id] ?? replaceByDefault(olderOf(a, paired))) ? olderOf(a, paired) : [];
    try {
      await phone.pairAnswer(a.id, allow);
      for (const d of older) await phone.revoke(d.id);
    } catch (e) {
      error = nice(e);
    }
    busy = false;
    load();
  }

  async function answerSend(h: HeldSend, allow: boolean) {
    const p = pass;
    pass = "";
    busy = true;
    error = "";
    try {
      const r = await phone.confirmSend(h.confirm, allow, allow && askPass ? p : undefined);
      if (r.need_passphrase) {
        needPass = true;
        error = "Your wallet is locked. Enter its passphrase to send this payment.";
      } else if (allow && r.txid) {
        // An in-page receipt, not an alert() (macOS shows none): the txid stays on screen.
        showReceipt({
          txid: r.txid,
          what: `${paymentDone(h)} for ${h.name}`,
          rows: h.address ? [{ label: "To", value: h.address, mono: true }] : [],
        });
      }
    } catch (e) {
      error = nice(e);
    }
    busy = false;
    load();
  }

  function mmss(s: number) {
    s = Math.max(0, s);
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  }

  $: send = held[0];
  // A different send on show: start its dialog afresh.
  $: if (send?.confirm !== shown) {
    shown = send?.confirm;
    pass = "";
    needPass = false;
    error = "";
  }
  $: askPass = needPass || (!!wallet && wallet.encrypted === true && wallet.locked && !wallet.phone_send);
</script>

{#if approvals.length}
  {@const a = approvals[0]}
  <div class="phone-modal-back">
    <div class="phone-modal card" role="dialog" aria-modal="true" aria-labelledby="approve-title" data-testid="approval-wait">
      <h3 id="approve-title">Approve on your phone</h3>
      <p class="phone-name">{a.text}</p>
      <p class="muted small">
        Open FreeBank on your phone and approve it with Face ID. Waiting {mmss(a.expires - now)} more.
      </p>
      {#if error}<p class="soft-error">{error}</p>{/if}
      <div class="row-actions">
        <button class="secondary" on:click={() => cancelApproval(a)}>Cancel</button>
      </div>
    </div>
  </div>
{:else if asks.length}
  <div class="phone-modal-back">
    <div class="phone-modal card" role="dialog" aria-modal="true" aria-labelledby="pair-title">
      <h3 id="pair-title">Allow this phone?</h3>
      <p class="muted small">
        {#if asks.length > 1}
          {asks.length} phones opened your pairing code. Allow only the one showing the same code as your phone; the
          others are then refused.
        {:else}
          A phone opened your pairing code and asks to use this wallet.
        {/if}
      </p>
      {#each asks as a (a.id)}
        <div class="ask" data-testid="pair-ask">
          <p class="phone-name">{a.name}</p>
          <p>Allow only if your phone shows <strong class="pair-code">{a.code}</strong></p>
          {#if olderOf(a, paired).length}
            {@const older = olderOf(a, paired)}
            <label class="replace-row small">
              <input
                type="checkbox"
                checked={replace[a.id] ?? replaceByDefault(older)}
                on:change={(e) => (replace[a.id] = e.currentTarget.checked)}
              />
              Remove the older “{a.name}” pairing{older.length === 1 ? "" : `s (${older.length})`} here (last seen
              {older.map((d) => when(d.last_seen)).join(", ")}). Leave it unticked if that's another phone you still use.
              {#if older.some((d) => d.face_id)}It has Face ID: removed, it can no longer approve sends.{/if}
            </label>
          {/if}
          <div class="row-actions">
            <button on:click={() => answerPair(a, true)} disabled={busy}>Allow</button>
            <button class="secondary" on:click={() => answerPair(a, false)} disabled={busy}>Deny</button>
          </div>
        </div>
      {/each}
      {#if error}<p class="soft-error">{error}</p>{/if}
    </div>
  </div>
{:else if send}
  <div class="phone-modal-back">
    <div class="phone-modal card" role="dialog" aria-modal="true" aria-labelledby="held-title">
      <h3 id="held-title">{send.name} wants to {paymentWhat(send)}</h3>
      {#if send.address}
        <p class="muted small">to</p>
        <code class="phone-addr">{send.address}</code>
      {/if}
      <p class="muted small">
        {#if send.why === "locked"}
          Your wallet is locked, and phone payments aren't turned on in Settings, Phone.
        {:else}
          This is more than the phone may send today without asking.
        {/if}
        The phone waits {mmss(Math.min(send.expires - now, send.expires - send.at))} more for your answer.
        {#if held.length > 1}{held.length - 1} more after this one.{/if}
      </p>
      {#if send.face_id}<p class="muted small">Signed with Face ID on {send.name}.</p>{/if}
      {#if askPass}
        <form class="pass-row" on:submit|preventDefault={() => answerSend(send, true)}>
          <input
            type="password"
            bind:value={pass}
            placeholder="Wallet passphrase"
            aria-label="Wallet passphrase"
            autocomplete="off"
          />
        </form>
        <p class="muted small">Used for this payment only, then forgotten.</p>
      {/if}
      {#if error}<p class="soft-error">{error}</p>{/if}
      <div class="row-actions">
        <button on:click={() => answerSend(send, true)} disabled={busy || (askPass && !pass)}>
          {busy ? "Working…" : paymentButton(send)}
        </button>
        <button class="secondary" on:click={() => answerSend(send, false)} disabled={busy}>Decline</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .phone-modal-back {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .phone-modal {
    width: 100%;
    max-width: 400px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .phone-name {
    font-size: 18px;
    font-weight: 600;
  }
  .ask {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 10px;
    border-top: 1px solid var(--border-color);
  }
  .pair-code {
    font-size: 20px;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.04em;
    white-space: nowrap;
  }
  .phone-addr {
    padding: 10px;
    background: var(--bg-inset);
    border-radius: 8px;
    font-size: 12.5px;
    word-break: break-all;
  }
  .pass-row input {
    width: 100%;
  }
</style>
