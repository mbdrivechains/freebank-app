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
    paymentWhat,
    phone,
    when,
    type HeldSend,
    type KeepInfo,
    type PhoneDevice,
    type PhoneSend,
    type PhoneWallet,
    type RelayStatus,
    type ApproveInfo,
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
    try {
      approve = await phone.approveInfo();
      if (approve.over !== null && !approveDirty) approveAmount = String(approve.over);
    } catch {
      approve = null;
    }
  }

  // Its explanation: two sentences, the rest behind More (v0.2.6, the walk-through counted 14 lines).
  let approveMore = false;
  let iphoneMore = false;
  // "Approve sends on my phone" (v0.2.5; operator 2026-10-02: "yes..opt in I assume"; reworked after the security
  // review, operator 2026-10-03: "yes to that"): once this computer's payments in a day would come to more than the
  // amount, a phone's Face ID first. Off, or a higher amount, takes the phone too, or the recovery words and a day.
  let approve: ApproveInfo | null = null;
  let approveAmount = "1";
  let approveDirty = false;
  let approveBusy = false;
  let approveNote = "";
  let withWords = false;
  let words = "";
  async function setApprove(over: number | null, useWords = false) {
    approveBusy = true;
    approveNote = "";
    try {
      const due = await phone.approveSet(over, useWords ? words : undefined);
      words = "";
      withWords = false;
      approveDirty = false;
      approveNote = due
        ? `Done with your recovery words. It happens ${dueText(due)}, unless your phone declines it or you cancel it here.`
        : over === null
          ? "Off."
          : `Your phone now approves payments once a day's come to more than ${over} ${BASE_TICKER}.`;
    } catch (e) {
      approveNote = String(e);
    }
    approveBusy = false;
    load();
  }
  function dueText(due: number): string {
    return `on ${new Date(due * 1000).toLocaleString(undefined, { weekday: "long", hour: "numeric", minute: "2-digit" })}`;
  }
  async function cancelScheduled() {
    approveNote = "";
    try {
      await phone.approveCancelScheduled();
      approveNote = "Cancelled: nothing changes.";
    } catch (e) {
      approveNote = String(e);
    }
    load();
  }
  // Removing the last phone that can approve leaves payments over the day's amount, and pairing, blocked until the
  // recovery words turn the setting off (a day later): said before it happens (security review L2).
  $: approving = approve?.over != null;
  $: approverCount = devices.filter((d) => d.face_id).length;
  const LAST_APPROVER =
    "It's your last phone that can approve. Without one, payments over the day's amount and pairing a phone can't be " +
    "done until your recovery words turn approval off, a day later.";
  // `on` and `count` passed in, so the warning redraws when they change.
  const lastApprover = (gone: PhoneDevice[], on: boolean, count: number) =>
    on && gone.some((d) => d.face_id) && gone.filter((d) => d.face_id).length >= count;
  function saveApproveAmount() {
    const v = Number(approveAmount);
    if (!Number.isFinite(v) || v <= 0) {
      approveNote = `Enter an amount in ${BASE_TICKER} above zero.`;
      return;
    }
    setApprove(v, withWords);
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

  async function setLogin(box: HTMLInputElement) {
    const on = box.checked;
    keepBusy = true;
    keepNote = "";
    try {
      await phone.loginSet(on);
      keep = await phone.keepInfo();
      const st = await node.status();
      keepRestart = on && st.managed && !st.keeps_running;
    } catch (e) {
      keepNote = nice(e);
      keep = await phone.keepInfo().catch(() => keep);
    } finally {
      // The box shows what is so, also after a refusal (one-way `checked` sees no change to redraw).
      if (keep) box.checked = keep.at_login;
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

  // The list (v0.2.5, operator 2026-10-03: "why all these iphone settings? how to manage"): the phone seen last first,
  // one line each with its limit and Revoke behind Manage; names told apart by when they were paired; and phones not
  // seen for a week removed in one go.
  const WEEK = 7 * 24 * 3600;
  let managing: string | null = null;
  let tidying = false;
  $: sorted = [...devices].sort((a, b) => (b.last_seen ?? b.added) - (a.last_seen ?? a.added));
  $: stale = devices.filter((d) => !d.online && (d.last_seen ?? d.added) < Date.now() / 1000 - WEEK);
  function label(d: PhoneDevice): string {
    const same = devices.filter((x) => x.name === d.name).length > 1;
    return same ? `${d.name} (paired ${new Date(d.added * 1000).toLocaleDateString(undefined, { dateStyle: "medium" })})` : d.name;
  }
  async function removeStale() {
    error = "";
    for (const d of stale) {
      try {
        await phone.revoke(d.id);
        delete limits[d.id];
      } catch (e) {
        error = String(e);
      }
    }
    tidying = false;
    load();
  }

  const STATE_TEXT: Record<string, string> = {
    off: "Not connected.",
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

  // A held send is listed once, with its latest state (src-tauri/src/phone/store.rs). Its "held" line says
  // "waiting for you" only while it waits: lines from before v0.2.2 can list a held send twice.
  function resultText(s: PhoneSend, held: HeldSend[]): string {
    if (s.result === "held") {
      const id = s.held ?? (s.detail as { confirm?: string } | null)?.confirm;
      return held.some((h) => h.confirm === id) ? "waiting for you" : "was held for your answer";
    }
    if (s.result === "sent" && s.held) return "sent after you allowed it";
    // Why it failed, as the phone heard it (v0.2.6, the walk-through: the list said "failed" and no more).
    if (s.result === "failed" && typeof s.detail === "string" && s.detail) {
      const why = s.detail.replace(/^RPC error(?: -?\d+)?: /, "");
      return `failed: ${why.length > 120 ? why.slice(0, 117) + "…" : why}`;
    }
    return RESULT_TEXT[s.result] ?? s.result;
  }

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
      <p>
        <strong>Scan this with your phone:</strong> with its Camera app, or in FreeBank on the phone, tap
        <strong>Scan the code on your desktop</strong>.
      </p>
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
        <strong>On an iPhone,</strong> scan this with the Camera app: the page it opens shows how to put FreeBank on your
        Home Screen, where it pairs.
        {#if !iphoneMore}<button class="link-btn inline" on:click={() => (iphoneMore = true)}>More</button>{/if}
      </p>
      {#if iphoneMore}
        <p class="hint">
          Then open FreeBank on the Home Screen and tap <strong>Scan the code on your desktop</strong>. The Home Screen app
          keeps its own storage, apart from Safari, so it needs its own pairing, even if Safari has one, and shows here as
          a second phone. If its camera won't open, click Copy and tap <strong>Paste pairing link</strong> there: a Mac's
          Universal Clipboard passes the link to your iPhone.
        </p>
      {/if}
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
    {#if stale.length}
      {#if tidying}
        <div class="confirm-box">
          <p>
            Remove {stale.length} phone{stale.length === 1 ? "" : "s"} not seen for a week? Each is cut off at once; a
            phone you still use can pair again.
          </p>
          {#if lastApprover(stale, approving, approverCount)}<p class="soft-error">{LAST_APPROVER}</p>{/if}
          <div class="row-actions">
            <button class="danger" on:click={removeStale}>Remove {stale.length}</button>
            <button class="secondary" on:click={() => (tidying = false)}>Cancel</button>
          </div>
        </div>
      {:else}
        <button class="link-btn" on:click={() => (tidying = true)}>
          Remove {stale.length} phone{stale.length === 1 ? "" : "s"} not seen for a week…
        </button>
      {/if}
    {/if}
    <ul class="phone-list">
      {#each sorted as d (d.id)}
        <li>
          <div class="phone-head">
            <strong>{label(d)}</strong>
            {#if d.online}<span class="pill pill-ok">connected</span>{/if}
            {#if d.face_id}<span class="pill" title="The phone asks for Face ID{d.face_id_sends ? ' when it opens and before each send' : ' when it opens'}">Face ID</span>{/if}
            {#if d.face_id && approving}<span class="pill" title="It approves this computer's payments over the day's amount">approves</span>{/if}
          </div>
          <span class="muted small">
            Last seen {when(d.last_seen)} ·
            <button class="link-btn inline" on:click={() => (managing = managing === d.id ? null : d.id)} aria-expanded={managing === d.id}>
              {managing === d.id ? "Done" : "Manage"}
            </button>
          </span>
          {#if managing === d.id}
          <span class="muted small">Paired {when(d.added)}.</span>
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
              {#if lastApprover([d], approving, approverCount)}<p class="soft-error">{LAST_APPROVER}</p>{/if}
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
              {#if lastApprover([d], approving, approverCount)}<p class="soft-error">{LAST_APPROVER}</p>{/if}
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
        {#if keep.at_login_here}
          <label class="toggle-row">
            <input type="checkbox" checked={keep.at_login} disabled={keepBusy} on:change={(e) => setLogin(e.currentTarget)} />
            <span>Start it when I log in</span>
          </label>
          <p class="hint">
            Your phone reaches FreeBank whenever this computer is on, awake and logged in, even if you haven't opened
            FreeBank since: the node starts when your phone asks (usually within a minute or two) and, until you open
            FreeBank, stops again after half an hour without it. Until then a phone can see your balance and history and
            get an address; it can send within its limit only if your wallet has no passphrase.
          </p>
        {/if}
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

  {#if devices.length && approve}
    <div class="phone-send" data-testid="approve-sends">
      <label class="toggle-row">
        <input
          type="checkbox"
          checked={approve.over !== null}
          disabled={approveBusy || (approve.over === null && approve.approvers === 0)}
          on:change={(e) => (e.currentTarget.checked ? saveApproveAmount() : setApprove(null, withWords))}
        />
        <span>Approve sends on my phone</span>
      </label>
      <p class="hint">
        Once this computer's payments in a day come to more than the amount, your phone approves the next one with Face
        ID. It guards FreeBank, not the node directly.
        {#if !approveMore}<button class="link-btn inline" on:click={() => (approveMore = true)}>More</button>{/if}
      </p>
      {#if approveMore}
        <p class="hint">
          Every payment FreeBank makes here counts, with its fee: Send, Speed up, eCash, notes, bills, pools and houses,
          and a phone's payment you confirm here. Your phone also approves what would get round it: showing your recovery
          words, a new wallet or a restore, Obliterate, pairing another phone, Face ID on another phone or removing it, a
          higher daily limit for a phone, and turning this off or raising the amount. Lost your phone? Your recovery
          words turn it off a day later; your phones can cancel that while FreeBank is open on them.
        </p>
        <p class="hint">
          Someone with this computer and your wallet passphrase could still use the node directly, copy the wallet, or
          change FreeBank's files or this computer's clock. A phone still pays within its own daily limit without asking.
        </p>
      {/if}
      {#if approve.over === null && approve.approvers === 0}
        <p class="hint">First turn Face ID on in FreeBank on your phone (its Settings).</p>
      {:else}
        <label class="limit-row">
          <span class="small">Ask my phone once a day's payments come to more than</span>
          <span class="input-with-btn">
            <input type="number" min="0" step="0.01" bind:value={approveAmount} on:input={() => ((approveDirty = true), (approveNote = ""))} />
            {#if approve.over !== null}
              <button class="secondary" on:click|preventDefault={saveApproveAmount} disabled={approveBusy || !approveDirty}>Save</button>
            {/if}
          </span>
        </label>
      {/if}
      {#if approve.over !== null && approve.left !== null}
        <p class="muted small" data-testid="approve-left">
          {approve.left} {BASE_TICKER} left today without asking.
        </p>
      {/if}
      {#if approve.scheduled}
        <div class="confirm-box" data-testid="approve-scheduled">
          <p>
            Your recovery words were used:
            {approve.scheduled.over === null ? "this turns off" : `the amount goes up to ${approve.scheduled.over} ${BASE_TICKER}`}
            {dueText(approve.scheduled.due)}, unless your phone declines it or you cancel it here.
          </p>
          <div class="row-actions">
            <button class="secondary" on:click={cancelScheduled}>Cancel it</button>
          </div>
        </div>
      {:else if approve.over !== null}
        {#if withWords}
          <label class="words-row">
            <span class="small">Your 24 recovery words, instead of your phone. The change then waits a day.</span>
            <textarea rows="3" bind:value={words} autocomplete="off" spellcheck="false"></textarea>
          </label>
        {:else}
          <button class="link-btn inline" on:click={() => (withWords = true)}>Lost your phone? Use your recovery words instead</button>
        {/if}
      {/if}
      {#if approveBusy}<p class="hint">Waiting for your phone…</p>{/if}
      {#if approveNote}<p class="hint">{approveNote}</p>{/if}
    </div>
  {/if}

  {#if sends.length}
    <h4 class="phone-sub">Sends from phones</h4>
    <ul class="phone-sends">
      {#each sends as s}
        <li>
          <span>{s.name}: {paymentWhat(s)}{#if s.address}{" "}to <code>{s.address}</code>{/if}</span>
          <span class="muted small">{when(s.time)} · {resultText(s, status?.held ?? [])}</span>
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
