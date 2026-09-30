<script lang="ts">
  // Settings, "Phone": pair a phone through the relay (a QR code it opens in Safari or Firefox),
  // the phones allowed in, each with its own daily limit and Revoke, "Let my phone send while
  // FreeBank is open" for a wallet with a passphrase, the relay address and link state, and the
  // latest sends phones made. "Allow this phone?" and held sends are answered in PhoneAlerts,
  // which shows over any screen.
  import { onDestroy, onMount } from "svelte";
  import QrCode from "./QrCode.svelte";
  import { BASE_TICKER } from "../lib/brand";
  import {
    onPhoneEvent,
    phone,
    when,
    type KeepInfo,
    type PhoneDevice,
    type PhoneSend,
    type PhoneWallet,
    type RelayStatus,
  } from "../lib/phone";
  import { node } from "../lib/node";
  import { nice } from "../lib/errors";

  let status: RelayStatus | null = null;
  let devices: PhoneDevice[] = [];
  let sends: PhoneSend[] = [];
  let error = "";
  let relayInput = "";
  let relayDirty = false;
  let relayNote = "";

  let pair: { url: string; expires: number } | null = null;
  let left = 0;
  let copied = false;
  let limits: Record<string, string> = {};
  let limitNote: Record<string, string> = {};
  let revoking: string | null = null;

  // Phone sends from a wallet with a passphrase. The passphrase is typed here, handed to the app
  // once, and cleared from this screen at once; the app keeps it in memory only.
  let wallet: PhoneWallet | null = null;
  let askPass = false;
  let pass = "";
  let sendBusy = false;
  let sendNote = "";

  async function load() {
    try {
      [status, devices, sends] = await Promise.all([phone.status(), phone.devices(), phone.recentSends()]);
      if (!relayDirty) relayInput = status.url;
      for (const d of devices) if (!(d.id in limits)) limits[d.id] = String(d.limit);
      error = "";
    } catch (e) {
      error = String(e);
    }
    try {
      wallet = devices.length ? await phone.wallet() : null;
    } catch {
      wallet = null;
    }
    try {
      keep = await phone.keepInfo();
    } catch {
      keep = null;
    }
  }

  // "Keep your phone connected when FreeBank is closed" (asked once, after the first phone pairs).
  let keep: KeepInfo | null = null;
  let keepNote = "";
  let keepBusy = false;
  // "Keep running" came with it, but the node started before: it stops with the app until restarted.
  let keepRestart = false;
  async function restartNode() {
    keepBusy = true;
    keepNote = "";
    try {
      await node.restart();
      keepRestart = false;
    } catch (e) {
      keepNote = nice(e);
    } finally {
      keepBusy = false;
    }
  }

  async function setKeep(on: boolean) {
    keepBusy = true;
    keepNote = "";
    try {
      await phone.keepSet(on);
      keep = await phone.keepInfo();
      // "Keep running" comes with it; a node started before that stops with the app until it restarts.
      keepRestart = on && !(await node.status()).keeps_running;
    } catch (e) {
      keepNote = nice(e);
    } finally {
      keepBusy = false;
    }
  }

  async function toggleSend(e: Event) {
    const box = e.currentTarget as HTMLInputElement;
    sendNote = "";
    if (box.checked) {
      // On only once the passphrase checks out.
      box.checked = false;
      askPass = true;
      return;
    }
    sendBusy = true;
    try {
      await phone.sendOff();
      sendNote = "Off. Phone payments wait for you here.";
    } catch (err) {
      sendNote = String(err);
    }
    sendBusy = false;
    load();
  }

  async function sendOn() {
    const p = pass;
    pass = "";
    sendBusy = true;
    sendNote = "";
    try {
      await phone.sendOn(p);
      askPass = false;
      sendNote = keep?.keep
        ? "On until you turn it off or remove your last phone. When FreeBank closes, the background part keeps it until FreeBank opens again."
        : "On until you turn it off, remove your last phone or quit FreeBank.";
    } catch (err) {
      sendNote = String(err);
    }
    sendBusy = false;
    load();
  }

  function cancelOn() {
    pass = "";
    askPass = false;
    sendNote = "";
  }

  let off: (() => void) | null = null;
  let tick: ReturnType<typeof setInterval>;
  let knownIds = new Set<string>();
  onMount(async () => {
    await load();
    knownIds = new Set(devices.map((d) => d.id));
    off = await onPhoneEvent(async () => {
      await load();
      // A new phone came in: the QR code has done its job.
      if (pair && devices.some((d) => !knownIds.has(d.id))) pair = null;
      knownIds = new Set(devices.map((d) => d.id));
    });
    tick = setInterval(() => {
      if (pair) {
        left = Math.max(0, pair.expires - Math.floor(Date.now() / 1000));
        if (left === 0) pair = null;
      }
    }, 1000);
  });
  onDestroy(() => {
    off?.();
    clearInterval(tick);
  });

  async function startPairing() {
    error = "";
    copied = false;
    try {
      pair = await phone.pairStart();
      left = Math.max(0, pair.expires - Math.floor(Date.now() / 1000));
      load();
    } catch (e) {
      error = String(e);
    }
  }

  async function copy() {
    if (!pair) return;
    await navigator.clipboard.writeText(pair.url);
    copied = true;
  }

  async function saveRelay() {
    relayNote = "";
    try {
      await phone.setRelay(relayInput);
      relayDirty = false;
      relayNote = "Saved.";
      pair = null;
      load();
    } catch (e) {
      relayNote = String(e);
    }
  }

  async function saveLimit(d: PhoneDevice) {
    const v = Number(limits[d.id]);
    if (!Number.isFinite(v) || v < 0) {
      limitNote[d.id] = "Enter an amount, 0 or more.";
      return;
    }
    try {
      await phone.setLimit(d.id, v);
      limitNote[d.id] = "Saved.";
      load();
    } catch (e) {
      limitNote[d.id] = String(e);
    }
  }

  // "Remove Face ID": for a phone that lost its passkey.
  let unlocking: string | null = null;
  async function removeFaceId(id: string) {
    try {
      await phone.removePasskey(id);
      unlocking = null;
      load();
    } catch (e) {
      error = String(e);
    }
  }

  async function revoke(id: string) {
    try {
      await phone.revoke(id);
      revoking = null;
      delete limits[id];
      load();
    } catch (e) {
      error = String(e);
    }
  }

  const STATE_TEXT: Record<string, string> = {
    off: "Not connected: no phone is paired.",
    connecting: "Connecting to the relay…",
    online: "Connected to the relay.",
    retrying: "Can't reach the relay.",
  };

  const RESULT_TEXT: Record<string, string> = {
    sent: "sent",
    held: "waiting for you",
    declined: "declined",
    failed: "failed",
    expired: "not answered in time",
    cancelled: "cancelled when FreeBank restarted",
    refused: "refused: FreeBank was closed",
  };

  function mmss(s: number) {
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  }
</script>

