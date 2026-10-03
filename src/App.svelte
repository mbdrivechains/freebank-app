<script lang="ts">
  import { onMount } from "svelte";
  import { api, getSavedConfig, type Transaction, type BlockchainInfo, type NoteHolding, type House, type Pool, type LpHolding, type Bill } from "./lib/api";
  import {
    APP_NAME,
    APP_TAGLINE,
    BASE_TICKER,
    DEFAULT_PORT,
    CONN_PRESETS,
    type ConnMode,
  } from "./lib/brand";
  import BalanceCard from "./components/BalanceCard.svelte";
  import Setup from "./components/Setup.svelte";
  import NodeStatus from "./components/NodeStatus.svelte";
  import NodeSettings from "./components/NodeSettings.svelte";
  import PathText from "./components/PathText.svelte";
  import PhoneSettings from "./components/PhoneSettings.svelte";
  import PhoneAlerts from "./components/PhoneAlerts.svelte";
  import QuitNotice from "./components/QuitNotice.svelte";
  import UnlockPrompt from "./components/UnlockPrompt.svelte";
  import Notice from "./components/Notice.svelte";
  import SendPanel from "./components/SendPanel.svelte";
  import SendReceipt from "./components/SendReceipt.svelte";
  import HomeTransactions from "./components/HomeTransactions.svelte";
  // v0.2.0 wallet: the passphrase-first flow over every screen, the Home banner, Settings > Wallet
  import WalletGate from "./components/WalletGate.svelte";
  import WalletBanner from "./components/WalletBanner.svelte";
  import WalletSettings from "./components/WalletSettings.svelte";
  import { holdAddresses } from "./lib/walletSeed";
  import { checkForUpdate, node, update, versions, type Obliterated, type Removed } from "./lib/node";
  import { ECX_PROBLEM, ecxInput, fmtEcx, parseEcx } from "./lib/amount";
  import { nice } from "./lib/errors";
  import { cancelUnlock, submitUnlock, unlockRequest, walletLocked, withUnlock } from "./lib/wallet";
  import { clearReceipts, dismissReceipt, receipts, showReceipt } from "./lib/receipts";
  // v0.2.0 panels: the Security card and Home's red items, the Deposit panel, the Receive QR code
  import SecuritySettings from "./components/SecuritySettings.svelte";
  import HelpSettings from "./components/HelpSettings.svelte";
  import ReportDialog from "./components/ReportDialog.svelte";
  import { openReport, reportDraft } from "./lib/report";
  import SecurityAlerts from "./components/SecurityAlerts.svelte";
  import DepositPanel from "./components/DepositPanel.svelte";
  import QrCode from "./components/QrCode.svelte";
  import { runSecurityCheck } from "./lib/security";

  // Detect PWA/browser mode
  const isPWA = api.isPWA();
  let warningDismissed = false;

  // State
  let connected = false;
  let connecting = false;
  let balance = 0;
  let transactions: Transaction[] = [];
  let blockchainInfo: BlockchainInfo | null = null;
  let currentView: "node" | "home" | "notes" | "houses" | "pools" | "bills" | "send" | "receive" | "settings" = "home";
  let error = "";

  // Desktop: the first-run flow finds or installs the local node. "Connect by hand" is the
  // older remote-control screen (a node elsewhere, e.g. over Tailscale).
  let manualConnect = false;
  let localNode = false;
  // The gear works during setup too: Settings (and Obliterate) for someone stuck there.
  let setupSettings = false;
  $: onSetup = !connected && !isPWA && !manualConnect && !gone && !removed;

  async function onSetupReady() {
    localNode = true;
    connected = true;
    currentView = "node";
    await refresh();
    checkForUpdate();
  }

  // After "Remove FreeBank": say where the data is kept, then back to setup.
  let removed: Removed | null = null;
  function onRemoved(e: CustomEvent<Removed>) {
    removed = e.detail;
    connected = false;
    localNode = false;
    gates.set(null);
    clearReceipts();
    update.set(null);
    versions.update((v) => (v ? { ...v, node: null, commit: null } : v));
  }

  // After "Obliterate": say it's done and how to remove the app itself, then close.
  let gone: Obliterated | null = null;
  let closing = false;
  function onObliterated(e: CustomEvent<Obliterated>) {
    gone = e.detail;
    connected = false;
    localNode = false;
    gates.set(null);
    clearReceipts();
    update.set(null);
    versions.set(null);
  }
  async function closeApp() {
    closing = true;
    try {
      await node.quit();
    } catch {
      closing = false;
    }
  }

  const NEED_COINS = "You need FreeBank coins first: deposit ECX from BitWindow.";

  // Connection form (Model A: remote-control your own custodial node)
  let connMode: ConnMode = "local";
  let host = "127.0.0.1";
  let port = DEFAULT_PORT;
  let user = "";
  let password = "";
  $: preset = CONN_PRESETS.find((p) => p.mode === connMode) ?? CONN_PRESETS[0];

  function pickMode(mode: ConnMode) {
    connMode = mode;
    const p = CONN_PRESETS.find((x) => x.mode === mode);
    if (!p) return;
    // Prefill host only when the field is empty or still holding a preset default,
    // so a host the user typed is never clobbered by switching modes.
    if (!host || CONN_PRESETS.some((x) => x.defaultHost && x.defaultHost === host)) {
      host = p.defaultHost;
    }
  }

  function inferMode(h: string): ConnMode {
    if (h === "127.0.0.1" || h === "localhost") return "local";
    if (h.startsWith("100.")) return "tailscale";
    if (h.endsWith(".onion")) return "tor";
    return "custom";
  }

  // Receive
  let receiveAddress = "";
  let generatingAddress = false;
  // Addresses (Receive, Deposit) show only once the wallet has its passphrase: setting one replaces the
  // wallet's seed. v0.2.0 wallet: a new wallet's address screens wait until it is protected
  // ($holdAddresses, lib/walletSeed.ts); an older wallet's show, and its Home banner asks for the passphrase.
  $: canShowAddresses = !$holdAddresses;
  // The security checks run when the app connects, and again on each visit to Home.
  $: if (connected) runSecurityCheck();

  // Notes (M1)
  let notes: NoteHolding[] = [];
  let notesLoading = false;
  let mintHouseId = "";
  let mintUnits = "";
  let action: { type: "send" | "redeem" | "demand"; houseId: number } | null = null;
  let actionUnits = "";
  let actionAddress = "";
  let actionBusy = false;
  const STATUS_LABEL: Record<string, string> = {
    o: "Open", s: "Stressed", d: "Suspended", i: "Insolvent", w: "Wound down",
  };

  // An address in a receipt's headline; the full one goes in its "To" row.
  function short(a: string): string {
    return a.length > 20 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a;
  }

  // Everything is shown and entered in ECX (D-2026-09-29-5 and -7: no grams until gold is switched
  // on). Note units are base-native, 1 unit = 1 sat of ECX, so a note amount is an ECX amount, and
  // this shows "= 50,000,000 units" under an ECX field once it holds a valid amount.
  function unitsEcho(v: string | number): string {
    const u = parseEcx(v);
    return u === null ? "" : `= ${u.toLocaleString()} units`;
  }

  // Lists that need the wallet unlocked (listmynotes, listmylp, listmybills): with it locked, the screen says so and
  // offers Unlock (the usual passphrase prompt) instead of the node's "Please enter the wallet passphrase" (found on
  // Xvfb, 2026-10-01).
  let locked = { notes: false, lp: false, bills: false };
  async function unlockToRead<T>(read: () => Promise<T>, what: string): Promise<T | null> {
    try {
      return await withUnlock(read, { what, upfront: true });
    } catch (e) {
      error = nice(e);
      return null;
    }
  }

  async function loadNotes() {
    notesLoading = true;
    error = "";
    try {
      notes = await api.listMyNotes();
      locked.notes = false;
    } catch (e) {
      if (walletLocked(e)) locked.notes = true;
      else error = nice(e);
    }
    notesLoading = false;
  }

  async function unlockNotes() {
    const n = await unlockToRead(() => api.listMyNotes(), "see your notes");
    if (n) {
      notes = n;
      locked.notes = false;
    }
  }

  async function doMint() {
    if (!mintHouseId || !mintUnits) return;
    const units = parseEcx(mintUnits);
    if (units === null) { error = ECX_PROBLEM; return; }
    const houseId = parseInt(mintHouseId);
    const amount = `${fmtEcx(units)} ${BASE_TICKER}`;
    actionBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.mintNote(houseId, units), { what: `mint ${amount} of notes` });
      showReceipt({ txid, what: `Minted ${amount} of notes from House #${houseId}` });
      mintUnits = "";
      await loadNotes();
    } catch (e) {
      error = nice(e);
    }
    actionBusy = false;
  }

  function startAction(type: "send" | "redeem" | "demand", houseId: number) {
    action = { type, houseId };
    actionAddress = "";
    // A redeem or a demand takes one holder's coins summing exactly to the amount, so start from what can go:
    // everything redeemable now (while suspended, only the demanded notes), or everything not yet demanded.
    const n = notes.find((x) => x.house_id === houseId);
    const units = !n ? 0 : type === "redeem" ? n.redeemable_units ?? n.units : type === "demand" ? n.units - n.demanded_units : 0;
    actionUnits = units > 0 ? ecxInput(units) : "";
  }

  // The yearly rate on demands queued at a suspended house (node v0.2.18, defer_interest_bps: 1000 = 10%). It takes
  // the list, so the line re-renders when the houses arrive after the notes.
  function queueRate(list: House[], houseId: number): string {
    const bps = list.find((h) => h.id === houseId)?.defer_interest_bps;
    return typeof bps === "number" && bps > 0 ? `${bps / 100}% a year` : "interest";
  }

  async function submitAction() {
    if (!action || !actionUnits) return;
    const units = parseEcx(actionUnits);
    if (units === null) { error = ECX_PROBLEM; return; }
    const { type, houseId } = action;
    const amount = `${fmtEcx(units)} ${BASE_TICKER}`;
    actionBusy = true;
    error = "";
    try {
      if (type === "send") {
        const to = actionAddress.trim();
        if (!to) throw new Error("Recipient address required");
        const txid = await withUnlock(() => api.transferNote(houseId, units, to), { what: `send ${amount} of notes` });
        showReceipt({
          txid,
          what: `Sent ${amount} of House #${houseId} notes to ${short(to)}`,
          rows: [{ label: "To", value: to, mono: true }],
        });
      } else if (type === "redeem") {
        const txid = await withUnlock(() => api.redeemNote(houseId, units), { what: `redeem ${amount} of notes` });
        showReceipt({ txid, what: `Redeemed ${amount} of House #${houseId} notes` });
      } else {
        const txid = await withUnlock(() => api.demandNote(houseId, units), { what: `lodge a demand on ${amount} of notes` });
        showReceipt({ txid, what: `Lodged a demand on ${amount} of House #${houseId} notes` });
      }
      action = null;
      await loadNotes();
    } catch (e) {
      error = nice(e);
      // A redeem or a demand takes one holder's coins summing exactly to the amount (the node: "... sum exactly ...").
      if (type !== "send" && /sum exactly/.test(error))
        error += " Your notes of this house may sit at more than one of your addresses: send them to one of your own addresses first (Send, with an address from Receive), then try again.";
    }
    actionBusy = false;
  }

  // Houses (M2)
  let houses: House[] = [];
  let housesLoading = false;
  let regName = "";
  let regTier = "0";
  let regEscrow = "";
  let regBusy = false;

  function pct(bps: number): string {
    return (bps / 100).toFixed(1) + "%";
  }
  function util(h: House): number {
    return h.mintcapunits > 0 ? Math.min(100, (h.mintedunits / h.mintcapunits) * 100) : 0;
  }

  async function loadHouses() {
    housesLoading = true;
    error = "";
    try {
      houses = await api.listHouses();
    } catch (e) {
      error = nice(e);
    }
    housesLoading = false;
  }

  async function doRegister() {
    if (!regName || !regEscrow) return;
    const name = regName.trim();
    const escrow = parseFloat(regEscrow);
    regBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.registerHouse(name, parseInt(regTier), escrow), {
        what: "charter the house",
      });
      showReceipt({ txid, what: `Chartered the house "${name}" with ${escrow} ${BASE_TICKER} pledged` });
      regName = "";
      regEscrow = "";
      await loadHouses();
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  async function doAttest(houseId: number) {
    regBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.attestHouse(houseId), { what: `attest House #${houseId}'s reserves` });
      showReceipt({ txid, what: `Attested House #${houseId}'s reserves` });
      await loadHouses();
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  // Pools (M3): note ⇄ ECX AMM
  let pools: Pool[] = [];
  let myLp: LpHolding[] = [];
  let poolsLoading = false;
  let poolAction: { type: "swap" | "add" | "remove"; poolId: number } | null = null;
  let swapDir: "noteforbtx" | "btxfornote" = "noteforbtx";
  let poolAmountIn = "";
  let poolMinOut = "";
  let poolAddNoteEcx = "";
  let poolAddEcx = "";
  let poolRemoveLp = "";
  let poolBusy = false;
  // Create pool
  let createPoolId = "";
  let createNoteEcx = "";
  let createEcx = "";
  let createFeeBps = "30";

  function price(p: Pool): string {
    // The node leaves the spot price out while a pool side is empty. Both sides count in sats
    // (1 note unit = 1 sat), so sats per unit is ECX paid per ECX of notes.
    if (p.spot_price_sats_x1e8 == null) return "no price yet";
    return `${(p.spot_price_sats_x1e8 / 1e8).toFixed(6)} ${BASE_TICKER} per ${BASE_TICKER} of notes`;
  }

  // An empty or zero "least out" is no limit; anything else must be a valid amount (null if not).
  function minOut(v: string | number | null): number | null {
    return v === "" || v === null || Number(v) === 0 ? 0 : parseEcx(v);
  }

  async function loadPools() {
    poolsLoading = true;
    error = "";
    try {
      pools = await api.listPools();
      try {
        myLp = await api.listMyLp();
        locked.lp = false;
      } catch (e) {
        if (!walletLocked(e)) throw e;
        locked.lp = true;
      }
    } catch (e) {
      error = nice(e);
    }
    poolsLoading = false;
  }

  async function unlockLp() {
    const lp = await unlockToRead(() => api.listMyLp(), "see your liquidity");
    if (lp) {
      myLp = lp;
      locked.lp = false;
    }
  }

  function startPoolAction(type: "swap" | "add" | "remove", poolId: number) {
    poolAction = { type, poolId };
    swapDir = "noteforbtx";
    poolAmountIn = "";
    poolMinOut = "";
    poolAddNoteEcx = "";
    poolAddEcx = "";
    poolRemoveLp = "";
  }

  async function submitPoolAction() {
    if (!poolAction) return;
    const { type, poolId } = poolAction;
    poolBusy = true;
    error = "";
    try {
      if (type === "swap") {
        const dir = swapDir;
        const amountIn = parseEcx(poolAmountIn);
        const least = minOut(poolMinOut);
        if (amountIn === null || least === null) throw new Error(ECX_PROBLEM);
        const txid = await withUnlock(() => api.swapNote(poolId, dir, amountIn, least), { what: "swap in the pool" });
        const given = `${fmtEcx(amountIn)} ${BASE_TICKER}`;
        showReceipt({
          txid,
          what: dir === "noteforbtx"
            ? `Swapped ${given} of notes for ${BASE_TICKER} in Pool #${poolId}`
            : `Swapped ${given} for notes in Pool #${poolId}`,
        });
      } else if (type === "add") {
        const notes = parseEcx(poolAddNoteEcx);
        const ecx = parseEcx(poolAddEcx);
        if (notes === null || ecx === null) throw new Error(ECX_PROBLEM);
        const txid = await withUnlock(() => api.addLiquidity(poolId, notes, ecx), { what: "add liquidity" });
        showReceipt({
          txid,
          what: `Added ${fmtEcx(notes)} ${BASE_TICKER} of notes and ${fmtEcx(ecx)} ${BASE_TICKER} to Pool #${poolId}`,
        });
      } else {
        const lp = Number(poolRemoveLp);
        if (!Number.isInteger(lp) || lp < 1) throw new Error("Enter how many LP units to burn.");
        const txid = await withUnlock(() => api.removeLiquidity(poolId, lp), { what: "remove liquidity" });
        showReceipt({ txid, what: `Removed ${lp.toLocaleString()} LP units from Pool #${poolId}` });
      }
      poolAction = null;
      await loadPools();
    } catch (e) {
      error = nice(e);
    }
    poolBusy = false;
  }

  async function doCreatePool() {
    if (!createPoolId || !createNoteEcx || !createEcx) return;
    const notes = parseEcx(createNoteEcx);
    const ecx = parseEcx(createEcx);
    if (notes === null || ecx === null) { error = ECX_PROBLEM; return; }
    const poolId = parseInt(createPoolId);
    poolBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.createPool(poolId, notes, ecx, parseInt(createFeeBps)), {
        what: "create the pool",
      });
      showReceipt({
        txid,
        what: `Created Pool #${poolId} with ${fmtEcx(notes)} ${BASE_TICKER} of notes and ${fmtEcx(ecx)} ${BASE_TICKER}`,
      });
      createNoteEcx = "";
      createEcx = "";
      await loadPools();
    } catch (e) {
      error = nice(e);
    }
    poolBusy = false;
  }

  // Bills (M4): bills of exchange — the discount-house asset side
  let bills: Bill[] = [];
  let billsLoading = false;
  let billBusy = false;
  let billAction: { type: "endorse"; id: number } | null = null;
  let endorsePubkey = "";
  let newBillPubkey = "";
  let billBody = "";
  let billAmount = "";
  let billEscrow = "";
  let billMatureIn = "1000";
  let billGrace = "1008";
  const BILL_STATUS: Record<string, string> = {
    a: "Active", r: "Retired", d: "Defaulted", x: "Disputed",
  };
  function toHex(s: string): string {
    let h = "";
    for (let i = 0; i < s.length; i++) h += s.charCodeAt(i).toString(16).padStart(2, "0");
    return h;
  }

  async function loadBills() {
    billsLoading = true;
    error = "";
    try {
      bills = await api.listMyBills();
      locked.bills = false;
    } catch (e) {
      if (walletLocked(e)) locked.bills = true;
      else error = nice(e);
    }
    billsLoading = false;
  }

  async function unlockBills() {
    const b = await unlockToRead(() => api.listMyBills(), "see your bills");
    if (b) {
      bills = b;
      locked.bills = false;
    }
  }

  async function doIssueBill() {
    if (!billAmount || !billEscrow) return;
    billBusy = true;
    error = "";
    try {
      const now = blockchainInfo?.blocks ?? 0;
      const maturity = now + parseInt(billMatureIn || "1000");
      const bodyHex = toHex(billBody || "bill");
      const amount = parseFloat(billAmount);
      const escrow = parseFloat(billEscrow);
      const txid = await withUnlock(
        () => api.issueBill(bodyHex, amount, escrow, maturity, parseInt(billGrace || "1008")),
        { what: "issue the bill" },
      );
      showReceipt({
        txid,
        what: `Issued a bill for ${amount} ${BASE_TICKER}, bonded with ${escrow} ${BASE_TICKER}`,
        rows: [{ label: "Matures at", value: `block ${maturity.toLocaleString()}` }],
      });
      billBody = "";
      billAmount = "";
      billEscrow = "";
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function getBillPubkey() {
    error = "";
    try {
      newBillPubkey = await withUnlock(() => api.getNewBillPubkey(), { what: "make a new bill pubkey" });
    } catch (e) {
      error = nice(e);
    }
  }

  async function doEndorseBill() {
    if (!billAction || !endorsePubkey) return;
    billBusy = true;
    error = "";
    try {
      const id = billAction.id;
      const to = endorsePubkey.trim();
      const txid = await withUnlock(() => api.endorseBill(id, to), { what: `endorse Bill #${id}` });
      showReceipt({ txid, what: `Endorsed Bill #${id}` });
      billAction = null;
      endorsePubkey = "";
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function doRetireBill(id: number) {
    billBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.retireBill(id), { what: `retire Bill #${id}` });
      showReceipt({ txid, what: `Retired Bill #${id}` });
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function doClaimBillEscrow(id: number) {
    billBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.claimBillEscrow(id), { what: `claim Bill #${id}'s escrow` });
      showReceipt({ txid, what: `Claimed Bill #${id}'s escrow` });
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function connect() {
    connecting = true;
    error = "";
    try {
      connected = await api.connectNode({ host, port, user, password });
      if (connected) {
        await refresh();
        // The footer's node version, from the node's user agent ("/FreeBank:0.2.15/").
        const agent = await api.getNetworkInfo().then((n) => n.subversion).catch(() => "");
        const m = /FreeBank:([\d.]+)/.exec(agent);
        versions.update((v) => (v ? { ...v, node: m ? `v${m[1]}` : null, commit: null } : v));
      }
    } catch (e) {
      error = nice(e);
      connected = false;
    }
    connecting = false;
  }

  async function refresh() {
    if (!connected) return;
    try {
      [balance, transactions, blockchainInfo] = await Promise.all([
        api.getBalance(),
        api.getTransactions(20),
        api.getBlockchainInfo(),
      ]);
    } catch (e) {
      error = nice(e);
    }
  }

  async function generateAddress() {
    generatingAddress = true;
    try {
      receiveAddress = await withUnlock(() => api.getNewAddress(), { what: "make a new address" });
    } catch (e) {
      error = nice(e);
    }
    generatingAddress = false;
  }

  // The Mac's Help menu: "Report a Problem or Suggest Something…" (src-tauri/src/lib.rs).
  onMount(() => {
    if (isPWA) return;
    let stop: (() => void) | null = null;
    import("@tauri-apps/api/event")
      .then(({ listen }) => listen("report-open", () => openReport("problem")))
      .then((u) => (stop = u))
      .catch(() => {});
    return () => stop?.();
  });

  onMount(async () => {
    // Prefill from a previously saved connection, then infer the mode from the host.
    const saved = getSavedConfig();
    if (saved) {
      host = saved.host;
      port = saved.port;
      user = saved.user;
      password = saved.password;
      connMode = inferMode(saved.host);
    }
    // Check if already connected
    try {
      connected = await api.getConnectionStatus();
      if (connected) {
        await refresh();
      }
    } catch {
      connected = false;
    }
  });
</script>

<main>
  {#if !isPWA}<PhoneAlerts />{/if}
  {#if !isPWA}<QuitNotice />{/if}
  {#if !isPWA}<WalletGate {connected} {localNode} />{/if}
  {#if $unlockRequest}
    <UnlockPrompt
      what={$unlockRequest.what}
      error={$unlockRequest.error}
      busy={$unlockRequest.busy}
      on:submit={(e) => submitUnlock(e.detail)}
      on:cancel={cancelUnlock}
    />
  {/if}
  {#if isPWA && !warningDismissed}
    <div class="pwa-warning">
      <strong>Browser Mode</strong>
      <p>Running as PWA. RPC credentials are stored in browser localStorage — less secure than the desktop app. Use for small amounts only.</p>
      <button on:click={() => (warningDismissed = true)}>Dismiss</button>
    </div>
  {/if}

  <header>
    <h1><span class="brand-mark" aria-hidden="true">☉</span>{APP_NAME}</h1>
    {#if connected || onSetup}
      <div class="head-right">
        {#if blockchainInfo}
          <span class="network">{blockchainInfo.chain} · block {blockchainInfo.blocks.toLocaleString()}</span>
        {/if}
        <button
          class="gear"
          class:active={onSetup ? setupSettings : currentView === "settings"}
          title="Settings"
          aria-label="Settings"
          on:click={() => {
            if (onSetup) setupSettings = !setupSettings;
            else currentView = currentView === "settings" ? (localNode ? "node" : "home") : "settings";
          }}
        >
          <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
        </button>
      </div>
    {/if}
  </header>

  {#if error && !connected}
    <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
  {/if}

  {#if gone}
    <div class="card removed">
      <h2>{gone.app_removed ? "FreeBank is removed" : "Removed"}</h2>
      <p>
        Everything you ticked is gone{gone.at_exit.length ? ", except the app's window cache, which is removed when FreeBank closes" : ""}.
      </p>
      {#if gone.backups.length || gone.kept.length}
        <dl class="facts">
          {#each gone.backups as saved}<div><dt>Wallet backup</dt><dd class="mono"><PathText path={saved} /></dd></div>{/each}
          {#each gone.kept as kept}<div><dt>Left in place</dt><dd class="mono"><PathText path={kept} /></dd></div>{/each}
        </dl>
      {/if}
      {#if gone.app_removed}
        <p>FreeBank can't remove the app itself while it runs. Once it has closed:</p>
        <div class="gone-step">
          {#if gone.app.kind === "deb"}
            <p>Run <code>sudo apt remove freebank</code> in a terminal.</p>
          {:else if gone.app.kind === "appimage"}
            <p>Delete the AppImage file:</p>
            <p class="mono gone-path"><PathText path={gone.app.path ?? ""} /></p>
          {:else if gone.app.kind === "mac"}
            <p>Drag FreeBank from Applications to the Trash.</p>
          {:else}
            <p>Delete the FreeBank program{gone.app.path ? ":" : "."}</p>
            {#if gone.app.path}<p class="mono gone-path"><PathText path={gone.app.path} /></p>{/if}
          {/if}
        </div>
      {/if}
      <button class="wide" on:click={closeApp} disabled={closing}>{closing ? "Closing…" : "Close FreeBank"}</button>
    </div>
  {:else if removed}
    <div class="card removed">
      <h2>FreeBank's programs are removed</h2>
      <p>Your node's data folder and wallet are kept:</p>
      <dl class="facts">
        <div><dt>Data folder</dt><dd class="mono"><PathText path={removed.datadir} /></dd></div>
        {#each removed.wallets as wallet}<div><dt>Wallet</dt><dd class="mono"><PathText path={wallet} /></dd></div>{/each}
      </dl>
      <p class="hint">Install FreeBank again whenever you like, and it picks up the same wallet. The app itself stays installed; remove it like any other app if you wish.</p>
      <button class="wide" on:click={() => (removed = null)}>Done</button>
    </div>
  {:else if !connected && !isPWA && !manualConnect}
    <Setup
      bind:settingsOpen={setupSettings}
      on:ready={onSetupReady}
      on:manual={() => (manualConnect = true)}
      on:removed={onRemoved}
      on:obliterated={onObliterated}
    />
  {:else if !connected}
    <!-- Connection: Model A — remote-control your own custodial node -->
    {#if !isPWA}
      <button class="link-btn back-link" on:click={() => (manualConnect = false)}>← Set up FreeBank on this computer</button>
    {/if}
    <div class="card">
      <h2>Connect to your node</h2>

      <div class="custodial-note">
        {APP_NAME} is <strong>node-custodial</strong>: your keys live on your freebankd node.
        This app is a remote control — <strong>keys never travel</strong>. Connect to the node
        on this machine, or reach your own node from anywhere over Tailscale.
      </div>

      <div class="conn-modes" role="tablist">
        {#each CONN_PRESETS as p}
          <button
            role="tab"
            class:active={connMode === p.mode}
            aria-selected={connMode === p.mode}
            on:click={() => pickMode(p.mode)}
          >
            {p.label}
          </button>
        {/each}
      </div>

      <p class="mode-help">{preset.help}</p>
      {#if !preset.works}
        <p class="mode-warn">Not wired in this build yet — you can still enter the address, but the connection won't complete.</p>
      {/if}

      <div class="form">
        <label>
          Host
          <input type="text" bind:value={host} placeholder={preset.hostPlaceholder} />
        </label>
        <label>
          RPC port
          <input type="number" bind:value={port} placeholder={String(DEFAULT_PORT)} />
        </label>
        <label>
          RPC username
          <input type="text" bind:value={user} placeholder="rpcuser" autocomplete="off" />
        </label>
        <label>
          RPC password
          <input type="password" bind:value={password} placeholder="rpcpassword" autocomplete="off" />
        </label>
        <button on:click={connect} disabled={connecting}>
          {connecting ? "Connecting…" : "Connect"}
        </button>
      </div>
    </div>
  {:else}
    <!-- Navigation -->
    <nav>
      {#if localNode}
        <button class:active={currentView === "node"} on:click={() => (currentView = "node")}>
          Node
        </button>
      {/if}
      <button class:active={currentView === "home"} on:click={() => { currentView = "home"; error = ""; refresh(); }}>
        Home
      </button>
      <button class:active={currentView === "notes"} on:click={() => { currentView = "notes"; loadNotes(); loadHouses(); }}>
        Notes
      </button>
      <button class:active={currentView === "houses"} on:click={() => { currentView = "houses"; loadHouses(); }}>
        Houses
      </button>
      <button class:active={currentView === "pools"} on:click={() => { currentView = "pools"; loadPools(); loadHouses(); }}>
        Pools
      </button>
      <button class:active={currentView === "bills"} on:click={() => { currentView = "bills"; loadBills(); }}>
        Bills
      </button>
      <!-- Send shows "Available" and Max from the balance: fresh when it opens (found in the coin tests). -->
      <button class:active={currentView === "send"} on:click={() => { currentView = "send"; refresh(); }}>
        Send
      </button>
      <button class:active={currentView === "receive"} on:click={() => (currentView = "receive")}>
        Receive
      </button>
    </nav>

    {#if error}
      <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
    {/if}

    <!-- Receipts for what this session sent, newest first, on every tab until closed -->
    <!-- Older ones fold to their headline; each new confirmation refreshes the balance, the list and the header -->
    {#each $receipts as r, i (r.id)}
      <SendReceipt
        receiptId={r.id}
        txid={r.txid}
        what={r.what}
        sentAt={r.sentAt}
        rows={r.rows}
        collapsed={i > 0}
        on:close={() => dismissReceipt(r.id)}
        on:status={(e) => e.detail.confirmations > 0 && refresh()}
      />
    {/each}

    {#if currentView === "node"}
      <NodeStatus
        on:height={(e) => {
          if (blockchainInfo) blockchainInfo = { ...blockchainInfo, blocks: e.detail };
        }}
      />
    {:else if currentView === "home"}
      <WalletBanner />
      <!-- Home / Dashboard -->
      <SecurityAlerts on:open={() => (currentView = "settings")} />
      <BalanceCard {balance} onRefresh={refresh} />
      <DepositPanel {canShowAddresses} />

      <HomeTransactions {transactions} {balance} needCoins={NEED_COINS} on:changed={refresh} />
    {:else if currentView === "notes"}
      <!-- Notes (M1): per-house credit notes — hold / send / redeem / demand -->
      {#if locked.notes}
        <div class="card locked-read">
          <p>Your wallet is locked. Unlock it to see your notes.</p>
          <button on:click={unlockNotes}>Unlock</button>
        </div>
      {/if}
      <div class="card">
        <div class="notes-head">
          <h2>My Notes</h2>
          <button class="link-btn" on:click={loadNotes} disabled={notesLoading}>
            {notesLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if locked.notes}
          <p class="muted">Unlock your wallet (above) to see them.</p>
        {:else if notes.length === 0}
          <div class="empty">
            <p>You don't hold any notes yet.</p>
            {#if !housesLoading && houses.length === 0}
              <p class="muted small">Notes are issued by houses, and there are no houses on this network yet.</p>
            {:else}
              <p class="muted small">Anyone can send you a note at one of your addresses (see Receive), or you can buy one in a pool.</p>
            {/if}
            {#if balance === 0}<p class="muted small">{NEED_COINS}</p>{/if}
          </div>
        {:else}
          {#each notes as n}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">House #{n.house_id}</span>
                  <span class="badge badge-{n.house_status}">{STATUS_LABEL[n.house_status] ?? n.house_status}</span>
                </div>
                <div class="note-units">{fmtEcx(n.units)} {BASE_TICKER}</div>
              </div>
              <div class="hint">{n.units.toLocaleString()} units</div>
              {#if n.demanded_units > 0}
                <div class="note-demanded">
                  {#if n.house_status === "d"}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} in the house's payout queue, earning {queueRate(houses, n.house_id)}
                  {:else if n.house_status === "o" || n.house_status === "s"}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} demanded: the house must pay it within the demand window
                  {:else}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} demanded; the house has failed, so holders are paid from what it has left
                  {/if}
                </div>
              {/if}
              <div class="note-actions">
                <button on:click={() => startAction("send", n.house_id)}>Send</button>
                <button on:click={() => startAction("redeem", n.house_id)} disabled={!n.redeemable}>Redeem</button>
                <button on:click={() => startAction("demand", n.house_id)} disabled={!n.demandable}>Demand</button>
              </div>
              {#if action && action.houseId === n.house_id}
                <div class="note-form">
                  <label>
                    Amount ({BASE_TICKER})
                    <input type="number" bind:value={actionUnits} placeholder="0.00000000" step="0.00000001" min="0" />
                  </label>
                  {#if unitsEcho(actionUnits)}
                    <p class="hint">{unitsEcho(actionUnits)}</p>
                  {/if}
                  {#if action.type === "send"}
                    <label>
                      To address
                      <input type="text" bind:value={actionAddress} placeholder="X… (recipient)" />
                    </label>
                  {/if}
                  {#if action.type === "redeem"}
                    {#if n.house_status === "d"}
                      <p class="hint">The house is suspended: only notes under demand can be redeemed now, with their interest.</p>
                    {/if}
                    <p class="hint">Redemption is paid from the house's reserves — this succeeds when your node controls the house.</p>
                  {/if}
                  {#if action.type === "demand"}
                    {#if n.house_status === "d"}
                      <p class="hint">Your notes join the house's payout queue and earn {queueRate(houses, n.house_id)} from today. The house can pay you at any time; until it does, these notes can't be sent.</p>
                    {:else}
                      <p class="hint">A formal demand: the house must pay you in full within the demand window. Until it pays, these notes can't be sent.</p>
                    {/if}
                  {/if}
                  <div class="note-form-actions">
                    <button on:click={submitAction} disabled={actionBusy || !actionUnits}>
                      {actionBusy ? "…" : action.type === "send" ? "Send note" : action.type === "redeem" ? "Redeem" : "Lodge demand"}
                    </button>
                    <button class="secondary" on:click={() => (action = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      {#if houses.length > 0}
      <div class="card">
        <h3>Mint notes</h3>
        <p class="muted">Issue new notes from a house your node controls. Enter the amount in {BASE_TICKER}: notes are base-native, 1 unit = 1 sat.</p>
        <div class="form">
          <label>
            House ID
            <input type="number" bind:value={mintHouseId} placeholder="e.g. 1" />
          </label>
          <label>
            Amount ({BASE_TICKER})
            <input type="number" bind:value={mintUnits} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          {#if unitsEcho(mintUnits)}
            <p class="hint">{unitsEcho(mintUnits)}</p>
          {/if}
          <button on:click={doMint} disabled={actionBusy || !mintHouseId || !mintUnits}>
            {actionBusy ? "…" : "Mint"}
          </button>
        </div>
      </div>
      {/if}
    {:else if currentView === "houses"}
      <!-- Houses (M2): the directory of competing note-issuers -->
      <div class="card">
        <div class="notes-head">
          <h2>Houses</h2>
          <button class="link-btn" on:click={loadHouses} disabled={housesLoading}>
            {housesLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">Every note-issuing house on the chain. A note is only as sound as the house behind it — check its status and reserves before you trust its notes.</p>
        {#if houses.length === 0}
          <div class="empty">
            <p>No houses on this network yet.</p>
            <p class="muted small">
              {balance > 0 ? "You can charter the first one below." : `Chartering one takes a reserve in ECX. ${NEED_COINS}`}
            </p>
          </div>
        {:else}
          {#each houses as h}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">#{h.id} · {h.classid}</span>
                  <span class="badge badge-{h.effective_status.charAt(0)}">{h.effective_status}</span>
                </div>
                <div class="note-units">tier {h.tier} · λ{(h.lambdax10 / 10).toFixed(1)}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Reserve pledged</span> {h.activeescrow} {BASE_TICKER}</div>
                <div><span class="stat-label">Notes outstanding</span> {fmtEcx(h.mintedunits)} of {fmtEcx(h.mintcapunits)} {BASE_TICKER} cap</div>
                {#if h.mintedunits > 0}
                  <div><span class="stat-label">Attested ratio</span> {pct(h.attestedratiobps)}</div>
                {/if}
                <div><span class="stat-label">Last attested</span> {h.lastattestheight > 0 ? `block ${h.lastattestheight} · ${h.lastattestreserves} ECX` : "never"}</div>
              </div>
              {#if h.mintcapunits > 0}
                <div class="util-bar"><div class="util-fill" style="width:{util(h)}%"></div></div>
              {/if}
              <div class="note-actions">
                <button on:click={() => doAttest(h.id)} disabled={regBusy}>Attest reserves</button>
              </div>
            </div>
          {/each}
        {/if}
      </div>

      <div class="card">
        <h3>Charter a house</h3>
        <p class="muted">Open your own note-issuing house — the Scottish move: anyone can start a bank, kept honest by convertibility. Your node holds the keys.</p>
        <div class="form">
          <label>
            Name (note-class id)
            <input type="text" bind:value={regName} placeholder="e.g. clyde — a–z 0–9, ≤16 chars" />
          </label>
          <label>
            Liability tier (0–3; higher tier = more leverage)
            <input type="number" bind:value={regTier} min="0" max="3" />
          </label>
          <label>
            Pledged reserve (ECX)
            <input type="number" bind:value={regEscrow} placeholder="e.g. 1.0" step="0.00000001" />
          </label>
          <button on:click={doRegister} disabled={regBusy || !regName || !regEscrow || balance === 0}>
            {regBusy ? "…" : "Charter house"}
          </button>
          {#if balance === 0}<p class="blocked-why">{NEED_COINS}</p>{/if}
        </div>
      </div>
    {:else if currentView === "pools"}
      <!-- Pools (M3): note ⇄ ECX constant-product AMM -->
      {#if locked.lp}
        <div class="card locked-read">
          <p>Your wallet is locked. Unlock it to see your liquidity in the pools.</p>
          <button on:click={unlockLp}>Unlock</button>
        </div>
      {/if}
      {#if pools.length > 0}
      <div class="card">
        <div class="notes-head">
          <h2>My liquidity</h2>
          <button class="link-btn" on:click={loadPools} disabled={poolsLoading}>
            {poolsLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if locked.lp}
          <p class="muted">Unlock your wallet (above) to see it.</p>
        {:else if myLp.length === 0}
          <p class="muted">You haven't added to any pool. Adding to one below earns a share of its swap fees.</p>
        {:else}
          {#each myLp as lp}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Pool #{lp.pool_id}</span>
                  <span class="badge badge-o">{pct(lp.share_bps)}</span>
                </div>
                <div class="note-units">{lp.lp_units.toLocaleString()} LP · {lp.fee_bps} bps</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">My share</span> {pct(lp.share_bps)} of {lp.lp_supply.toLocaleString()} LP</div>
                <div><span class="stat-label">Underlying notes</span> {fmtEcx(lp.my_note_units)} {BASE_TICKER}</div>
                <div><span class="stat-label">Underlying {BASE_TICKER}</span> {fmtEcx(lp.my_btx_sats)} {BASE_TICKER}</div>
              </div>
            </div>
          {/each}
        {/if}
      </div>

      {/if}

      <div class="card">
        <div class="notes-head">
          <h2>Pools</h2>
          <button class="link-btn" on:click={loadPools} disabled={poolsLoading}>
            {poolsLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">Constant-product pools trade a house's notes against base {BASE_TICKER}. Swap across a pool, or add/remove liquidity to earn fees.</p>
        {#if pools.length === 0}
          <div class="empty">
            <p>No pools on this network yet.</p>
            <p class="muted small">
              {houses.length === 0
                ? "Each pool trades one house's notes, so pools come after houses, and there are no houses yet."
                : "A house's note holders can seed the first one below."}
            </p>
          </div>
        {:else}
          {#each pools as p}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Pool #{p.pool_id}</span>
                  <span class="badge badge-o">{p.fee_bps} bps</span>
                </div>
                <div class="note-units">{price(p)}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Note reserve</span> {fmtEcx(p.note_reserve)} {BASE_TICKER}</div>
                <div><span class="stat-label">{BASE_TICKER} reserve</span> {fmtEcx(p.btx_reserve)} {BASE_TICKER}</div>
                <div><span class="stat-label">LP supply</span> {p.lp_supply.toLocaleString()}</div>
              </div>
              <div class="note-actions">
                <button on:click={() => startPoolAction("swap", p.pool_id)}>Swap</button>
                <button on:click={() => startPoolAction("add", p.pool_id)}>Add</button>
                <button on:click={() => startPoolAction("remove", p.pool_id)}>Remove</button>
              </div>
              {#if poolAction && poolAction.poolId === p.pool_id}
                <div class="note-form">
                  {#if poolAction.type === "swap"}
                    <div class="conn-modes">
                      <button
                        class:active={swapDir === "noteforbtx"}
                        on:click={() => (swapDir = "noteforbtx")}
                      >Note → {BASE_TICKER}</button>
                      <button
                        class:active={swapDir === "btxfornote"}
                        on:click={() => (swapDir = "btxfornote")}
                      >{BASE_TICKER} → Note</button>
                    </div>
                    <label>
                      {swapDir === "noteforbtx" ? `Notes in (${BASE_TICKER})` : `${BASE_TICKER} in`}
                      <input type="number" bind:value={poolAmountIn} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <label>
                      {swapDir === "noteforbtx" ? `Least ${BASE_TICKER} out` : `Least notes out (${BASE_TICKER})`}
                      <input type="number" bind:value={poolMinOut} placeholder="empty or 0 = no slippage limit" step="0.00000001" min="0" />
                    </label>
                  {:else if poolAction.type === "add"}
                    <label>
                      Notes ({BASE_TICKER})
                      <input type="number" bind:value={poolAddNoteEcx} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <label>
                      {BASE_TICKER}
                      <input type="number" bind:value={poolAddEcx} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <p class="hint">Liquidity is deposited pro-rata to the pool's current ratio; excess is refunded.</p>
                  {:else}
                    <label>
                      LP units to burn
                      <input type="number" bind:value={poolRemoveLp} placeholder="LP units" />
                    </label>
                    <p class="hint">Burns your LP units and returns the underlying notes + {BASE_TICKER} at the current ratio.</p>
                  {/if}
                  <div class="note-form-actions">
                    <button on:click={submitPoolAction} disabled={poolBusy}>
                      {poolBusy ? "…" : poolAction.type === "swap" ? "Swap" : poolAction.type === "add" ? "Add liquidity" : "Remove liquidity"}
                    </button>
                    <button class="secondary" on:click={() => (poolAction = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      {#if houses.length > 0}
      <div class="card">
        <h3>Create pool</h3>
        <p class="muted">Seed a new note/{BASE_TICKER} pool. The pool id is the house id whose notes it trades. You supply both sides of the initial reserves.</p>
        <div class="form">
          <label>
            Pool id (house id)
            <input type="number" bind:value={createPoolId} placeholder="e.g. 1" />
          </label>
          <label>
            Seed notes ({BASE_TICKER})
            <input type="number" bind:value={createNoteEcx} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          <label>
            Seed {BASE_TICKER}
            <input type="number" bind:value={createEcx} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          <label>
            Fee (bps)
            <input type="number" bind:value={createFeeBps} placeholder="e.g. 30" />
          </label>
          <button on:click={doCreatePool} disabled={poolBusy || !createPoolId || !createNoteEcx || !createEcx}>
            {poolBusy ? "…" : "Create pool"}
          </button>
        </div>
      </div>
      {/if}
    {:else if currentView === "bills"}
      <!-- Bills (M4): bills of exchange — the discount-house asset side -->
      {#if locked.bills}
        <div class="card locked-read">
          <p>Your wallet is locked. Unlock it to see your bills.</p>
          <button on:click={unlockBills}>Unlock</button>
        </div>
      {/if}
      <div class="card">
        <div class="notes-head">
          <h2>My Bills</h2>
          <button class="link-btn" on:click={loadBills} disabled={billsLoading}>
            {billsLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">Bills of exchange you hold, drew, or accepted — a discount house's asset side: dated credit, backed by an escrow bond, that settles at par.</p>
        {#if locked.bills}
          <p class="muted">Unlock your wallet (above) to see them.</p>
        {:else if bills.length === 0}
          <div class="empty">
            <p>You have no bills yet.</p>
            <p class="muted small">
              {balance > 0
                ? "Issue one below, or share a bill pubkey so someone can endorse a bill to you."
                : "Someone can endorse a bill to you: share a bill pubkey below."}
            </p>
          </div>
        {:else}
          {#each bills as b}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Bill #{b.id}</span>
                  <span class="badge badge-bill-{b.status}">{BILL_STATUS[b.status] ?? b.status}</span>
                </div>
                <div class="note-units">{b.amount} {BASE_TICKER}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Escrow bond</span> {b.escrow} {BASE_TICKER}</div>
                <div><span class="stat-label">Matures at</span> block {b.maturity_height} (+{b.grace_blocks} grace)</div>
                {#if b.roles && b.roles.length}
                  <div><span class="stat-label">Your role</span> {b.roles.join(", ")}</div>
                {/if}
              </div>
              <div class="note-actions">
                <button on:click={() => { billAction = { type: "endorse", id: b.id }; endorsePubkey = ""; }} disabled={b.status !== "a"}>Endorse</button>
                <button on:click={() => doRetireBill(b.id)} disabled={billBusy || b.status !== "a"}>Retire</button>
                <button on:click={() => doClaimBillEscrow(b.id)} disabled={billBusy || b.status !== "d"}>Claim escrow</button>
              </div>
              {#if billAction && billAction.id === b.id}
                <div class="note-form">
                  <label>
                    Endorse to bill pubkey
                    <input type="text" bind:value={endorsePubkey} placeholder="02… (recipient's bill pubkey)" />
                  </label>
                  <div class="note-form-actions">
                    <button on:click={doEndorseBill} disabled={billBusy || !endorsePubkey}>
                      {billBusy ? "…" : "Endorse bill"}
                    </button>
                    <button class="secondary" on:click={() => (billAction = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      <div class="card">
        <h3>Receive a bill</h3>
        <p class="muted">Share a fresh bill pubkey so someone can endorse a bill to you.</p>
        {#if newBillPubkey}
          <div class="address-display">
            <code>{newBillPubkey}</code>
            <button on:click={() => navigator.clipboard.writeText(newBillPubkey)}>Copy</button>
          </div>
        {/if}
        <button on:click={getBillPubkey}>New bill pubkey</button>
      </div>

      <div class="card">
        <h3>Issue a bill</h3>
        <p class="muted">Draw and accept a bill: a dated promise for a face amount, backed by an escrow bond the holder can claim if it defaults.</p>
        <div class="form">
          <label>
            Description
            <input type="text" bind:value={billBody} placeholder="e.g. 90-day trade bill" />
          </label>
          <label>
            Face amount ({BASE_TICKER})
            <input type="number" bind:value={billAmount} placeholder="e.g. 1.0" step="0.00000001" />
          </label>
          <label>
            Escrow bond ({BASE_TICKER})
            <input type="number" bind:value={billEscrow} placeholder="e.g. 0.1" step="0.00000001" />
          </label>
          <label>
            Matures in (blocks from now)
            <input type="number" bind:value={billMatureIn} placeholder="1000" />
          </label>
          <label>
            Grace blocks
            <input type="number" bind:value={billGrace} placeholder="1008" />
          </label>
          <button on:click={doIssueBill} disabled={billBusy || !billAmount || !billEscrow || balance === 0}>
            {billBusy ? "…" : "Issue bill"}
          </button>
          {#if balance === 0}<p class="blocked-why">Issuing a bill takes an escrow bond in ECX. {NEED_COINS}</p>{/if}
        </div>
      </div>
    {:else if currentView === "send"}
      <!-- Send: Max, a speed, the fee shown before Confirm; the receipt has Speed up -->
      <SendPanel {balance} on:sent={() => { refresh(); currentView = "home"; }} />
    {:else if currentView === "receive"}
      <!-- Receive -->
      <div class="card">
        <h2>Receive {BASE_TICKER}</h2>
        {#if !canShowAddresses}
          <p class="hint">Your addresses show here once your wallet has a passphrase.</p>
        {:else}
        {#if receiveAddress}
          <div class="address-display">
            <code>{receiveAddress}</code>
            <button on:click={() => navigator.clipboard.writeText(receiveAddress)}>
              Copy
            </button>
          </div>
          <div class="qr-placeholder">
            <QrCode text={receiveAddress} size={200} />
          </div>
        {/if}
        <button on:click={generateAddress} disabled={generatingAddress}>
          {generatingAddress ? "Generating…" : "New Address"}
        </button>
        {/if}
      </div>
    {:else if currentView === "settings"}
      <!-- Settings -->
      {#if localNode}
        <WalletSettings />
        <NodeSettings on:removed={onRemoved} on:obliterated={onObliterated} />
        <PhoneSettings />
      {:else}
        <div class="card">
          <h2>Settings</h2>
          <p>Connected to: {host}:{port}</p>
          {#if blockchainInfo}
            <p>Chain: {blockchainInfo.chain}</p>
            <p>Blocks: {blockchainInfo.blocks}</p>
            <p>Difficulty: {blockchainInfo.difficulty.toExponential(2)}</p>
          {/if}
          <button on:click={() => { connected = false; localNode = false; gates.set(null); clearReceipts(); }}>
            Disconnect
          </button>
        </div>
        {#if !isPWA}<PhoneSettings />{/if}
      {/if}
      {#if !isPWA}<SecuritySettings /><HelpSettings />{/if}
    {/if}
  {/if}

  {#if $reportDraft}<ReportDialog />{/if}

  {#if $versions}
    <footer class="app-foot">
      FreeBank app {$versions.app}{$versions.node ? ` · node ${$versions.node}` : ""}{#if $update?.available}<span class="avail">{$update.latest} available</span>{/if}
    </footer>
  {/if}
</main>

<style>
  /* Styles are in styles/app.css */
  .locked-read {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .locked-read p {
    margin: 0;
  }
</style>
