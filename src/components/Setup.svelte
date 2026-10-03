<script lang="ts">
  // First run: find the eCash beta stack, then connect to a running FreeBank node, or install
  // and start one. Dispatches "ready" once the wallet can talk to the node. ("manual", for a node
  // elsewhere, is not offered for now: operator, 2026-09-26; see distribution/todo/remote-node.md.) Each step after the first can go back one: the
  // install screen to the eCash node, a download can be cancelled, and a node this screen started
  // can be stopped. The header's gear opens Settings here too (`settingsOpen`), so someone stuck in
  // setup can still delete chain data, remove FreeBank or obliterate.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import AdvancedSettings from "./AdvancedSettings.svelte";
  import NodeSettings from "./NodeSettings.svelte";
  import PathText from "./PathText.svelte";
  import {
    megabytes,
    node,
    openUrl,
    randomTag,
    tagProblem,
    versions,
    BITWINDOW_URL,
    type DatadirCheck,
    type InstallProgress,
    type NodeProgress,
    type SetupInfo,
    type StackCheck,
  } from "../lib/node";

  const dispatch = createEventDispatcher<{ ready: void; manual: void }>();

  /** Settings shown instead of the setup screens (the header's gear). */
  export let settingsOpen = false;

  // "stack": the eCash node and enforcer, found or not.
  type Screen = "checking" | "unsupported" | "stack" | "locked" | "install" | "installing" | "syncing";
  let screen: Screen = "checking";
  let info: SetupInfo | null = null;
  let stack: StackCheck | null = null;
  let datadir: DatadirCheck | null = null;
  let lockedMessage = "";
  let checking = false;

  let tag = "";
  let moveAside = false;
  // An earlier install's folder: "use" it (its blocks, wallet and name) or start "fresh" (moved aside).
  let earlier: "use" | "fresh" | null = null;
  $: problem = tagProblem(tag);
  // Said right by the Install button whenever it is disabled.
  $: installBlocked = problem
    ? "Fix the name above to continue."
    : datadir?.kind === "other" && !moveAside
      ? "Tick the box above so the old data can be moved aside first."
      : datadir?.kind === "earlier" && !earlier
        ? "Choose Use it or Start fresh above."
        : "";

  function chooseEarlier(c: "use" | "fresh") {
    earlier = c;
    // The name goes with the choice, unless it was changed by hand.
    if (c === "fresh" && datadir?.tag && tag === datadir.tag) tag = info?.suggested_tag ?? tag;
    if (c === "use" && datadir?.tag && tag === info?.suggested_tag) tag = datadir.tag;
  }

  // "It holds blocks up to 403, a wallet and the name “Kestrel” on its blocks."
  function earlierFacts(d: DatadirCheck): string {
    const parts = [
      ...(d.height != null ? [`blocks up to ${d.height.toLocaleString()}`] : []),
      ...(d.has_wallet ? ["a wallet"] : []),
      ...(d.tag ? [`the name “${d.tag}” on its blocks`] : []),
    ];
    if (parts.length === 0) return "";
    const last = parts.pop();
    return `It holds ${parts.length ? `${parts.join(", ")} and ${last}` : last}.`;
  }

  let install: InstallProgress | null = null;
  let prog: NodeProgress | null = null;
  let startError = "";
  // Set when this screen started the node, so going back may stop it. A node that was already
  // starting when the app opened is left alone.
  let startedHere = false;
  let stopping = false;
  let backError = "";
  let cancelling = false;
  let cancelError = "";
  // Said on the install screen after "Cancel".
  let cancelledNote = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pollGen = 0;

  const STAGES: [string, string][] = [
    ["release", "Find the newest release"],
    ["signature", "Check its signature"],
    ["download", "Download"],
    ["verify", "Check it against the signed checksums"],
    ["unpack", "Unpack"],
    ["config", "Save your name"],
    ["start", "Start FreeBank"],
  ];
  $: stageIndex = install ? STAGES.findIndex(([id]) => id === install!.stage) : -1;
  // The stages the backend lets "Cancel" stop (install::cancellable): nothing is written yet.
  const CANCELLABLE = ["release", "signature", "download", "verify"];
  $: canCancel = !install || (install.running && CANCELLABLE.includes(install.stage));
  // While Cancel shows, the download's bar keeps its place (empty before, full after), so Cancel
  // never moves under the pointer.
  const DOWNLOAD = STAGES.findIndex(([id]) => id === "download");
  $: downloadPct =
    stageIndex > DOWNLOAD ? 100 : stageIndex === DOWNLOAD && install?.total ? (install.bytes / install.total) * 100 : 0;

  // A double-click's second click lands on whatever replaced the button: Install and Cancel can
  // share a spot. Only the first click counts (a key press has detail 0).
  function firstClick(fn: () => void) {
    return (e: MouseEvent) => {
      if (e.detail <= 1) fn();
    };
  }

  // One request at a time: the next poll is scheduled only after this one returns, so a busy
  // node (the RPC stalls while it verifies blocks) never gets a pile of queued calls.
  function poll(fn: () => Promise<void>, ms: number) {
    stopPolling();
    const gen = ++pollGen;
    const tick = async () => {
      await fn();
      if (gen === pollGen) timer = setTimeout(tick, ms);
    };
    tick();
  }
  function stopPolling() {
    pollGen++;
    if (timer) clearTimeout(timer);
    timer = null;
  }
  onDestroy(stopPolling);

  // auto: once the eCash node and enforcer are found, go straight on to installing or starting
  // FreeBank. Off when the user came back to look at them.
  async function check(auto = true) {
    checking = true;
    stopPolling();
    startError = "";
    backError = "";
    try {
      info = await node.setupInfo();
      versions.update((v) => v ?? { app: info!.app_version, node: null, commit: null });
      if (!tag) tag = info.current_tag || info.suggested_tag;
      if (info.platform_error) {
        screen = "unsupported";
        return;
      }
      const probe = await node.probe();
      if (probe.state === "up") return finish();
      if (probe.state === "warming") {
        startedHere = false;
        return watchNode();
      }
      if (probe.state === "locked") {
        lockedMessage = probe.message;
        screen = "locked";
        return;
      }
      stack = await node.stackCheck();
      if (!stack.found || !auto) {
        screen = "stack";
      } else {
        await proceed();
      }
    } catch (e) {
      startError = String(e);
      screen = "stack";
    } finally {
      checking = false;
    }
  }

  async function proceed() {
    if (info?.installed) {
      await startNode();
    } else {
      await showInstall();
    }
  }

  async function showInstall() {
    datadir = await node.datadirCheck();
    moveAside = false;
    earlier = null;
    cancelledNote = "";
    screen = "install";
  }

  // "Continue" on the eCash node screen, and "Back" after a failed install.
  async function goOn(step: () => Promise<void>) {
    checking = true;
    startError = "";
    try {
      await step();
    } catch (e) {
      startError = String(e);
    } finally {
      checking = false;
    }
  }

  let finishing = false;
  async function finish() {
    finishing = true;
    stopPolling();
    await node.connectLocal();
    dispatch("ready");
  }

  // The sync screen keeps showing the last "Catching up" while the node is only busy connecting blocks (it answers
  // late), instead of flicking back to "Starting FreeBank" (v0.2.5). A node that stops or rebuilds starts over.
  let lastUp: NodeProgress | null = null;
  $: if (prog?.rpc.state === "up") lastUp = prog;
  $: if (prog && (prog.rpc.state === "down" || prog.reindexing || prog.exited)) lastUp = null;
  $: view = prog?.rpc.state === "warming" && prog.rpc.busy && lastUp ? { ...lastUp, log_line: prog.log_line } : prog;

  async function startNode() {
    startError = "";
    startedHere = true;
    try {
      await node.start();
    } catch (e) {
      startError = String(e);
    }
    watchNode();
  }

  function watchNode() {
    screen = "syncing";
    poll(async () => {
      try {
        prog = await node.progress();
      } catch (e) {
        startError = String(e);
        return;
      }
      if (prog.exited) {
        startError = prog.exited;
        stopPolling();
      } else if (prog.rpc.state === "up" && synced(prog) && !settingsOpen) {
        // Not while Settings is open: someone there may be about to obliterate.
        await finish();
      }
    }, 2000);
  }

  function synced(p: NodeProgress): boolean {
    const b = p.rpc.blocks ?? 0;
    return (p.peers ?? 0) > 0 && p.explorer_tip != null && b >= p.explorer_tip - 1;
  }

  async function startInstall() {
    if (problem) return;
    startError = "";
    cancelledNote = "";
    try {
      await node.installStart(tag, datadir?.kind === "earlier" ? earlier === "fresh" : moveAside);
    } catch (e) {
      startError = String(e);
      return;
    }
    install = null;
    cancelError = "";
    startedHere = true;
    screen = "installing";
    poll(async () => {
      install = await node.installProgress();
      if (install.error) {
        // A refused cancel promised it would be ready in a moment; the error says otherwise.
        cancelError = "";
        stopPolling();
      } else if (install.done) {
        watchNode();
      }
    }, 500);
  }

  async function cancelInstall() {
    cancelling = true;
    cancelError = "";
    try {
      await node.installCancel();
    } catch (e) {
      // Too late: it is past the download and finishes by itself.
      cancelError = String(e);
      return;
    } finally {
      cancelling = false;
    }
    stopPolling();
    install = null;
    await goOn(showInstall);
    cancelledNote = "Install cancelled. Nothing was installed.";
  }

  // Back from "Starting FreeBank": stop the node this screen started, then show the eCash node.
  async function stopAndGoBack() {
    stopping = true;
    backError = "";
    stopPolling();
    try {
      await node.stop();
    } catch (e) {
      backError = String(e);
      stopping = false;
      watchNode();
      return;
    }
    stopping = false;
    startedHere = false;
    await check(false);
  }

  function fmt(n: number | null | undefined): string {
    return n == null ? "…" : n.toLocaleString();
  }

  $: syncPct =
    view && view.rpc.blocks != null && view.explorer_tip
      ? Math.min(100, (view.rpc.blocks / view.explorer_tip) * 100)
      : 0;

  // Back from Settings: its changes (the data folder, the ports) may change what setup finds. A
  // download or a starting node carries on by itself.
  let wasOpen = false;
  $: settingsToggled(settingsOpen);
  function settingsToggled(open: boolean) {
    if (wasOpen && !open && !["installing", "syncing", "checking"].includes(screen)) check(false);
    wasOpen = open;
  }

  onMount(() => {
    settingsOpen = false;
    check();
  });
