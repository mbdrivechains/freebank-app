<script lang="ts">
  // The node was started once with -reindex (a new release's data format, or an unclean stop, asked for it).
  const REBUILDING =
    "Your node is rebuilding its data from the blocks it already has. That takes a few minutes. Your wallet stays as it is, but its balance may read low until the rebuild is done.";
  // The Node tab: height against the explorer, peers in and out, the name on your blocks (and
  // changing it), Stop/Start for the node the app runs, versions, and Update. A node the app left
  // running when it last closed says since when; a release installed before the app checked
  // signatures isn't started, and is downloaded and checked again from here.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import PathText from "./PathText.svelte";
  import WhatsRunning from "./WhatsRunning.svelte";
  import {
    friendlyLog,
    checkForUpdate,
    megabytes,
    node,
    openUrl,
    randomTag,
    tagProblem,
    update,
    versions,
    when,
    type InstallProgress,
    type NodeStatus,
  } from "../lib/node";

  let st: NodeStatus | null = null;
  let problem = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let alive = true;
  const dispatch = createEventDispatcher<{ height: number }>();

  async function load() {
    try {
      st = await node.status();
      problem = "";
      if (st.state === "up") dispatch("height", st.blocks);
      if (st.versions.node || !$versions) versions.set(st.versions);
    } catch (e) {
      problem = String(e);
    }
  }

  // Reload every 5 s, never overlapping (a node verifying blocks can take a while to answer).
  async function tick() {
    await load();
    if (alive) timer = setTimeout(tick, 5000);
  }
  onMount(() => {
    tick();
    checkForUpdate();
    watchUpdate(false);
  });
  onDestroy(() => {
    alive = false;
    if (timer) clearTimeout(timer);
    if (updTimer) clearTimeout(updTimer);
  });

  $: up = st?.state === "up";
  $: tip = st?.explorer_tip ?? null;
  $: inSync = !!st && up && tip != null && st.blocks >= tip - 1;
  $: pct = st && tip ? Math.min(100, (st.blocks / tip) * 100) : 0;
  $: outbound = st ? st.peers.filter((p) => !p.inbound).length : 0;
  $: inbound = st ? st.peers.length - outbound : 0;
  $: external = !!st && !st.managed && st.state !== "down" && st.state !== "busy";
  $: pill = !st
    ? ""
    : st.state === "busy"
      ? "Busy"
      : st.state === "down"
        ? "Stopped"
        : st.state === "warming"
          ? "Starting"
          : st.state === "locked"
            ? "Not ours"
            : inSync
              ? "In sync"
              : "Syncing";

  function fmt(n: number | null | undefined): string {
    return n == null ? "…" : n.toLocaleString();
  }
  // "/FreeBank:0.2.15/" -> "FreeBank 0.2.15"
  function ver(s: string): string {
    return s.replace(/^\/|\/$/g, "").replace(/:/g, " ").replace(/\//g, " · ") || "unknown";
  }

  // Stop / Start
  let acting = false;
  let actError = "";
  async function stopNode() {
    acting = true;
    actError = "";
    tagNote = "";
    try {
      await node.stop();
    } catch (e) {
      actError = String(e);
    }
    acting = false;
    load();
  }
  async function startNode() {
    acting = true;
    actError = "";
    tagNote = "";
    try {
      await node.start();
    } catch (e) {
      actError = String(e);
    }
    acting = false;
    load();
  }

  // Change the name on your blocks
  let editing = false;
  let newTag = "";
  let saving = false;
  let tagNote = "";
  $: tagErr = editing ? tagProblem(newTag) : "";
  function editTag() {
    newTag = st?.tag ?? randomTag();
    tagNote = "";
    editing = true;
  }
  async function saveTag() {
    if (tagErr || !st) return;
    if (newTag === st.tag) {
      editing = false;
      return;
    }
    saving = true;
    tagNote = "";
    actError = "";
    editing = false;
    try {
      const r = await node.setTag(newTag);
      tagNote =
        r === "restarted"
          ? "Saved. Your node restarted with the new name."
          : r === "external"
            ? "Saved in freebank.conf. Your node was started by another program, so restart it there to use the new name."
            : "Saved. Your node uses it from its next start.";
    } catch (e) {
      actError = String(e);
    }
    saving = false;
    load();
  }

  // Update, and "Download it again" for a release installed before the app checked signatures
  const UPDATE_STAGES: [string, string][] = [
    ["release", "Find the newest release"],
    ["signature", "Check its signature"],
    ["download", "Download"],
    ["verify", "Check it against the signed checksums"],
    ["unpack", "Unpack"],
    ["stop", "Stop the node"],
    ["start", "Start the new version"],
  ];
  const REFETCH_STAGES: [string, string][] = [
    ["release", "Find the release"],
    ["signature", "Check its signature"],
    ["download", "Download"],
    ["verify", "Check it against the signed checksums"],
    ["unpack", "Unpack"],
  ];
  let upd: InstallProgress | null = null;
  let updTimer: ReturnType<typeof setTimeout> | null = null;
  let checking = false;
  $: stages = upd?.what === "refetch" ? REFETCH_STAGES : UPDATE_STAGES;
  $: updIndex = upd ? stages.findIndex(([id]) => id === upd!.stage) : -1;
  $: showUpd = !!upd && (upd.running || upd.done || !!upd.error);
  // v0.2.16 was the first signed release: an older one can't be checked, only updated.
  $: signedRelease = !!st?.release && Number(st.release.replace(/^v0\.2\./, "")) >= 16;

  async function watchUpdate(started: boolean) {
    try {
      upd = await node.updateProgress();
    } catch {
      return;
    }
    if (!alive) return;
    if (upd.running || started) {
      updTimer = setTimeout(() => watchUpdate(false), 700);
    } else if (upd.done) {
      await checkForUpdate();
      load();
    }
  }
  async function startUpdate() {
    actError = "";
    try {
      await node.updateStart();
    } catch (e) {
      actError = String(e);
      return;
    }
    watchUpdate(true);
  }
  async function startRefetch() {
    actError = "";
    try {
      await node.refetchStart();
    } catch (e) {
      actError = String(e);
      return;
    }
    watchUpdate(true);
  }
  async function recheck() {
    checking = true;
    await checkForUpdate(true);
    checking = false;
  }
</script>

{#if problem && !st}
  <div class="card"><p class="muted">{problem}</p></div>
{:else if !st}
  <div class="card"><p class="muted">Reading your node…</p></div>
{:else}
  <div class="card node-card">
    <div class="node-head">
      <h2 class="node-title">Your FreeBank node</h2>
      <span class="pill" class:pill-ok={inSync}>{pill}</span>
    </div>
    {#if st.adopted && st.background_since}
      <p class="hint bg-note">Running in the background since {when(st.background_since)}.</p>
    {/if}

    {#if st.state === "up"}
      {#if st.reindexing}<p class="hint">{REBUILDING}</p>{/if}
      <div class="big-stat">
        <div>
          <div class="stat-cap">FreeBank block</div>
          <div class="stat-num">{fmt(st.blocks)}</div>
        </div>
        <div class="stat-right">
          <div class="stat-cap">Explorer</div>
          <div class="stat-num dim">{fmt(tip)}</div>
        </div>
      </div>
      <span class="bar"><span class="bar-fill" style="width:{pct}%"></span></span>
      <dl class="facts">
        <div><dt>eCash beta block</dt><dd>{fmt(st.l1_blocks)}</dd></div>
      </dl>
    {:else if st.state === "busy" || st.state === "warming"}
      <div class="node-quiet-state">
        <span class="spinner"></span>
        <span>{st.state === "busy" ? st.activity : st.reindexing ? REBUILDING : "Your node is starting. That takes a minute or so, longer the first time while it checks the eCash chain."}</span>
      </div>
      <span class="bar"><span class="bar-fill indeterminate" style="width:100%"></span></span>
      {#if st.log_line && st.log_line !== "Shutdown: done" && friendlyLog(st.log_line)}<p class="log-line">{friendlyLog(st.log_line)}</p>{/if}
    {:else if st.state === "down"}
      <div class="node-quiet-state">
        <span>
          {#if st.installed}
            FreeBank is stopped. Your wallet and data are kept{st.unverified ? "." : "; start it when you're ready."}
          {:else}
            No FreeBank node is running on this computer.
          {/if}
        </span>
      </div>
      {#if st.exited}<p class="log-line">{st.exited}</p>{/if}
      {#if st.installed && st.unverified}
        <p class="hint">
          FreeBank {st.release} on this computer was installed by an earlier version of the app, before it checked release
          signatures, so it won't be started.
          {signedRelease ? "Download it again: FreeBank checks its signature first." : "It is too old to check: update to the newest release."}
        </p>
        {#if !upd?.running}
          {#if signedRelease}
            <button class="wide" on:click={startRefetch}>Download {st.release} again and check it</button>
          {:else}
            <button class="wide" on:click={startUpdate}>Update to the newest release</button>
          {/if}
        {/if}
      {:else if st.installed}
        <button class="wide" on:click={startNode} disabled={acting}>{acting ? "Starting…" : "Start FreeBank"}</button>
      {/if}
    {:else}
      <p class="muted">{st.message}</p>
    {/if}

    <div class="name-row">
      <div class="node-name">
        <div class="stat-cap">Name on your blocks</div>
        {#if editing}
          <div class="input-with-btn">
            <input type="text" bind:value={newTag} maxlength="64" spellcheck="false" autocomplete="off" />
            <button class="ghost" type="button" title="Suggest another name" on:click={() => (newTag = randomTag())}>↻</button>
          </div>
        {:else}
          <div class="node-tag">
            {st.tag ?? "none set"}
            <button class="link-btn inline" on:click={editTag} disabled={saving || st.state === "busy"}>Change</button>
          </div>
        {/if}
      </div>
    </div>
    {#if editing}
      {#if tagErr}
        <p class="field-problem">{tagErr}</p>
      {:else}
        <p class="hint">
          {#if st.managed}
            Saving restarts your node. That takes a minute or so.
          {:else if external}
            Your node was started by another program. The name is saved in freebank.conf, and it takes effect when that program restarts the node.
          {:else}
            The explorer shows this name on blocks your node wins.
          {/if}
        </p>
      {/if}
      <div class="row-actions tag-actions">
        <button on:click={saveTag} disabled={!!tagErr}>Save</button>
        <button class="secondary" on:click={() => (editing = false)}>Cancel</button>
      </div>
    {:else if tagNote}
      <p class="hint ok-note">{tagNote}</p>
    {/if}
    <button class="wide secondary" on:click={() => openUrl(st?.explorer ?? "")}>Open the explorer ↗</button>
  </div>

  {#if actError}<p class="soft-error">{actError}</p>{/if}

  <div class="card">
    <dl class="facts versions">
      <div>
        <dt>Node</dt>
        <dd>
          {#if st.versions.node}
            FreeBank {st.versions.node}
          {:else if st.state === "up"}
            {ver(st.version)}
          {:else}
            {st.release ? `FreeBank ${st.release}` : "—"}
          {/if}
          {#if $update?.available}<span class="avail">{$update.latest} available</span>{/if}
        </dd>
      </div>
    </dl>

    {#if showUpd && upd}
      <ul class="stages upd-stages">
        {#each stages as [id, label], i}
          {@const state = upd.error && i === updIndex ? "failed" : i < updIndex || upd.done ? "done" : i === updIndex ? "active" : "todo"}
          <li class="stage {state}">
            <span class="stage-icon">
              {#if state === "done"}✓{:else if state === "failed"}!{:else if state === "active"}<span class="spinner"></span>{/if}
            </span>
            <span class="stage-text">
              {label}{id === "start" && upd.tag ? ` ${upd.tag}` : ""}
              {#if state === "active" && id === "download"}
                <span class="stage-note">{megabytes(upd.bytes, upd.total)}</span>
                {#if upd.total}
                  <span class="bar"><span class="bar-fill" style="width:{(upd.bytes / upd.total) * 100}%"></span></span>
                {/if}
              {:else if upd.note && i === updIndex}
                <span class="stage-note">{upd.note}</span>
              {/if}
            </span>
          </li>
        {/each}
      </ul>
      {#if upd.error}<p class="soft-error">{upd.error}</p>{/if}
      {#if upd.done && !upd.running}
        {#if upd.what === "refetch"}
          <p class="hint ok-note">FreeBank {upd.tag} is checked. Start it when you're ready.</p>
        {:else}
          <p class="hint ok-note">Updated to {upd.tag}. The previous release stays on disk in case you need it.</p>
        {/if}
      {/if}
    {/if}

    {#if $update?.available && !upd?.running}
      {#if external}
        <p class="hint">Your node was started by another program. Stop it there to update from here.</p>
      {/if}
      <button class="wide" on:click={startUpdate} disabled={external || st.state === "busy"}>
        Update node to {$update.latest}
      </button>
      <p class="fine">
        Downloads it from GitHub, checks it was signed with the FreeBank release key and matches its checksums, then
        restarts your node.
      </p>
    {:else if st.release && !upd?.running}
      <div class="update-row">
        <span class="muted small">
          {#if $update?.error}
            Couldn't reach GitHub just now.
          {:else if $update?.latest}
            Up to date.
          {/if}
        </span>
        <button class="link-btn" on:click={recheck} disabled={checking}>{checking ? "Checking…" : "Check for updates"}</button>
      </div>
    {/if}
  </div>

  {#if up}
    <div class="card">
      <div class="notes-head">
        <h2>Peers</h2>
        <span class="muted small">{outbound} out · {inbound} in</span>
      </div>
      {#if st.peers.length === 0}
        <p class="muted">No peers yet. Your node is looking for them.</p>
      {:else}
        <ul class="peers">
          {#each st.peers as p}
            <li class="peer">
              <span class="dir" class:dir-in={p.inbound}>{p.inbound ? "in" : "out"}</span>
              <span class="peer-main">
                <span class="peer-addr">{p.addr}</span>
                <span class="peer-ver">{ver(p.subver)}</span>
              </span>
              <span class="peer-height">{p.synced_blocks != null && p.synced_blocks >= 0 ? `#${p.synced_blocks.toLocaleString()}` : ""}</span>
            </li>
          {/each}
        </ul>
      {/if}
      {#if st.listens && st.peers_local}
        <p class="hint">Your node takes peers from this computer only (freebank.conf binds the peer port here). It syncs through its own outbound peers.</p>
      {:else if st.listens}
        <p class="hint">Others can reach you only if port {st.p2p_port} is open to this computer. Without it, you still sync through your own outbound peers.</p>
      {:else}
        <p class="hint">Your node takes no incoming peers (freebank.conf says listen=0). It syncs through its own outbound peers.</p>
      {/if}
    </div>
  {/if}

  <WhatsRunning {st} {external} />

  <div class="card quiet">
    <dl class="facts">
      <div><dt>Data folder</dt><dd class="mono path-dd"><PathText path={st.datadir} /></dd></div>
      <div>
        <dt>Started by</dt>
        <dd class="text-dd">
          {#if st.managed && st.adopted}
            this app, before it last closed; {st.keeps_running ? "it keeps running when you close the app" : "it stops when you quit"}
          {:else if st.managed && st.keeps_running}
            this app; it keeps running when you close the app
          {:else if st.managed}
            this app; it stops when you quit
          {:else if external}
            another program
          {:else}
            —
          {/if}
        </dd>
      </div>
    </dl>
    {#if st.managed && st.state !== "busy"}
      <button class="wide secondary" on:click={stopNode} disabled={acting}>{acting ? "Stopping…" : "Stop FreeBank"}</button>
      <p class="fine">It starts again when you press Start, or when you next open the app.</p>
    {/if}
  </div>
{/if}

<style>
  /* A path wraps after a slash (PathText), never mid-name */
  .path-dd {
    word-break: normal;
    overflow-wrap: anywhere;
  }
  /* Words wrap whole here (the facts' default breaks anywhere, for addresses) */
  .text-dd {
    word-break: normal;
    overflow-wrap: break-word;
  }
  .node-title {
    margin: 0;
    font-size: 17px;
  }
  .name-row {
    margin: 14px 0 10px;
  }
  .bg-note {
    margin: 2px 0 14px;
  }
</style>