<div class="card phone-card">
  <h3>Phone</h3>
  <p class="muted small">
    Use this wallet from your phone's browser. The phone talks to this computer through a relay that only passes
    sealed messages along; your keys stay on your node. A phone can see your balance and history, get an address,
    and send up to its daily limit. Anything above that waits for you here.
  </p>

  {#if status}
    <div class="phone-link">
      <span class="dot" class:ok={status.state === "online"} class:bad={status.state === "retrying"}></span>
      <span>{STATE_TEXT[status.state] ?? status.state}</span>
      {#if status.state === "retrying" && status.detail}<span class="muted small">{status.detail}</span>{/if}
    </div>
  {/if}

  {#if pair}
    <div class="pair-box">
      <p><strong>Scan this with your phone's camera.</strong> It opens FreeBank in the phone's browser.</p>
      <QrCode text={pair.url} />
      <div class="address-display">
        <code>{pair.url}</code>
        <button on:click={copy}>{copied ? "Copied" : "Copy"}</button>
      </div>
      <p class="hint">
        Works once, for {mmss(left)} more. Your phone then shows a 6-digit code, and this computer asks you to allow
        it: allow it only if both show the same code. Anyone who sees this QR code can ask to pair, so only show it to
        your own phone.
      </p>
      <p class="hint">
        For FreeBank on an iPhone's Home Screen, copy the link instead and tap <strong>Paste pairing link</strong> in
        the Home Screen app: the icon keeps its own storage, apart from Safari's. A Mac's Universal Clipboard passes the
        link to your iPhone.
      </p>
      <div class="row-actions">
        <button class="secondary" on:click={() => (pair = null)}>Close</button>
      </div>
    </div>
  {:else}
    <div class="row-actions">
      <button on:click={startPairing}>Pair a phone</button>
    </div>
  {/if}

  {#if devices.length}
    <ul class="phone-list">
      {#each devices as d (d.id)}
        <li>
          <div class="phone-head">
            <strong>{d.name}</strong>
            {#if d.online}<span class="pill pill-ok">connected</span>{/if}
            {#if d.face_id}<span class="pill" title="The phone asks for Face ID{d.face_id_sends ? ' when it opens and before each send' : ' when it opens'}">Face ID</span>{/if}
          </div>
          <span class="muted small">Added {when(d.added)} · last seen {when(d.last_seen)}</span>
          <label class="limit-row">
            <span class="small">Daily limit without asking</span>
            <span class="input-with-btn">
              <input type="number" min="0" step="0.01" bind:value={limits[d.id]} on:input={() => (limitNote[d.id] = "")} />
              <button class="secondary" on:click|preventDefault={() => saveLimit(d)}>Save</button>
            </span>
          </label>
          <span class="muted small">
            {d.spent_today} of {d.limit} {BASE_TICKER} used today.{limitNote[d.id] ? ` ${limitNote[d.id]}` : ""}
          </span>
          {#if revoking === d.id}
            <div class="confirm-box">
              <p>{d.name} is cut off at once, and anything it has waiting for you is declined. To use it again, pair it again.</p>
              <div class="row-actions">
                <button class="danger" on:click={() => revoke(d.id)}>Revoke</button>
                <button class="secondary" on:click={() => (revoking = null)}>Cancel</button>
              </div>
            </div>
          {:else if unlocking === d.id}
            <div class="confirm-box">
              <p>
                {d.name} opens without Face ID until it is turned on again there. Do this only for a phone that lost its
                passkey (a new phone, or its passwords reset).
              </p>
              <div class="row-actions">
                <button class="danger" on:click={() => removeFaceId(d.id)}>Remove Face ID</button>
                <button class="secondary" on:click={() => (unlocking = null)}>Cancel</button>
              </div>
            </div>
          {:else}
            <span class="device-actions">
              <button class="link-btn inline" on:click={() => (revoking = d.id)}>Revoke…</button>
              {#if d.face_id}<button class="link-btn inline" on:click={() => (unlocking = d.id)}>Remove Face ID…</button>{/if}
            </span>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if status}
    <p class="hint">No phone is paired yet.</p>
  {/if}

  {#if devices.length && keep}
    <div class="phone-keep" data-testid="phone-keep">
      {#if !keep.asked}
        <p><strong>Keep your phone connected when FreeBank is closed?</strong></p>
        <p class="hint">
          The window closes, but a small background part of FreeBank keeps the node and the phone link running while
          this computer is on and awake (asleep, your phone shows "Desktop offline"). Nothing can wait for you then: a
          send over the limit is refused, and the phone says to open FreeBank. With phone sends on, your wallet
          passphrase stays in that part's memory too (never on disk).
        </p>
        <div class="row-actions">
          <button on:click={() => setKeep(true)} disabled={keepBusy}>Keep connected</button>
          <button class="secondary" on:click={() => setKeep(false)} disabled={keepBusy}>Only while FreeBank is open</button>
        </div>
      {:else}
        <label class="toggle-row">
          <input type="checkbox" checked={keep.keep} disabled={keepBusy} on:change={(e) => setKeep(e.currentTarget.checked)} />
          <span>Keep my phone connected when FreeBank is closed</span>
        </label>
        <p class="hint">
          A small background part of FreeBank keeps the node and the phone link running while this computer is on and
          awake; with phone sends on, it keeps your passphrase in memory too. Opening FreeBank takes the phone back. To
          stop it all, close FreeBank with "Stop everything and close".
        </p>
      {/if}
      {#if keep.took_back}<p class="hint">Your phone stayed connected while FreeBank was closed, from {when(keep.took_back)}.</p>{/if}
      {#if keep.take_back_error}<p class="soft-error">{keep.take_back_error}</p>{/if}
      {#if keepRestart}
        <p class="hint">Your node started before this was on, so it would still stop when you close FreeBank. Restart it to keep it running.</p>
        <button class="secondary" on:click={restartNode} disabled={keepBusy}>{keepBusy ? "Restarting…" : "Restart the node"}</button>
      {/if}
      {#if keepNote}<p class="hint">{keepNote}</p>{/if}
    </div>
  {/if}

  {#if devices.length && wallet && wallet.encrypted !== null}
    <div class="phone-send" data-testid="phone-send">
      {#if wallet.encrypted}
        <label class="toggle-row">
          <input type="checkbox" checked={wallet.phone_send} disabled={sendBusy} on:change={toggleSend} />
          <span>Let my phone send while FreeBank is open</span>
        </label>
        <p class="hint">
          Your wallet has a passphrase. With this on, FreeBank keeps it in memory while it is open, never on disk, and
          unlocks the wallet for a few seconds for each phone payment within the daily limit. With it off, every phone
          payment waits for you here.
        </p>
        {#if askPass}
          <form class="pass-row" on:submit|preventDefault={sendOn}>
            <input
              type="password"
              bind:value={pass}
              placeholder="Wallet passphrase"
              aria-label="Wallet passphrase"
              autocomplete="off"
            />
            <button type="submit" disabled={sendBusy || !pass}>{sendBusy ? "Checking…" : "Turn on"}</button>
            <button type="button" class="secondary" on:click={cancelOn}>Cancel</button>
          </form>
        {/if}
      {:else}
        <p class="hint">
          Your wallet has no passphrase, so a phone sends up to its daily limit without asking. There is nothing to
          unlock.
        </p>
      {/if}
      {#if sendNote}<p class="hint">{sendNote}</p>{/if}
    </div>
  {/if}

  {#if sends.length}
    <h4 class="phone-sub">Sends from phones</h4>
    <ul class="phone-sends">
      {#each sends as s}
        <li>
          <span>{s.name}: {s.amount} {BASE_TICKER} to <code>{s.address}</code></span>
          <span class="muted small">{when(s.time)} · {RESULT_TEXT[s.result] ?? s.result}</span>
        </li>
      {/each}
    </ul>
  {/if}

  <details class="advanced">
    <summary>Relay</summary>
    <div class="adv-body">
      <label>
        Relay address
        <input
          type="text"
          bind:value={relayInput}
          on:input={() => { relayDirty = true; relayNote = ""; }}
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
        />
      </label>
      <p class="hint">
        Phones already paired keep the relay they were paired with; pair them again after changing this.
        {#if status}Room <code>{status.room}</code>.{/if}
      </p>
      <div class="adv-actions">
        <button class="secondary" on:click={saveRelay} disabled={!relayDirty}>Save</button>
      </div>
      {#if relayNote}<p class="hint">{relayNote}</p>{/if}
    </div>
  </details>

  {#if error}<p class="soft-error">{error}</p>{/if}
</div>

<style>
  .phone-card {
    margin-top: 16px;
  }
  .phone-link {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 12px 0;
    font-size: 13.5px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-secondary);
  }
  .dot.ok {
    background: var(--success-color);
  }
  .dot.bad {
    background: var(--error-color);
  }
  .pair-box {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 14px;
    border: 1px solid var(--border-color);
    border-radius: 12px;
    background: var(--bg-inset);
  }
  .pair-box .address-display {
    width: 100%;
    margin: 0;
  }
  .pair-box .row-actions {
    width: 100%;
  }
  .phone-list,
  .phone-sends {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 14px;
  }
  .phone-list li,
  .phone-sends li {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
  }
  .phone-sends li {
    gap: 2px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .phone-sends code {
    word-break: break-all;
  }
  .phone-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .limit-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .limit-row input {
    max-width: 140px;
  }
  .phone-sub {
    margin-top: 16px;
    font-size: 14px;
  }
  .phone-send {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 14px;
    padding: 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
  }
  .toggle-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
  }
  .toggle-row input {
    width: auto;
  }
  /* One line: the field gives way, so Cancel never wraps on its own. */
  .device-actions {
    display: flex;
    gap: 14px;
  }
  .phone-keep {
    margin-top: 16px;
  }
  .pass-row {
    display: flex;
    gap: 8px;
  }
  .pass-row input {
    flex: 1 1 120px;
    min-width: 0;
  }
  .pass-row button {
    flex: none;
  }
  .phone-card .advanced {
    margin-top: 16px;
  }
  .phone-card .link-btn.inline {
    align-self: flex-start;
  }
</style>
