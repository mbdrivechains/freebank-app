<script lang="ts">
  // First run: find the eCash beta stack, then connect to a running FreeBank node, or install
  // and start one. Dispatches "ready" once the wallet can talk to the node, and "manual" when the
  // user would rather connect to a node elsewhere.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import AdvancedSettings from "./AdvancedSettings.svelte";
  import {
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

  type Screen = "checking" | "unsupported" | "missing" | "locked" | "install" | "installing" | "syncing";
  let screen: Screen = "checking";
  let info: SetupInfo | null = null;
  let stack: StackCheck | null = null;
  let datadir: DatadirCheck | null = null;
  let lockedMessage = "";
  let checking = false;

  let tag = "";
  let moveAside = false;
  $: problem = tagProblem(tag);
  // Said right by the Install button whenever it is disabled.
  $: installBlocked = problem
    ? "Fix the name above to continue."
    : datadir?.kind === "other" && !moveAside
      ? "Tick the box above so the old data can be moved aside first."
      : "";

  let install: InstallProgress | null = null;
  let prog: NodeProgress | null = null;
  let startError = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pollGen = 0;

  const STAGES: [string, string][] = [
    ["release", "Find the newest release"],
    ["download", "Download"],
    ["verify", "Check it against SHA256SUMS"],
    ["unpack", "Unpack"],
    ["grpcurl", "Get grpcurl"],
    ["config", "Save your name"],
    ["start", "Start FreeBank"],
  ];
  $: stageIndex = install ? STAGES.findIndex(([id]) => id === install!.stage) : -1;

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

  async function check() {
    checking = true;
    stopPolling();
    startError = "";
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
      if (probe.state === "warming") return watchNode();
      if (probe.state === "locked") {
        lockedMessage = probe.message;
        screen = "locked";
        return;
      }
      stack = await node.stackCheck();
      if (!stack.found) {
        screen = "missing";
      } else if (info.installed) {
        await startNode();
      } else {
        datadir = await node.datadirCheck();
        moveAside = false;
        screen = "install";
      }
    } catch (e) {
      startError = String(e);
      screen = "missing";
    } finally {
      checking = false;
    }
  }

  async function finish() {
    stopPolling();
    await node.connectLocal();
    dispatch("ready");
  }

  async function startNode() {
    startError = "";
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
      } else if (prog.rpc.state === "up" && synced(prog)) {
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
    try {
      await node.installStart(tag, moveAside);
    } catch (e) {
      startError = String(e);
      return;
    }
    screen = "installing";
    poll(async () => {
      install = await node.installProgress();
      if (install.error) {
        stopPolling();
      } else if (install.done) {
        watchNode();
      }
    }, 500);
  }

  function mb(n: number): string {
    return (n / 1e6).toFixed(1);
  }
  function fmt(n: number | null | undefined): string {
    return n == null ? "…" : n.toLocaleString();
  }

  $: syncPct =
    prog && prog.rpc.blocks != null && prog.explorer_tip
      ? Math.min(100, (prog.rpc.blocks / prog.explorer_tip) * 100)
      : 0;

  onMount(check);
</script>

<div class="setup">
  {#if screen === "checking"}
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
    <button class="link-btn centered" on:click={() => dispatch("manual")}>Connect to a FreeBank node elsewhere</button>

  {:else if screen === "missing"}
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
        <button class="secondary" on:click={check} disabled={checking}>
          {checking ? "Checking…" : "Check again"}
        </button>
      </div>
    </div>

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
    {#if startError}<p class="soft-error">{startError}</p>{/if}

    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} on:saved={check} />
    {/if}
    <button class="link-btn centered" on:click={() => dispatch("manual")}>Connect to a FreeBank node elsewhere</button>

  {:else if screen === "locked"}
    <div class="hero">
      <h2>A FreeBank node is already running</h2>
      <p class="lede">{lockedMessage}</p>
      <p class="lede">Point FreeBank at that node's data folder under Advanced, or connect to it by hand.</p>
    </div>
    <div class="row-actions">
      <button class="secondary" on:click={check} disabled={checking}>Check again</button>
      <button class="secondary" on:click={() => dispatch("manual")}>Connect by hand</button>
    </div>
    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} open on:saved={check} />
    {/if}

  {:else if screen === "install"}
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
            <span class="path">Move to {datadir.away}</span>
          </span>
        </label>
      {:else if datadir && datadir.message}
        <p class="hint">{datadir.message}</p>
      {/if}

      <button class="wide" on:click={startInstall} disabled={!!installBlocked}>
        Install FreeBank
      </button>
      {#if installBlocked}<p class="blocked-why">{installBlocked}</p>{/if}
      <p class="fine">Downloads the newest FreeBank release from GitHub and checks it against its published checksums.</p>
    </div>
    {#if startError}<p class="soft-error">{startError}</p>{/if}

    {#if info}
      <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} on:saved={check} />
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
              {label}
              {#if state === "active" && id === "download" && install}
                <span class="stage-note">{mb(install.bytes)}{install.total ? ` of ${mb(install.total)}` : ""} MB</span>
                {#if install.total}
                  <span class="bar"><span class="bar-fill" style="width:{(install.bytes / install.total) * 100}%"></span></span>
                {/if}
              {:else if (state === "active" || state === "done") && install?.note && i === stageIndex}
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
        <button class="secondary" on:click={check}>Back</button>
      </div>
    {/if}

  {:else if screen === "syncing"}
    <div class="hero left">
      <h2>{prog?.rpc.state === "up" ? "Catching up" : "Starting FreeBank"}</h2>
      <p class="lede">
        {#if prog?.rpc.state === "up"}
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
          <div class="stat-num">{prog?.rpc.state === "up" ? fmt(prog.rpc.blocks) : "—"}</div>
        </div>
        <div class="stat-right">
          <div class="stat-cap">Explorer tip</div>
          <div class="stat-num dim">{fmt(prog?.explorer_tip)}</div>
        </div>
      </div>
      <span class="bar"><span class="bar-fill" class:indeterminate={prog?.rpc.state !== "up"} style="width:{prog?.rpc.state === 'up' ? syncPct : 100}%"></span></span>
      <div class="sync-meta">
        <span>{prog?.rpc.state === "up" ? `${prog.peers ?? 0} peer${prog.peers === 1 ? "" : "s"}` : "Warming up"}</span>
        {#if prog?.rpc.state === "warming" && prog.rpc.message}<span>{prog.rpc.message}</span>{/if}
      </div>
      {#if prog?.log_line && prog.rpc.state !== "up" && prog.log_line !== prog.rpc.message}
        <p class="log-line">{prog.log_line}</p>
      {/if}
      {#if prog?.rpc.state === "up"}
        <button class="wide secondary" on:click={finish}>Continue while it syncs</button>
      {/if}
    </div>

    {#if startError}
      <p class="soft-error">{startError}</p>
      <div class="row-actions">
        <button on:click={startNode}>Start again</button>
        <button class="secondary" on:click={check}>Back</button>
      </div>
      {#if info}
        <AdvancedSettings settings={info.settings} defaultDatadir={info.default_datadir} on:saved={check} />
      {/if}
    {/if}
  {/if}
</div>
