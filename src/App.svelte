<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api, getSavedConfig, type Transaction, type BlockchainInfo } from "./lib/api";
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
  import { appUpdate, startAppUpdateChecks } from "./lib/appUpdate";
  import AppUpdate from "./components/AppUpdate.svelte";
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
  import EcashPanel from "./components/EcashPanel.svelte";
  import CreditPanel from "./components/CreditPanel.svelte";
  import ReceivePanel from "./components/ReceivePanel.svelte";
  import EcashLogin from "./components/EcashLogin.svelte";
  import { depositOpen } from "./lib/deposit";
  import WalletsCard from "./components/WalletsCard.svelte";
  import { loadWallets, walletList, walletSelect } from "./lib/wallets";
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
  // v0.2.6 (the UX walk-through): four tabs, Home · Credit · eCash · Node, and Settings behind the gear. Send, Receive
  // and Deposit open in place on Home, as on the phone page.
  let currentView: "home" | "credit" | "ecash" | "node" | "settings" = "home";
  let homeAction: "" | "send" | "receive" | "deposit" = "";
  function openHome(a: "send" | "receive" | "deposit") {
    if (homeAction === a) { homeAction = ""; return; }
    if (a === "deposit") depositOpen.set(true);
    // Send shows "Available" and Max from the balance: fresh when it opens (found in the coin tests).
    if (a === "send") refresh();
    homeAction = a;
  }
  // The Deposit panel's own Hide closes it here too.
  $: if (homeAction === "deposit" && !$depositOpen) homeAction = "";
  // Several wallets (v0.2.6): the header's switcher, shown once there is more than one. Switching remounts the views,
  // so each reads the chosen wallet afresh.
  let walletKey = 0;
  $: if (connected && localNode) loadWallets().catch(() => {});
  $: activeWallet = $walletList.find((w) => w.active)?.name ?? "";
  async function chooseWallet(e: Event) {
    const v = (e.target as HTMLSelectElement).value;
    try {
      await walletSelect(v || null);
      await loadWallets();
      walletKey += 1;
      await refresh();
    } catch (err) {
      error = nice(err);
    }
  }
  // Settings: one section at a time, from an index (it was one long scroll).
  type SettingsPart = "wallet" | "phone" | "node" | "security" | "about" | "startover";
  // The eCash tab's "Open the eCash login" lands on the login itself, two screens down Node & connection.
  async function openFromEcash(part: string) {
    currentView = "settings";
    settingsPart = part === "wallet" ? "wallet" : "node";
    if (settingsPart !== "node") return;
    for (let i = 0; i < 20; i++) {
      await tick();
      const el = document.querySelector('[data-testid="ecash-login"]');
      if (el) return el.scrollIntoView({ block: "start" });
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  let settingsPart: SettingsPart = "wallet";
  $: settingsParts = (localNode
    ? [["wallet", "Wallet"], ["phone", "Phone"], ["node", "Node & connection"], ["security", "Security"], ["about", "About"], ["startover", "Start over"]]
    : [["node", "Connection"], ["phone", "Phone"], ["security", "Security"], ["about", "About"]]) as [SettingsPart, string][];
  $: if (!settingsParts.some(([p]) => p === settingsPart)) settingsPart = settingsParts[0][0];
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
    currentView = "home";
    await refresh();
    checkForUpdate();
  }

  // After "Remove FreeBank": say where the data is kept, then back to setup.
  let removed: Removed | null = null;
  function onRemoved(e: CustomEvent<Removed>) {
    removed = e.detail;
    connected = false;
    localNode = false;
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

  const NEED_COINS = "You need FreeBank coins first: From eCash, above, shows how to deposit them.";

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

  // Addresses (Receive, Deposit) show only once the wallet has its passphrase: setting one replaces the
  // wallet's seed. v0.2.0 wallet: a new wallet's address screens wait until it is protected
  // ($holdAddresses, lib/walletSeed.ts); an older wallet's show, and its Home banner asks for the passphrase.
  $: canShowAddresses = !$holdAddresses;
  // The security checks run when the app connects, and again on each visit to Home.
  $: if (connected) runSecurityCheck();

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

  async function refresh(quiet = false) {
    if (!connected) return;
    try {
      [balance, transactions, blockchainInfo] = await Promise.all([
        api.getBalance(),
        api.getTransactions(20),
        api.getBlockchainInfo(),
      ]);
    } catch (e) {
      if (!quiet) error = nice(e);
    }
  }

  // The balance, the list and the header's block, every 20 seconds on every tab (v0.2.6, the UX walk-through: they
  // moved only on Home/Send clicks, so a confirmed payment's "on its way" went while the balance kept its old figure,
  // and the header's block froze on the other tabs). Quietly: a node that's busy says so on its own tab.
  onMount(() => {
    const t = setInterval(() => {
      if (connected && !document.hidden) refresh(true);
    }, 20_000);
    return () => clearInterval(t);
  });

  // The network by name, never the node's raw chain word (v0.2.6): a node before v0.2.19 calls beta "main"; v0.2.19
  // calls it "beta", and its "main" is FreeBank's mainnet (it says "dormant" until switched on).
  function networkName(i: BlockchainInfo): string {
    if (i.chain === "beta") return "Beta";
    if (i.chain === "main") return "dormant" in i ? "Mainnet" : "Beta";
    if (i.chain === "regtest") return "Regtest";
    return i.chain;
  }


  // The Mac's Help menu: "Report a Problem or Suggest Something…" (src-tauri/src/lib.rs).
  onMount(() => {
    if (isPWA) return;
    // The app's own updates: a notice when a signed release is out (lib/appUpdate.ts).
    startAppUpdateChecks();
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
    <h1 title={APP_NAME}><span class="brand-mark" aria-hidden="true">☉</span><span class="app-name">{APP_NAME}</span></h1>
    {#if connected || onSetup}
      <div class="head-right">
        {#if connected && localNode && $walletList.length > 1}
          <select class="wallet-pick" value={activeWallet} on:change={chooseWallet} aria-label="Wallet" data-testid="wallet-pick">
            {#each $walletList as w (w.name ?? "")}
              <option value={w.name ?? ""}>{w.label}</option>
            {/each}
          </select>
        {/if}
        {#if blockchainInfo}
          <span class="network">{networkName(blockchainInfo)} · block {blockchainInfo.blocks.toLocaleString()}</span>
        {/if}
        <button
          class="gear"
          class:active={onSetup ? setupSettings : currentView === "settings"}
          title="Settings"
          aria-label="Settings"
          on:click={() => {
            if (onSetup) setupSettings = !setupSettings;
            else currentView = currentView === "settings" ? "home" : "settings";
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

  {#if !isPWA && !gone}<AppUpdate notice />{/if}

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
    <!-- Navigation: four tabs (v0.2.6) -->
    <nav class="tabs">
      <button class:active={currentView === "home"} on:click={() => { currentView = "home"; error = ""; refresh(); }}>
        Home
      </button>
      <button class:active={currentView === "credit"} on:click={() => (currentView = "credit")}>
        Credit
      </button>
      <button class:active={currentView === "ecash"} on:click={() => (currentView = "ecash")}>
        eCash
      </button>
      {#if localNode}
        <button class:active={currentView === "node"} on:click={() => (currentView = "node")}>
          Node
        </button>
      {/if}
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

    {#key walletKey}
    {#if currentView === "node"}
      <NodeStatus
        on:height={(e) => {
          if (blockchainInfo) blockchainInfo = { ...blockchainInfo, blocks: e.detail };
        }}
      />
    {:else if currentView === "home"}
      <WalletBanner />
      <!-- Home / Dashboard -->
      <SecurityAlerts on:open={() => { currentView = "settings"; settingsPart = "security"; }} />
      <BalanceCard {balance} onRefresh={refresh} />
      <div class="home-actions">
        <button class:active={homeAction === "send"} on:click={() => openHome("send")} data-testid="home-send">Send</button>
        <button class:active={homeAction === "receive"} on:click={() => openHome("receive")} data-testid="home-receive">Receive</button>
        <button class:active={homeAction === "deposit"} on:click={() => openHome("deposit")} data-testid="home-deposit">From eCash</button>
      </div>
      {#if homeAction === "send"}
        <!-- Send: Max, a speed, the fee shown before Confirm; the receipt has Speed up -->
        <SendPanel {balance} on:sent={() => { refresh(); homeAction = ""; }} />
      {:else if homeAction === "receive"}
        <ReceivePanel {canShowAddresses} />
      {:else if homeAction === "deposit"}
        <DepositPanel {canShowAddresses} />
      {/if}

      <HomeTransactions {transactions} {balance} needCoins={NEED_COINS} on:changed={() => refresh()} />
    {:else if currentView === "ecash"}
      <EcashPanel on:settings={(e) => openFromEcash(e.detail)} />
    {:else if currentView === "credit"}
      <CreditPanel {balance} height={blockchainInfo?.blocks ?? 0} syncing={!!blockchainInfo && blockchainInfo.headers > blockchainInfo.blocks + 2} />
    {:else if currentView === "settings"}
      <!-- Settings: an index, one section at a time (v0.2.6); Start over last. -->
      <nav class="segments settings-index" aria-label="Settings">
        {#each settingsParts as [p, label]}
          <button class:active={settingsPart === p} class:danger-text={p === "startover"} on:click={() => (settingsPart = p)}>{label}</button>
        {/each}
      </nav>
      {#if settingsPart === "wallet" && localNode}
        <WalletsCard />
        <WalletSettings />
      {:else if settingsPart === "phone"}
        {#if !isPWA}<PhoneSettings />{/if}
      {:else if settingsPart === "node"}
        {#if localNode}
          <NodeSettings part="connection" on:removed={onRemoved} on:obliterated={onObliterated} />
          <EcashLogin />
        {:else}
          <div class="card">
            <h3>Connection</h3>
            <p>Connected to: {host}:{port}</p>
            {#if blockchainInfo}
              <p>Network: {networkName(blockchainInfo)} · block {blockchainInfo.blocks.toLocaleString()}</p>
            {/if}
            <button on:click={() => { connected = false; localNode = false; clearReceipts(); }}>
              Disconnect
            </button>
          </div>
        {/if}
      {:else if settingsPart === "security"}
        {#if !isPWA}<SecuritySettings />{/if}
      {:else if settingsPart === "about"}
        {#if !isPWA}<AppUpdate /><HelpSettings />{/if}
      {:else if settingsPart === "startover" && localNode}
        <NodeSettings part="startover" on:removed={onRemoved} on:obliterated={onObliterated} />
      {/if}
    {/if}
    {/key}
  {/if}

  {#if $reportDraft}<ReportDialog />{/if}

  {#if $versions}
    <footer class="app-foot">
      FreeBank app {$versions.app}{#if $appUpdate?.available}<span class="avail">{$appUpdate.latest} available</span>{/if}{$versions.node ? ` · node ${$versions.node}` : ""}{#if $update?.available}<span class="avail">{$update.latest} available</span>{/if}
    </footer>
  {/if}
</main>

<style>
  /* Styles are in styles/app.css */
</style>
