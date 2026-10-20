<script lang="ts">
  // First run: find the eCash beta stack, then connect to a running FreeBank node, or install
  // and start one. Dispatches "ready" once the wallet can talk to the node. ("manual", for a node
  // elsewhere, is not offered for now.) Each step after the first can go back one: the
  // install screen to the eCash node, a download can be cancelled, and a node this screen started
  // can be stopped. The header's gear opens Settings here too (`settingsOpen`), so someone stuck in
  // setup can still delete chain data, remove FreeBank or obliterate.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import AdvancedSettings from "./AdvancedSettings.svelte";
  import NodeSettings from "./NodeSettings.svelte";
  import PathText from "./PathText.svelte";
  import { addSample, sayLeft, secondsLeft, type SyncSample } from "../lib/syncEta";
  import {
    friendlyLog,
    megabytes,
    node,
    openUrl,
    randomTag,
    tagProblem,
    versions,
    BITWINDOW_URL,
    demoMode,
    type DatadirCheck,
    type InstallProgress,
    type NodeProgress,
    type SetupInfo,
    type StackCheck,
  } from "../lib/node";
  import {
    gigabytes,
    network,
    snapshotWords,
    stack as ownStack,
    stackStages,
    switchNetwork,
    SNAPSHOT_PENDING_START,
    type Remote,
    type StackProgress,
    type StackStatus,
  } from "../lib/network";

  const dispatch = createEventDispatcher<{ ready: void; manual: void }>();

  /** Settings shown instead of the setup screens (the header's gear). */
  export let settingsOpen = false;

  // "stack": the eCash node and enforcer, found or not.
  type Screen = "checking" | "unsupported" | "stack" | "locked" | "install" | "installing" | "syncing";
  let screen: Screen = "checking";
  let info: SetupInfo | null = null;
  let stack: StackCheck | null = null;
  /** Advanced holds typed addresses that differ from the saved ones, which the checklist shows. */
  let advEdited = false;
  let datadir: DatadirCheck | null = null;
  let lockedMessage = "";
  let checking = false;

  let tag = "";
  let moveAside = false;
  // An earlier install's folder: "use" it (its blocks, wallet and name) or start "fresh" (moved aside).
  let earlier: "use" | "fresh" | null = null;
  $: problem = tagProblem(tag);
  // Someone who has never installed FreeBank's node: the demo leads, as it takes minutes, not a full node's hours.
  $: newcomer = !!info?.demo_available && !info?.installed;
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
    ["config", "Save your name"], // "Save its settings" in demo mode, which asks for no name
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
      demoMode.set(!!info.settings.demo); // the watermark from the first setup screen, after a restart too
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
      // The app's own eCash node and enforcer (v0.5.0), stopped: start them, then look again. Not while they are being
      // installed (this screen was left and opened again): the install starts them itself.
      const ownStack_ = info.settings.stack;
      const ownInstalling = !stack.found && $network.stack ? await ownStack.progress().catch(() => null) : null;
      if (
        !stack.found &&
        ownStack_ &&
        !ownInstalling?.running &&
        info.settings.rest === `127.0.0.1:${ownStack_.ports.rpc}` &&
        !info.settings.demo
      ) {
        try {
          await ownStack.start();
          stack = await node.stackCheck();
        } catch (e) {
          // A snapshot waiting to load: its card offers Continue (v0.5.1).
          if (!String(e).startsWith(SNAPSHOT_PENDING_START)) startError = String(e);
        }
      }
      if (!stack.found && $network.stack) {
        own = await ownStack.status(true).catch(() => null);
        // An install under way (this screen was left and opened again): its progress again.
        const p = ownInstalling;
        if (p?.running) {
          ownProg = p;
          watchOwn();
        } else if (own?.installed && !own.snapshot_pending && own.node_up) {
          // Installed and catching up (a full sync takes days): look again every few seconds, and go on once it's there.
          watchSyncingOwn();
        }
      }
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
    // Demo mode with a node too old for it: installing fetches the newest release (data folder and wallet stay).
    if (info?.installed && (!info.settings.demo || info.demo_ready)) {
      await startNode();
    } else {
      await showInstall();
    }
  }

  // The eCash node and enforcer, installed by the app itself (v0.5.0; easynet): the status before (its ports, and
  // which other programs hold the defaults) and the install's progress.
  let own: StackStatus | null = null;
  let ownProg: StackProgress | null = null;
  // On beta, easynet is offered too (it needs no BitWindow).
  $: easynetOffered = $network.id === "beta" && $network.all.some(([id, , offered]) => id === "easynet" && offered);
  $: ownStages = stackStages(ownProg?.snapshot);
  $: ownStage = ownProg ? ownStages.findIndex(([id]) => id === ownProg!.stage) : -1;
  // Beta's (v0.5.1): the whole chain (about 890 GB), from a snapshot when one answers (looked for first),
  // its blocks on a disk of their own if the person likes.
  $: bigChain = (own?.full_gb ?? 0) >= 100;
  $: available = (own?.offers ?? []).filter((o) => o.available);
  let startFrom: string | null = null;
  $: if (own && startFrom === null) startFrom = available[0]?.id ?? "full";
  let blocksElsewhere = false;
  let blocksDir = "";
  let blocksFree: number | null = null;
  // An install refused before it starts (a blocks folder that won't do, one already running): said in the card, by its
  // button, not at the foot of the page.
  let ownError = "";
  let blocksMissing = false;
  async function checkBlocksDir() {
    ownError = "";
    const d = blocksDir.trim();
    blocksFree = d ? await ownStack.free(blocksDir).catch(() => null) : null;
    blocksMissing = !!d && blocksFree == null;
  }
  $: diskFree = blocksElsewhere ? (blocksDir.trim() ? blocksFree : null) : (own?.blocks_free ?? null);
  $: diskShort = bigChain && diskFree != null && diskFree < (own?.full_gb ?? 0) * 1e9;

  async function installOwn(resume = false) {
    startError = "";
    ownError = "";
    try {
      await ownStack.install(
        bigChain && !resume
          ? { snapshot: startFrom, blocks: blocksElsewhere ? blocksDir.trim() || null : null, take: true }
          : { take: true },
      );
    } catch (e) {
      ownError = String(e);
      return;
    }
    watchOwn();
  }
  function watchOwn() {
    poll(async () => {
      ownProg = await ownStack.progress().catch(() => ownProg);
      if (ownProg && !ownProg.running) {
        stopPolling();
        if (ownProg.done) check();
        else own = await ownStack.status(true).catch(() => own);
      }
    }, 700);
  }
  function watchSyncingOwn() {
    poll(async () => {
      own = await ownStack.status(true).catch(() => own);
      const c = await node.stackCheck().catch(() => null);
      if (c?.found) {
        stopPolling();
        check();
      }
    }, 10000);
  }

  // Stop and delete (v0.5.1): the install, the snapshot or the sync stops at once, and what it wrote goes.
  let removing = false;
  async function removeOwn() {
    removing = true;
    startError = "";
    try {
      await ownStack.remove();
      ownProg = null;
    } catch (e) {
      startError = String(e);
    }
    removing = false;
    stopPolling();
    await check(false);
  }

  async function startOwn() {
    await goOn(async () => {
      await ownStack.start();
    });
    await check();
  }

  // Another computer's eCash node and enforcer (v0.5.1): found by its IP or Tailscale name, used as they are.
  let remoteHost = "";
  let remote: Remote | null = null;
  let finding = false;
  async function findRemote() {
    if (!remoteHost.trim()) return;
    finding = true;
    remote = null;
    try {
      remote = await ownStack.findRemote(remoteHost);
    } catch (e) {
      startError = String(e);
    }
    finding = false;
  }
  async function useRemote() {
    const r = remote;
    if (!r?.rest || !r.enforcer) return;
    await goOn(async () => {
      await ownStack.useRemote(r.rest!, r.enforcer!);
    });
    remote = null;
    await check();
  }

  async function showInstall() {
    datadir = await node.datadirCheck();
    moveAside = false;
    earlier = null;
    cancelledNote = "";
    screen = "install";
  }

  // Demo mode (v0.4.4): play money without an eCash node, FreeBank's gateway answering for the eCash chain. Turned on,
  // setup goes straight on once the gateway answers; turned off, it shows the eCash checklist again.
  async function setDemo(on: boolean) {
    checking = true;
    startError = "";
    try {
      await node.setDemo(on);
      demoMode.set(on);
      stack = null; // the last check was of the other mode's stack: don't show its answer on this screen
    } catch (e) {
      startError = String(e);
      checking = false;
      return;
    }
    await check(on);
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

  // Time left while catching up (lib/syncEta.ts).
  let syncSamples: SyncSample[] = [];
  $: if (view?.rpc.state === "up" && view.rpc.blocks != null) {
    syncSamples = addSample(syncSamples, { at: Date.now(), blocks: view.rpc.blocks });
  } else if (syncSamples.length) {
    syncSamples = [];
  }
  $: syncLeft = view?.rpc.state === "up" ? sayLeft(secondsLeft(syncSamples, view.explorer_tip)) : "";

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
    <NodeSettings
      on:removed
      on:obliterated
      on:stackRemoved={() => {
        settingsOpen = false;
        check();
      }}
    />

  {:else if screen === "checking"}
    <div class="hero">
      <div class="spinner big" aria-hidden="true"></div>
      <h2>Looking for {$network.l1}</h2>
      <p class="lede">{info?.settings.demo ? "Checking FreeBank's gateway." : "Checking this computer for an eCash node and enforcer."}</p>
    </div>

  {:else if screen === "unsupported"}
    <div class="hero">
      <h2>Not available on this computer yet</h2>
      <p class="lede">{info?.platform_error}</p>
    </div>

  {:else if screen === "stack" && info?.settings.demo}
    <div class="hero left">
      <h2>Demo mode</h2>
      <p class="lede">
        FreeBank runs with play money and no eCash node: FreeBank's gateway answers its questions about the eCash
        chain. There are no deposits or withdrawals; Receive has Get play money.
      </p>
    </div>
    <div class="checklist">
      <div class="check-item" class:ok={stack?.found}>
        <span class="dot"></span>
        FreeBank's gateway
        <span class="check-state">{stack?.found ? `block ${fmt(stack.l1_blocks)}` : stack || !checking ? "not answering" : "checking…"}</span>
      </div>
    </div>
    {#if stack && !stack.found}<p class="hint">{stack.detail}</p>{/if}
    <div class="row-actions">
      {#if stack?.found}
        <button on:click={() => goOn(proceed)} disabled={checking}>{checking ? "One moment…" : "Continue"}</button>
      {:else}
        <button on:click={() => check()} disabled={checking}>{checking ? "Checking…" : "Check again"}</button>
      {/if}
      <button class="secondary" on:click={() => setDemo(false)} disabled={checking}>Leave demo mode</button>
    </div>
    {#if startError}<p class="soft-error">{startError}</p>{/if}

  {:else if screen === "stack"}
    {#if stack?.found}
      <div class="hero left">
        <h2>{$network.l1} node</h2>
        <p class="lede">
          FreeBank will use this {$network.l1} node and its enforcer. To use different ones, change them under
          Advanced below.
        </p>
      </div>
    {:else}
      <div class="hero">
        <div class="mark" aria-hidden="true">☉</div>
        {#if newcomer}
          <h2>Welcome to FreeBank</h2>
          <p class="lede">
            Try it in a few minutes with play money, or run it alongside your own <strong>{$network.l1} full node</strong>
            and its <strong>enforcer</strong>{#if easynetOffered}, or on <strong>easynet</strong>, a small test network
            FreeBank sets up for you{/if}.
          </p>
        {:else}
          <h2>FreeBank needs {$network.l1}</h2>
          <p class="lede">
            FreeBank runs alongside a <strong>{$network.l1} full node</strong> and its <strong>enforcer</strong>.
            This computer doesn't have them running yet.
          </p>
        {/if}
      </div>

      <!-- Newcomers see the demo first; someone who has run FreeBank before, the way back to their own eCash node. -->
      <div class="setup-cards">
      {#if info?.demo_available}
        <div class="card steps" data-testid="try-demo" style:order={newcomer ? 0 : 1}>
          <h3>Just trying it?</h3>
          <p>
            Try FreeBank with play money and no eCash node: FreeBank's gateway answers its questions about the eCash
            chain. The play money and loans come from FreeBank's Telegram bot, so you'll need Telegram. There are no
            deposits or withdrawals. You can leave demo mode later in Settings.
          </p>
          <div class="row-actions">
            <button class:secondary={!newcomer} on:click={() => setDemo(true)} disabled={checking}>Try the demo</button>
          </div>
        </div>
      {/if}
      {#if $network.stack}
        <div class="card steps" data-testid="own-stack">
          <h3>{bigChain ? `Let FreeBank run ${$network.l1}'s node` : "Let FreeBank set it up"}</h3>
          {#if bigChain}
            <p>
              FreeBank downloads {$network.l1}'s node and enforcer (about 105 MB), checks them against the hashes it
              knows, and runs them for you. The whole chain comes to about {own?.full_gb} GB.
            </p>
          {:else}
            <p>
              FreeBank downloads {$network.l1}'s node and enforcer (about 105 MB), checks them against the hashes it
              knows, and runs them for you. The chain is small: it syncs in a few minutes.
            </p>
          {/if}
          {#if own?.clashes?.length}
            <p class="hint">
              {#each own.clashes as c}Port {c.port} is in use by {c.name}. {/each}FreeBank's own node will use free ports
              instead, and leaves those programs alone.
            </p>
          {/if}
          {#if ownProg?.running}
            <ol class="stack-stages">
              {#each ownStages as [id, label], i}
                <li class:done={i < ownStage} class:now={i === ownStage}>
                  {label}{#if i === ownStage && id === "download-snapshot"}: {gigabytes(ownProg.bytes)} of {gigabytes(ownProg.total)}{#if ownProg.headers}; headers {ownProg.headers.toLocaleString()}{/if}{:else if i === ownStage && id.startsWith("download")}: {megabytes(ownProg.bytes, ownProg.total)}{:else if i === ownStage && id === "sync" && ownProg.headers}: block {(ownProg.blocks ?? 0).toLocaleString()} of {ownProg.headers.toLocaleString()}{/if}
                </li>
              {/each}
            </ol>
            {#if bigChain}
              <div class="row-actions">
                <button class="secondary danger-text" on:click={removeOwn} disabled={removing} data-testid="own-stack-remove">
                  {removing ? "Stopping…" : "Stop and delete"}
                </button>
              </div>
              <p class="hint">Stop and delete stops it at once and deletes what it has written so far.</p>
            {/if}
          {:else if own?.snapshot_pending}
            <p>
              FreeBank's {$network.l1} node hasn't loaded its snapshot yet. Continue carries on: a download goes on from
              where it stopped.
            </p>
            <div class="row-actions">
              <button on:click={() => installOwn(true)} disabled={checking} data-testid="own-stack-continue">Continue</button>
              <button class="secondary danger-text" on:click={removeOwn} disabled={removing || checking}>{removing ? "Stopping…" : "Stop and delete"}</button>
            </div>
          {:else if own?.installed}
            {#if own.stopped_low}<p class="soft-error">{own.stopped_low}</p>{/if}
            <p data-testid="own-stack-syncing">
              {#if own.node_up}
                FreeBank's {$network.l1} node is catching up: block {(own.blocks ?? 0).toLocaleString()}{own.headers ? ` of ${own.headers.toLocaleString()}` : ""}.
                FreeBank goes on by itself once it is far enough.
              {:else}
                FreeBank's {$network.l1} node is stopped.
              {/if}
              {#if own.used != null}It takes {gigabytes(own.used)}{own.blocks_free != null ? `, with ${gigabytes(own.blocks_free)} free there` : ""}.{/if}
            </p>
            <div class="row-actions">
              {#if !own.node_up}<button on:click={startOwn} disabled={checking}>Start it</button>{/if}
              <button class="secondary danger-text" on:click={removeOwn} disabled={removing || checking}>{removing ? "Stopping…" : "Stop and delete"}</button>
            </div>
          {:else}
            {#if bigChain}
              <fieldset class="start-from" data-testid="own-start-from">
                <legend>Start from</legend>
                {#each available as o (o.id)}
                  <label><input type="radio" bind:group={startFrom} value={o.id} /> <span>{snapshotWords(o)}</span></label>
                {/each}
                <label><input type="radio" bind:group={startFrom} value="full" /> <span>A full sync from the start: days, with nothing to download first.</span></label>
              </fieldset>
              {#if !available.length}
                <p class="hint">No snapshot answered just now, so the node syncs from the start. Check again later for a faster start.</p>
              {/if}
              <p class="hint">
                A snapshot gets the node to the chain's tip in hours, not days: most of it is the download (about 9.4 GB). It
                then checks the older blocks behind it, so the disk fills either way: a snapshot saves time, not space.
              </p>
              <p class="disk" data-testid="own-disk">
                Its blocks go in <code>{blocksElsewhere ? blocksDir.trim() || "the folder below" : own?.blocks_at}</code>{diskFree != null ? `: ${gigabytes(diskFree)} free there` : ""}.
              </p>
              <label class="check">
                <input type="checkbox" bind:checked={blocksElsewhere} />
                Put the blocks on a disk of their own (a bigger one). FreeBank makes its own folder in the one you give,
                and Stop and delete removes only that. The rest, about 15 GB, stays with FreeBank.
              </label>
              {#if blocksElsewhere}
                <input
                  type="text"
                  bind:value={blocksDir}
                  on:change={checkBlocksDir}
                  on:blur={checkBlocksDir}
                  placeholder="/media/you/big-disk"
                  spellcheck="false"
                  data-testid="own-blocks-dir"
                />
                {#if blocksMissing}
                  <p class="soft-error">That folder isn't there. Give one that exists, as a full path: FreeBank makes its own folder inside it.</p>
                {/if}
              {/if}
              {#if diskShort}
                <p class="hint warn" data-testid="own-disk-short">
                  That disk has {gigabytes(diskFree)} free; the whole chain needs about {own?.full_gb} GB. FreeBank stops the
                  node when under 20 GB is left, and you can stop and delete it at any point.
                </p>
              {:else}
                <p class="hint">You can stop and delete it at any point, and FreeBank stops the node itself if under 20 GB is left.</p>
              {/if}
            {/if}
            <div class="row-actions">
              <button on:click={() => installOwn()} disabled={checking || (blocksElsewhere && !blocksDir.trim())} data-testid="own-stack-install">Install the {$network.l1} node</button>
            </div>
          {/if}
          {#if ownProg?.error}<p class="soft-error">{ownProg.error}</p>{/if}
          {#if ownError}<p class="soft-error" data-testid="own-error">{ownError}</p>{/if}
        </div>
      {/if}
      {#if !info?.settings.demo}
        <div class="card steps" data-testid="remote-stack">
          <h3>Or use another computer's</h3>
          <p>
            If a computer near you runs {$network.l1}'s node and enforcer (in BitWindow, or set up by hand), FreeBank can
            use them. Nothing is downloaded here, and FreeBank deletes nothing on that computer.
          </p>
          <div class="row-inline">
            <input
              type="text"
              bind:value={remoteHost}
              placeholder="IP address or Tailscale name"
              spellcheck="false"
              data-testid="remote-host"
              on:keydown={(e) => e.key === "Enter" && findRemote()}
            />
            <button class="secondary" on:click={findRemote} disabled={finding || !remoteHost.trim()} data-testid="remote-find">{finding ? "Looking…" : "Find it"}</button>
          </div>
          {#if remote}
            {#if remote.rest && remote.enforcer && remote.on_network}
              <p class="hint" data-testid="remote-found">
                Found its eCash node at <code>{remote.rest}</code>{remote.l1_blocks != null ? ` (block ${remote.l1_blocks.toLocaleString()})` : ""}
                and its enforcer at <code>{remote.enforcer}</code>.
              </p>
              {#if remote.tailscale}
                <p class="hint">Over Tailscale: the link is encrypted.</p>
              {:else}
                <p class="hint warn">
                  This computer doesn't reach {remote.address} over Tailscale, so what FreeBank and that computer say to
                  each other crosses your network unencrypted, including the eCash login if you give FreeBank one.
                </p>
              {/if}
              <div class="row-actions">
                <button on:click={useRemote} disabled={checking || !!ownProg?.running} data-testid="remote-use">Use them</button>
              </div>
              {#if ownProg?.running}
                <p class="hint">FreeBank is installing its own node above: stop and delete it, or let it finish, first.</p>
              {/if}
            {:else}
              <p class="soft-error" data-testid="remote-missing">{remote.detail}</p>
              {#if remote.address && !(remote.rest && remote.enforcer)}
                <p class="hint">
                  For FreeBank to reach them, that computer's eCash node needs <code>rest=1</code>, <code>rpcbind=</code> its
                  own address and <code>rpcallowip=</code> this computer's; its enforcer needs <code>--serve-grpc-addr=</code>
                  its address and port 50051. FreeBank tried {remote.tried.join(", ")}.
                </p>
              {/if}
            {/if}
          {/if}
          <p class="hint">
            FreeBank's eCash wallet logs in to that node with a login you type under Settings, eCash. It then adds two
            watch-only wallets of its own there (the node looks through {$network.l1}'s blocks once to fill them) and sends your
            payments through it. It never opens that node's other wallets.
          </p>
        </div>
      {/if}
      {#if easynetOffered}
        <div class="card steps" data-testid="try-easynet">
          <h3>Or try easynet</h3>
          <p>
            easynet is a small test network on eCash's rules, with no value. FreeBank installs its eCash node and enforcer
            itself, and it syncs in a few minutes, with no BitWindow.
          </p>
          <div class="row-actions">
            <button class="secondary" on:click={() => switchNetwork("easynet").catch((e) => (startError = String(e)))} disabled={checking}>
              Switch to easynet
            </button>
          </div>
          <p class="hint">FreeBank restarts on easynet. Beta stays as it is, and you can switch back in Settings.</p>
        </div>
      {/if}
      {#if $network.id === "beta"}
      <div class="card steps">
        <h3>{newcomer ? "With your own eCash node" : "The easiest way"}</h3>
        <ol>
          <li>Install <strong>BitWindow</strong>.</li>
          <li>Choose full-node mode on <strong>eCash beta</strong> and let it sync.</li>
          <li>Come back here and press <em>Check again</em>.</li>
        </ol>
        <div class="row-actions">
          <button class:secondary={newcomer} on:click={() => openUrl(BITWINDOW_URL)}>Get BitWindow</button>
          <button class="secondary" on:click={() => check()} disabled={checking}>
            {checking ? "Checking…" : "Check again"}
          </button>
        </div>
      </div>
      {/if}
      </div>
    {/if}

    {#if stack}
      {#if advEdited}
        <p class="hint">This list checks the saved addresses. To use the ones typed under Advanced, test them there, then
          press <em>Use these addresses</em>.</p>
      {/if}
      <div class="checklist">
        <div class="check-item" class:ok={stack.rest_ok && stack.on_beta}>
          <span class="dot"></span>
          eCash node at <code>{info?.settings.rest}</code>
          <span class="check-state">{stack.rest_ok ? (stack.on_beta ? `on ${$network.l1}` : `not on ${$network.l1}`) : "not found"}</span>
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
      <AdvancedSettings
        settings={info.settings}
        defaultDatadir={info.default_datadir}
        open={!!stack?.found}
        on:saved={() => check()}
        on:recheck={() => check(false)}
        on:edited={(e) => (advEdited = e.detail)}
      />
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
      {#if info?.settings.demo}
        <p class="lede">
          FreeBank installs its own node: a few minutes to download it, check it and catch up with the FreeBank chain.
        </p>
      {:else}
        <p class="lede">FreeBank found {$network.l1}'s node and its enforcer. It can now install its own node alongside them.</p>
      {/if}
    </div>

    <div class="checklist">
      {#if info?.settings.demo}
        <div class="check-item ok">
          <span class="dot"></span>
          FreeBank's gateway
          <span class="check-state">block {fmt(stack?.l1_blocks)}</span>
        </div>
      {:else}
        <div class="check-item ok">
          <span class="dot"></span>
          {$network.l1} node <code>{info?.settings.rest}</code>
          <span class="check-state">block {fmt(stack?.l1_blocks)}</span>
        </div>
        <div class="check-item ok">
          <span class="dot"></span>
          Enforcer <code>{info?.settings.enforcer}</code>
          <span class="check-state">found</span>
        </div>
      {/if}
    </div>

    <div class="card">
      {#if info?.settings.demo && info.installed && !info.demo_ready}
        <p class="hint unchecked-note">
          Demo mode needs a newer FreeBank node than {info.settings.installed_tag}. Installing downloads the newest release
          and checks it. Your data folder and wallet stay as they are.
        </p>
      {/if}
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
      <!-- A demo node can't bid for blocks, so its name (still set, for leaving demo later) isn't asked for. -->
      {#if !info?.settings.demo || problem}
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
        <p class="hint">When your node wins a block (it can bid for them later, eCash › Bidding), the explorer shows this name on it. Any name you like, up to 64 characters.</p>
      {/if}
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
      {#if !info?.settings.demo}<p class="lede">Name on your blocks: <strong>{tag}</strong></p>{/if}
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
                  <!-- A new bar once the size is known, so it doesn't slide back from full (v0.2.6). -->
                  {#key !!install?.total}
                    <span class="bar-fill" class:indeterminate={state === "active" && !install?.total} style="width:{state === 'active' && !install?.total ? 100 : downloadPct}%"></span>
                  {/key}
                </span>
              {:else}
                {id === "config" && info?.settings.demo ? "Save its settings" : label}
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
          <div class="stat-cap">{$network.explorer ? "Explorer tip" : "Network tip"}</div>
          <div class="stat-num dim">{fmt(view?.explorer_tip)}</div>
        </div>
      </div>
      <span class="bar"><span class="bar-fill" class:indeterminate={view?.rpc.state !== "up"} style="width:{view?.rpc.state === 'up' ? syncPct : 100}%"></span></span>
      <div class="sync-meta">
        <span>{view?.rpc.state === "up" ? `${view.peers ?? 0} peer${view.peers === 1 ? "" : "s"}` : "Warming up"}</span>
        {#if syncLeft}<span data-testid="sync-left">{syncLeft}</span>{/if}
        {#if view?.rpc.state === "warming" && view.rpc.message}<span>{view.rpc.message}</span>{/if}
      </div>
      {#if view?.reindexing}
        <p class="hint">Your node is rebuilding its data from the blocks it already has. That takes a few minutes; your wallet stays as it is.</p>
      {/if}
      {#if view?.log_line && view.rpc.state !== "up" && view.log_line !== view.rpc.message && friendlyLog(view.log_line)}
        <p class="log-line">{friendlyLog(view.log_line)}</p>
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
  /* v0.5.1: beta's own node and another computer's. */
  .start-from {
    border: 0;
    margin: 0.5rem 0 0;
    padding: 0;
  }
  .start-from legend {
    font-weight: 600;
    margin-bottom: 0.25rem;
  }
  .start-from label,
  label.check {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin-top: 8px;
    cursor: pointer;
    font-size: 14px;
    color: var(--text-secondary);
  }
  .start-from legend,
  p.disk {
    font-size: 14px;
  }
  p.disk {
    color: var(--text-secondary);
  }
  .start-from label:has(input:checked) {
    color: var(--text-color);
  }
  .start-from input,
  label.check input {
    margin-top: 3px;
    accent-color: var(--accent-color);
    flex: none;
  }
  p.disk {
    margin: 0.75rem 0 0;
    overflow-wrap: anywhere;
  }
  .row-inline {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .row-inline input {
    flex: 1 1 12rem;
    min-width: 0;
  }
  .hint.warn {
    color: var(--warn-color, #b7791f);
  }
  .stack-stages {
    margin: 0.25rem 0 0;
    padding-left: 1.2rem;
    color: var(--muted, #8b93a1);
  }
  .stack-stages li.done {
    color: inherit;
    opacity: 0.7;
  }
  .stack-stages li.now {
    color: var(--text, inherit);
    font-weight: 600;
  }
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
  /* In demo mode nothing may come before Install in its card. */
  .card > button.wide:first-child {
    margin-top: 0;
  }
  .setup-cards {
    display: flex;
    flex-direction: column;
    gap: 14px; /* as .setup's own */
  }
</style>
