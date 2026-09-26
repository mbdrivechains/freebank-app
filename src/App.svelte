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
  import TransactionItem from "./components/TransactionItem.svelte";
  import Setup from "./components/Setup.svelte";
  import NodeStatus from "./components/NodeStatus.svelte";
  import NodeSettings from "./components/NodeSettings.svelte";
  import { checkForUpdate, update, versions, type Removed } from "./lib/node";

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
    update.set(null);
    versions.update((v) => (v ? { ...v, node: null, commit: null } : v));
  }

  // Errors in plain words: a node that is starting or stopped isn't a failure of this screen.
  function nice(e: unknown): string {
    const m = String(e).replace(/^Error: /, "");
    if (/Loading|Verifying|Rewinding|warm|still starting|-28/i.test(m)) {
      return "Your node is still starting. This screen fills in once it's ready.";
    }
    if (/Request failed|error sending request|Connection refused|RPC not configured|busy; try again/i.test(m)) {
      return "Your FreeBank node isn't answering. It may be stopped or still starting; the Node tab shows which.";
    }
    return m.replace(/^RPC error: /, "");
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

  // Send form
  let sendAddress = "";
  let sendAmount = "";
  let sending = false;

  // Receive
  let receiveAddress = "";
  let generatingAddress = false;

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

  function fmtUnits(u: number): string {
    return `${u.toLocaleString()} units · ${(u / 1e8).toFixed(8)} ${BASE_TICKER}`;
  }

  // Launch-scale gram rate (display-only). 0 = unknown; sats_per_gram, so grams = sats / gramRate.
  let gramRate = 0;

  // Fetch the gram rate once after connect (hoisted out of loadNotes so every tab has it).
  async function loadGramRate() {
    try {
      const r = await api.getGramRate();
      gramRate = r.sats_per_gram || 0;
    } catch {
      gramRate = 0;
    }
  }

  // Shared display helper: sats → "☉ N g" caption (empty when rate unknown). ECX → sats = ecx * 1e8.
  function fmtG(sats: number): string {
    return gramRate > 0 ? '☉ ' + (sats / gramRate).toFixed(6) + ' g' : '';
  }

  // Client-side gram → integer note units (units are sats). Guarded by callers for gramRate > 0.
  function gramsToUnits(grams: number): number {
    return Math.round(grams * gramRate);
  }

  // Gram-input toggles default to GRAMS once the rate is known (applied once, never clobbers the user).
  let mintInGrams = false;
  let actionInGrams = false;
  let gramDefaultApplied = false;
  $: if (gramRate > 0 && !gramDefaultApplied) {
    mintInGrams = true;
    actionInGrams = true;
    gramDefaultApplied = true;
  }

  async function loadNotes() {
    notesLoading = true;
    error = "";
    try {
      notes = await api.listMyNotes();
    } catch (e) {
      error = nice(e);
    }
    notesLoading = false;
  }

  async function doMint() {
    if (!mintHouseId || !mintUnits) return;
    let units: number;
    if (mintInGrams) {
      if (!(gramRate > 0)) { error = "No gram rate — switch to units"; return; }
      units = gramsToUnits(parseFloat(mintUnits));
    } else {
      units = parseInt(mintUnits);
    }
    if (!Number.isInteger(units) || units < 1) { error = "Amount must be at least 1 unit"; return; }
    actionBusy = true;
    error = "";
    try {
      const txid = await api.mintNote(parseInt(mintHouseId), units);
      alert(`Minted.\n\nTXID: ${txid}`);
      mintUnits = "";
      await loadNotes();
    } catch (e) {
      error = nice(e);
    }
    actionBusy = false;
  }

  function startAction(type: "send" | "redeem" | "demand", houseId: number) {
    action = { type, houseId };
    actionUnits = "";
    actionAddress = "";
  }

  async function submitAction() {
    if (!action || !actionUnits) return;
    let units: number;
    if (actionInGrams) {
      if (!(gramRate > 0)) { error = "No gram rate — switch to units"; return; }
      units = gramsToUnits(parseFloat(actionUnits));
    } else {
      units = parseInt(actionUnits);
    }
    if (!Number.isInteger(units) || units < 1) { error = "Amount must be at least 1 unit"; return; }
    actionBusy = true;
    error = "";
    try {
      let txid = "";
      if (action.type === "send") {
        if (!actionAddress) throw new Error("Recipient address required");
        txid = await api.transferNote(action.houseId, units, actionAddress);
      } else if (action.type === "redeem") {
        txid = await api.redeemNote(action.houseId, units);
      } else {
        txid = await api.demandNote(action.houseId, units);
      }
      alert(`${action.type} submitted.\n\nTXID: ${txid}`);
      action = null;
      await loadNotes();
    } catch (e) {
      error = nice(e);
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
    regBusy = true;
    error = "";
    try {
      const txid = await api.registerHouse(regName.trim(), parseInt(regTier), parseFloat(regEscrow));
      alert(`House chartered.\n\nTXID: ${txid}`);
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
      const txid = await api.attestHouse(houseId);
      alert(`Reserves attested.\n\nTXID: ${txid}`);
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
  let poolAddNote = "";
  let poolAddBtx = "";
  let poolRemoveLp = "";
  let poolBusy = false;
  // Create pool
  let createPoolId = "";
  let createNoteUnits = "";
  let createBtxSats = "";
  let createFeeBps = "30";

  function price(p: Pool): string {
    // The node leaves the spot price out while a pool side is empty.
    if (p.spot_price_sats_x1e8 == null) return "no price yet";
    return `${(p.spot_price_sats_x1e8 / 1e8).toFixed(8)} sats/unit`;
  }

  async function loadPools() {
    poolsLoading = true;
    error = "";
    try {
      [pools, myLp] = await Promise.all([api.listPools(), api.listMyLp()]);
    } catch (e) {
      error = nice(e);
    }
    poolsLoading = false;
  }

  function startPoolAction(type: "swap" | "add" | "remove", poolId: number) {
    poolAction = { type, poolId };
    swapDir = "noteforbtx";
    poolAmountIn = "";
    poolMinOut = "";
    poolAddNote = "";
    poolAddBtx = "";
    poolRemoveLp = "";
  }

  async function submitPoolAction() {
    if (!poolAction) return;
    poolBusy = true;
    error = "";
    try {
      let txid = "";
      if (poolAction.type === "swap") {
        if (!poolAmountIn) throw new Error("Amount in required");
        txid = await api.swapNote(
          poolAction.poolId,
          swapDir,
          parseInt(poolAmountIn),
          poolMinOut ? parseInt(poolMinOut) : 0,
        );
      } else if (poolAction.type === "add") {
        if (!poolAddNote || !poolAddBtx) throw new Error("Note units and ECX sats required");
        txid = await api.addLiquidity(poolAction.poolId, parseInt(poolAddNote), parseInt(poolAddBtx));
      } else {
        if (!poolRemoveLp) throw new Error("LP units required");
        txid = await api.removeLiquidity(poolAction.poolId, parseInt(poolRemoveLp));
      }
      alert(`${poolAction.type} submitted.\n\nTXID: ${txid}`);
      poolAction = null;
      await loadPools();
    } catch (e) {
      error = nice(e);
    }
    poolBusy = false;
  }

  async function doCreatePool() {
    if (!createPoolId || !createNoteUnits || !createBtxSats) return;
    poolBusy = true;
    error = "";
    try {
      const txid = await api.createPool(
        parseInt(createPoolId),
        parseInt(createNoteUnits),
        parseInt(createBtxSats),
        parseInt(createFeeBps),
      );
      alert(`Pool created.\n\nTXID: ${txid}`);
      createNoteUnits = "";
      createBtxSats = "";
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
    } catch (e) {
      error = nice(e);
    }
    billsLoading = false;
  }

  async function doIssueBill() {
    if (!billAmount || !billEscrow) return;
    billBusy = true;
    error = "";
    try {
      const now = blockchainInfo?.blocks ?? 0;
      const maturity = now + parseInt(billMatureIn || "1000");
      const bodyHex = toHex(billBody || "bill");
      const txid = await api.issueBill(
        bodyHex,
        parseFloat(billAmount),
        parseFloat(billEscrow),
        maturity,
        parseInt(billGrace || "1008"),
      );
      alert(`Bill issued.\n\nTXID: ${txid}`);
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
      newBillPubkey = await api.getNewBillPubkey();
    } catch (e) {
      error = nice(e);
    }
  }

  async function doEndorseBill() {
    if (!billAction || !endorsePubkey) return;
    billBusy = true;
    error = "";
    try {
      const txid = await api.endorseBill(billAction.id, endorsePubkey.trim());
      alert(`Bill endorsed.\n\nTXID: ${txid}`);
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
      const txid = await api.retireBill(id);
      alert(`Bill retired.\n\nTXID: ${txid}`);
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
      const txid = await api.claimBillEscrow(id);
      alert(`Escrow claimed.\n\nTXID: ${txid}`);
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
      // Fetch the gram rate once here so every tab (Home/Houses/Pools/Bills/Notes) can display grams.
      await loadGramRate();
    } catch (e) {
      error = nice(e);
    }
  }

  async function generateAddress() {
    generatingAddress = true;
    try {
      receiveAddress = await api.getNewAddress();
    } catch (e) {
      error = nice(e);
    }
    generatingAddress = false;
  }

  async function send() {
    if (!sendAddress || !sendAmount) return;
    sending = true;
    error = "";
    try {
      const txid = await api.sendTransaction(sendAddress, parseFloat(sendAmount));
      alert(`Transaction sent!\n\nTXID: ${txid}`);
      sendAddress = "";
      sendAmount = "";
      await refresh();
      currentView = "home";
    } catch (e) {
      error = nice(e);
    }
    sending = false;
  }

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
  {#if isPWA && !warningDismissed}
    <div class="pwa-warning">
      <strong>Browser Mode</strong>
      <p>Running as PWA. RPC credentials are stored in browser localStorage — less secure than the desktop app. Use for small amounts only.</p>
      <button on:click={() => (warningDismissed = true)}>Dismiss</button>
    </div>
  {/if}

  <header>
    <h1><span class="brand-mark" aria-hidden="true">☉</span>{APP_NAME}</h1>
    {#if connected}
      <div class="head-right">
        {#if blockchainInfo}
          <span class="network">{blockchainInfo.chain} · block {blockchainInfo.blocks.toLocaleString()}</span>
        {/if}
        <button
          class="gear"
          class:active={currentView === "settings"}
          title="Settings"
          aria-label="Settings"
          on:click={() => (currentView = currentView === "settings" ? (localNode ? "node" : "home") : "settings")}
        >
          <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
        </button>
      </div>
    {/if}
  </header>

  {#if error && !connected}
    <div class="soft-error banner">
      <span>{error}</span>
      <button class="ghost dismiss" on:click={() => (error = "")} aria-label="Dismiss">×</button>
    </div>
  {/if}

  {#if removed}
    <div class="card removed">
      <h2>FreeBank's programs are removed</h2>
      <p>Your node's data folder and wallet are kept:</p>
      <dl class="facts">
        <div><dt>Data folder</dt><dd class="mono">{removed.datadir}</dd></div>
        {#if removed.wallet}<div><dt>Wallet</dt><dd class="mono">{removed.wallet}</dd></div>{/if}
      </dl>
      <p class="hint">Install FreeBank again whenever you like, and it picks up the same wallet. The app itself stays installed; remove it like any other app if you wish.</p>
      <button class="wide" on:click={() => (removed = null)}>Done</button>
    </div>
  {:else if !connected && !isPWA && !manualConnect}
    <Setup on:ready={onSetupReady} on:manual={() => (manualConnect = true)} />
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
      <button class:active={currentView === "send"} on:click={() => (currentView = "send")}>
        Send
      </button>
      <button class:active={currentView === "receive"} on:click={() => (currentView = "receive")}>
        Receive
      </button>
    </nav>

    {#if error}
      <div class="soft-error banner">
        <span>{error}</span>
        <button class="ghost dismiss" on:click={() => (error = "")} aria-label="Dismiss">×</button>
      </div>
    {/if}

    {#if currentView === "node"}
      <NodeStatus on:height={(e) => { if (blockchainInfo) blockchainInfo = { ...blockchainInfo, blocks: e.detail }; }} />
    {:else if currentView === "home"}
      <!-- Home / Dashboard -->
      <BalanceCard {balance} {gramRate} onRefresh={refresh} />

      <div class="card">
        <h3>Recent Transactions</h3>
        {#if transactions.length === 0}
          <p class="muted">No transactions yet.</p>
          {#if balance === 0}<p class="hint">{NEED_COINS}</p>{/if}
        {:else}
          <div class="tx-list">
            {#each transactions.slice(0, 10) as tx}
              <TransactionItem {tx} />
            {/each}
          </div>
        {/if}
      </div>
    {:else if currentView === "notes"}
      <!-- Notes (M1): per-house credit notes — hold / send / redeem / demand -->
      <div class="card">
        <div class="notes-head">
          <h2>My Notes</h2>
          <button class="link-btn" on:click={loadNotes} disabled={notesLoading}>
            {notesLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if notes.length === 0}
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
                <div class="note-units">{fmtUnits(n.units)}</div>
              </div>
              {#if gramRate > 0}
                <div class="hint">☉ {(n.units / gramRate).toFixed(6)} g <span class="muted">launch scale — not enforced</span></div>
              {/if}
              {#if n.demanded_units > 0}
                <div class="note-demanded">{n.demanded_units.toLocaleString()} units under demand (accruing interest)</div>
              {/if}
              <div class="note-actions">
                <button on:click={() => startAction("send", n.house_id)}>Send</button>
                <button on:click={() => startAction("redeem", n.house_id)} disabled={!n.redeemable}>Redeem</button>
                <button on:click={() => startAction("demand", n.house_id)} disabled={!n.demandable}>Demand</button>
              </div>
              {#if action && action.houseId === n.house_id}
                <div class="note-form">
                  <div class="conn-modes">
                    <button type="button" class:active={actionInGrams} on:click={() => (actionInGrams = true)} disabled={!(gramRate > 0)}>Grams</button>
                    <button type="button" class:active={!actionInGrams} on:click={() => (actionInGrams = false)}>Units</button>
                  </div>
                  <label>
                    {actionInGrams ? "Amount (grams ☉)" : "Units"}
                    <input type="number" bind:value={actionUnits} placeholder={actionInGrams ? "grams" : "units"} step={actionInGrams ? "0.000001" : "1"} />
                  </label>
                  {#if actionInGrams && gramRate > 0 && actionUnits && !isNaN(parseFloat(actionUnits))}
                    <p class="hint">= {gramsToUnits(parseFloat(actionUnits)).toLocaleString()} units</p>
                  {/if}
                  {#if action.type === "send"}
                    <label>
                      To address
                      <input type="text" bind:value={actionAddress} placeholder="X… (recipient)" />
                    </label>
                  {/if}
                  {#if action.type === "redeem"}
                    <p class="hint">Redemption is paid from the house's reserves — this succeeds when your node controls the house.</p>
                  {/if}
                  {#if action.type === "demand"}
                    <p class="hint">Lodges a demand under the option clause; your notes stay yours and start accruing interest.</p>
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
        <p class="muted">Issue new notes from a house your node controls. Units are base-native (1 unit = 1 sat); enter in grams or units.</p>
        <div class="form">
          <label>
            House ID
            <input type="number" bind:value={mintHouseId} placeholder="e.g. 1" />
          </label>
          <div class="conn-modes">
            <button type="button" class:active={mintInGrams} on:click={() => (mintInGrams = true)} disabled={!(gramRate > 0)}>Grams</button>
            <button type="button" class:active={!mintInGrams} on:click={() => (mintInGrams = false)}>Units</button>
          </div>
          <label>
            {mintInGrams ? "Amount (grams ☉)" : "Units"}
            <input type="number" bind:value={mintUnits} placeholder={mintInGrams ? "grams" : "units"} step={mintInGrams ? "0.000001" : "1"} />
          </label>
          {#if mintInGrams && gramRate > 0 && mintUnits && !isNaN(parseFloat(mintUnits))}
            <p class="hint">= {gramsToUnits(parseFloat(mintUnits)).toLocaleString()} units</p>
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
                <div><span class="stat-label">Reserve pledged</span> {h.activeescrow} ECX {#if gramRate > 0}<span class="muted">· {fmtG(h.activeescrow * 1e8)}</span>{/if}</div>
                <div><span class="stat-label">Notes outstanding</span> {h.mintedunits.toLocaleString()} / {h.mintcapunits.toLocaleString()} cap {#if gramRate > 0}<span class="muted">· {fmtG(h.mintedunits)}</span>{/if}</div>
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
      {#if pools.length > 0}
      <div class="card">
        <div class="notes-head">
          <h2>My liquidity</h2>
          <button class="link-btn" on:click={loadPools} disabled={poolsLoading}>
            {poolsLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if myLp.length === 0}
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
                <div><span class="stat-label">Underlying notes</span> {lp.my_note_units.toLocaleString()} units {#if gramRate > 0}<span class="muted">· {fmtG(lp.my_note_units)}</span>{/if}</div>
                <div><span class="stat-label">Underlying {BASE_TICKER}</span> {lp.my_btx_sats.toLocaleString()} sats {#if gramRate > 0}<span class="muted">· {fmtG(lp.my_btx_sats)}</span>{/if}</div>
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
                <div><span class="stat-label">Note reserve</span> {p.note_reserve.toLocaleString()} units {#if gramRate > 0}<span class="muted">· {fmtG(p.note_reserve)}</span>{/if}</div>
                <div><span class="stat-label">{BASE_TICKER} reserve</span> {p.btx_reserve.toLocaleString()} sats {#if gramRate > 0}<span class="muted">· {fmtG(p.btx_reserve)}</span>{/if}</div>
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
                      Amount in ({swapDir === "noteforbtx" ? "note units" : "sats"})
                      <input type="number" bind:value={poolAmountIn} placeholder={swapDir === "noteforbtx" ? "units" : "sats"} />
                    </label>
                    <label>
                      Min out ({swapDir === "noteforbtx" ? "sats" : "note units"})
                      <input type="number" bind:value={poolMinOut} placeholder="0 = no slippage limit" />
                    </label>
                  {:else if poolAction.type === "add"}
                    <label>
                      Note units
                      <input type="number" bind:value={poolAddNote} placeholder="units" />
                    </label>
                    <label>
                      {BASE_TICKER} (sats)
                      <input type="number" bind:value={poolAddBtx} placeholder="sats" />
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
            Seed note units
            <input type="number" bind:value={createNoteUnits} placeholder="units" />
          </label>
          <label>
            Seed {BASE_TICKER} (sats)
            <input type="number" bind:value={createBtxSats} placeholder="sats" />
          </label>
          <label>
            Fee (bps)
            <input type="number" bind:value={createFeeBps} placeholder="e.g. 30" />
          </label>
          <button on:click={doCreatePool} disabled={poolBusy || !createPoolId || !createNoteUnits || !createBtxSats}>
            {poolBusy ? "…" : "Create pool"}
          </button>
        </div>
      </div>
      {/if}
    {:else if currentView === "bills"}
      <!-- Bills (M4): bills of exchange — the discount-house asset side -->
      <div class="card">
        <div class="notes-head">
          <h2>My Bills</h2>
          <button class="link-btn" on:click={loadBills} disabled={billsLoading}>
            {billsLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">Bills of exchange you hold, drew, or accepted — a discount house's asset side: dated credit, backed by an escrow bond, that settles at par.</p>
        {#if bills.length === 0}
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
              {#if gramRate > 0}
                <div class="hint">{fmtG(b.amount * 1e8)} <span class="muted">launch scale — not enforced</span></div>
              {/if}
              <div class="house-stats">
                <div><span class="stat-label">Escrow bond</span> {b.escrow} {BASE_TICKER} {#if gramRate > 0}<span class="muted">· {fmtG(b.escrow * 1e8)}</span>{/if}</div>
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
      <!-- Send -->
      <div class="card">
        <h2>Send {BASE_TICKER}</h2>
        <div class="form">
          <label>
            Address
            <input type="text" bind:value={sendAddress} placeholder="X…" />
          </label>
          <label>
            Amount ({BASE_TICKER})
            <input
              type="number"
              bind:value={sendAmount}
              placeholder="0.00"
              step="0.00000001"
            />
          </label>
          <button on:click={send} disabled={sending || !sendAddress || !sendAmount}>
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
      </div>
    {:else if currentView === "receive"}
      <!-- Receive -->
      <div class="card">
        <h2>Receive {BASE_TICKER}</h2>
        {#if receiveAddress}
          <div class="address-display">
            <code>{receiveAddress}</code>
            <button on:click={() => navigator.clipboard.writeText(receiveAddress)}>
              Copy
            </button>
          </div>
          <!-- QR code would go here -->
          <div class="qr-placeholder">
            [QR Code]
          </div>
        {/if}
        <button on:click={generateAddress} disabled={generatingAddress}>
          {generatingAddress ? "Generating…" : "New Address"}
        </button>
      </div>
    {:else if currentView === "settings"}
      <!-- Settings -->
      {#if localNode}
        <NodeSettings on:removed={onRemoved} />
      {:else}
        <div class="card">
          <h2>Settings</h2>
          <p>Connected to: {host}:{port}</p>
          {#if blockchainInfo}
            <p>Chain: {blockchainInfo.chain}</p>
            <p>Blocks: {blockchainInfo.blocks}</p>
            <p>Difficulty: {blockchainInfo.difficulty.toExponential(2)}</p>
          {/if}
          <button on:click={() => { connected = false; localNode = false; }}>
            Disconnect
          </button>
        </div>
      {/if}
    {/if}
  {/if}

  {#if $versions}
    <footer class="app-foot">
      FreeBank app {$versions.app}{$versions.node ? ` · node ${$versions.node}` : ""}{#if $update?.available}<span class="avail">{$update.latest} available</span>{/if}
    </footer>
  {/if}
</main>

<style>
  /* Styles are in styles/app.css */
</style>