</script>

<div class="setup">
  {#if settingsOpen}
    <button class="link-btn back-link" on:click={() => (settingsOpen = false)}>← Back to setup</button>
    <NodeSettings on:removed on:obliterated />

  {:else if screen === "checking"}
    <div class="hero">
      <div class="spinner big" aria-hidden="true"></div>
      <h2>Looking for eCash beta</h2>
      <p class="lede">Checking this computer for an eCash node and enforcer.</p>
    </div>

  {:else if screen === "unsupported"}
    <div class="hero">
      <h2>Not available on this computer yet</h2>
      <p class="lede">{info?.platform_error}</p>
    </div>

  {:else if screen === "stack"}
    {#if stack?.found}
      <div class="hero left">
        <h2>eCash beta node</h2>
        <p class="lede">
          FreeBank will use this eCash beta node and its enforcer. To use different ones, change them under
          Advanced below.
        </p>
      </div>
    {:else}
      <div class="hero">
        <div class="mark" aria-hidden="true">☉</div>
        <h2>FreeBank needs eCash beta</h2>
        <p class="lede">
          FreeBank runs alongside an <strong>eCash beta full node</strong> and its <strong>enforcer</strong>.
          This computer doesn't have them running yet.
        </p>
      </div>

      <div class="card steps">
        <h3>The easiest way</h3>
        <ol>
          <li>Install <strong>BitWindow</strong>.</li>
          <li>Choose full-node mode on <strong>eCash beta</strong> and let it sync.</li>
          <li>Come back here and press <em>Check again</em>.</li>
        </ol>
        <div class="row-actions">
          <button on:click={() => openUrl(BITWINDOW_URL)}>Get BitWindow</button>
          <button class="secondary" on:click={() => check()} disabled={checking}>
            {checking ? "Checking…" : "Check again"}
          </button>
        </div>
      </div>
    {/if}

    {#if stack}
      <div class="checklist">
        <div class="check-item" class:ok={stack.rest_ok && stack.on_beta}>
          <span class="dot"></span>
          eCash node at <code>{info?.settings.rest}</code>
          <span class="check-state">{stack.rest_ok ? (stack.on_beta ? "on beta" : "not on beta") : "not found"}</span>
        </div>
        <div class="check-item" class:ok={stack.enforcer_ok}>
          <span class="dot"></span>
          Enforcer at <code>{info?.settings.enforcer}</code>
          <span class="check-state">{stack.enforcer_ok ? "found" : "not found"}</span>
        </div>
      </div>
    {/if}
    {#if stack?.found}
      <div class="row-actions">
        <button on:click={() => goOn(proceed)} disabled={checking}>
          {checking ? "One moment…" : "Continue"}
        </button>
      </div>
    {/if}
    {#if startError}<p class="soft-error">{startError}</p>{/if}

    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} open={!!stack?.found} on:saved={() => check()} />
    {/if}

  {:else if screen === "locked"}
    <div class="hero">
      <h2>A FreeBank node is already running</h2>
      <p class="lede">{lockedMessage}</p>
      <p class="lede">Point FreeBank at that node's data folder under Advanced, or stop that node and check again.</p>
    </div>
    <div class="row-actions">
      <button class="secondary" on:click={() => check()} disabled={checking}>Check again</button>
    </div>
    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} open on:saved={() => check()} />
    {/if}

  {:else if screen === "install"}
    <button class="link-btn back-link" on:click={() => (screen = "stack")}>← Back</button>
    <div class="hero left">
      <h2>Set up FreeBank</h2>
      <p class="lede">FreeBank found an eCash beta node and its enforcer. It can now install its own node alongside them.</p>
    </div>

    <div class="checklist">
      <div class="check-item ok">
        <span class="dot"></span>
        eCash beta node <code>{info?.settings.rest}</code>
        <span class="check-state">block {fmt(stack?.l1_blocks)}</span>
      </div>
      <div class="check-item ok">
        <span class="dot"></span>
        Enforcer <code>{info?.settings.enforcer}</code>
        <span class="check-state">found</span>
      </div>
    </div>

    <div class="card">
      {#if info?.unverified}
        <p class="hint unchecked-note">
          FreeBank {info.unverified} is on this computer, but an earlier version of the app installed it without checking its
          signature, so it won't be started. Installing downloads the newest release and checks it. Your data folder and
          wallet stay as they are.
        </p>
      {/if}
      {#if datadir && datadir.kind === "earlier"}
        <div class="aside-box" role="radiogroup" aria-label="An earlier FreeBank folder">
          <div>
            {datadir.message} {earlierFacts(datadir)}
            <span class="path"><PathText path={info?.settings.datadir ?? ""} /></span>
            <label class="earlier-choice">
              <input type="radio" name="earlier" checked={earlier === "use"} on:change={() => chooseEarlier("use")} />
              <span><strong>Use it</strong>: carry on from where it was{datadir.has_wallet ? ", with its wallet" : ""}.</span>
            </label>
            <label class="earlier-choice">
              <input type="radio" name="earlier" checked={earlier === "fresh"} on:change={() => chooseEarlier("fresh")} />
              <span>
                <strong>Start fresh</strong>: move it aside and sync from the start. Nothing is deleted.
                <span class="path">Move to <PathText path={datadir.away ?? ""} /></span>
              </span>
            </label>
          </div>
        </div>
      {/if}
      <label class="field">
        <span class="field-label">Name on your blocks</span>
        <div class="input-with-btn">
          <input type="text" bind:value={tag} maxlength="64" spellcheck="false" autocomplete="off" />
          <button class="ghost" type="button" title="Suggest another name" on:click={() => (tag = randomTag())}>↻</button>
        </div>
      </label>
      {#if problem}
        <p class="field-problem">{problem}</p>
      {:else}
        <p class="hint">When your node wins a block, the explorer shows this name on it. Any name you like, up to 64 characters.</p>
      {/if}

      {#if datadir && datadir.kind === "other"}
        <label class="aside-box">
          <input type="checkbox" bind:checked={moveAside} />
          <span>
            {datadir.message}{datadir.has_wallet ? " (It includes a wallet.dat.)" : ""}
            <span class="path">Move to <PathText path={datadir.away ?? ""} /></span>
          </span>
        </label>
      {:else if datadir && datadir.message && datadir.kind !== "earlier"}
        <p class="hint">{datadir.message}</p>
      {/if}

      <button class="wide" on:click={firstClick(startInstall)} disabled={!!installBlocked}>
        Install FreeBank
      </button>
      {#if installBlocked}<p class="blocked-why">{installBlocked}</p>{/if}
      {#if cancelledNote}<p class="cancelled-note">{cancelledNote}</p>{/if}
      <p class="fine">
        Downloads the newest FreeBank release from GitHub and checks it was signed with the FreeBank release key and
        matches its checksums.
      </p>
    </div>
    {#if startError}<p class="soft-error">{startError}</p>{/if}

    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} on:saved={() => check()} />
    {/if}

  {:else if screen === "installing"}
    <div class="hero left">
      <h2>Installing FreeBank{install?.tag ? ` ${install.tag}` : ""}</h2>
      <p class="lede">Name on your blocks: <strong>{tag}</strong></p>
    </div>
    <div class="card">
      <ul class="stages">
        {#each STAGES as [id, label], i}
          {@const state = install?.error && i === stageIndex ? "failed" : i < stageIndex || install?.done ? "done" : i === stageIndex ? "active" : "todo"}
          <li class="stage {state}">
            <span class="stage-icon">
              {#if state === "done"}✓{:else if state === "failed"}!{:else if state === "active"}<span class="spinner"></span>{/if}
            </span>
            <span class="stage-text">
              {#if id === "download" && canCancel}
                <span class="stage-line">
                  {label}
                  {#if state === "active" && install}
                    <span class="stage-note">{megabytes(install.bytes, install.total)}</span>
                  {/if}
                </span>
                <span class="bar">
                  <span class="bar-fill" class:indeterminate={state === "active" && !install?.total} style="width:{state === 'active' && !install?.total ? 100 : downloadPct}%"></span>
                </span>
              {:else}
                {label}
              {/if}
              {#if (state === "active" || state === "done") && install?.note && i === stageIndex && id !== "download"}
                <span class="stage-note">{install.note}</span>
              {/if}
            </span>
          </li>
        {/each}
      </ul>
    </div>
    {#if install?.error}
      <p class="soft-error">{install.error}</p>
      <div class="row-actions">
        <button on:click={startInstall}>Try again</button>
        <button class="secondary" on:click={() => goOn(showInstall)}>Back</button>
      </div>
    {:else if canCancel}
      <div class="row-actions">
        <button class="secondary" on:click={firstClick(cancelInstall)} disabled={cancelling}>
          {cancelling ? "Cancelling…" : "Cancel"}
        </button>
      </div>
    {/if}
    {#if cancelError}<p class="soft-error">{cancelError}</p>{/if}

  {:else if screen === "syncing"}
    {#if startedHere || startError}
      <button class="link-btn back-link" on:click={stopAndGoBack} disabled={stopping}>
        {stopping ? "Stopping FreeBank…" : view?.exited || !startedHere ? "← Back" : "← Stop FreeBank and go back"}
      </button>
    {/if}
    {#if backError}<p class="soft-error">{backError}</p>{/if}
    <div class="hero left">
      <h2>{view?.rpc.state === "up" ? "Catching up" : "Starting FreeBank"}</h2>
      <p class="lede">
        {#if view?.rpc.state === "up"}
          Your node is fetching FreeBank blocks from its peers.
        {:else}
          The first start checks the eCash chain. This can take a few minutes.
        {/if}
      </p>
    </div>

    <div class="card">
      <div class="big-stat">
        <div>
          <div class="stat-cap">FreeBank block</div>
          <div class="stat-num">{view?.rpc.state === "up" ? fmt(view.rpc.blocks) : "—"}</div>
        </div>
        <div class="stat-right">
          <div class="stat-cap">Explorer tip</div>
          <div class="stat-num dim">{fmt(view?.explorer_tip)}</div>
        </div>
      </div>
      <span class="bar"><span class="bar-fill" class:indeterminate={view?.rpc.state !== "up"} style="width:{view?.rpc.state === 'up' ? syncPct : 100}%"></span></span>
      <div class="sync-meta">
        <span>{view?.rpc.state === "up" ? `${view.peers ?? 0} peer${view.peers === 1 ? "" : "s"}` : "Warming up"}</span>
        {#if view?.rpc.state === "warming" && view.rpc.message}<span>{view.rpc.message}</span>{/if}
      </div>
      {#if view?.reindexing}
        <p class="hint">Your node is rebuilding its data from the blocks it already has. That takes a few minutes; your wallet stays as it is.</p>
      {/if}
      {#if view?.log_line && view.rpc.state !== "up" && view.log_line !== view.rpc.message}
        <p class="log-line">{view.log_line}</p>
      {/if}
      {#if view?.rpc.state === "up"}
        <button class="wide secondary" on:click={finish} disabled={finishing}>{finishing ? "Opening FreeBank…" : "Continue while it syncs"}</button>
      {/if}
    </div>

    {#if startError}
      <p class="soft-error">{startError}</p>
      <div class="row-actions">
        <button on:click={startNode}>Start again</button>
      </div>
      {#if info}
        <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} on:saved={() => check()} />
      {/if}
    {/if}
  {/if}
</div>

<style>
  .earlier-choice {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin-top: 10px;
    color: var(--text-color);
    cursor: pointer;
  }
  .earlier-choice input {
    margin-top: 3px;
    accent-color: var(--accent-color);
  }
  .unchecked-note {
    margin-bottom: 16px;
  }
</style>
