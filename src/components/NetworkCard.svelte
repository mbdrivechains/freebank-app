<script lang="ts">
  // Settings › Node & connection (v0.5.0): which network FreeBank runs on, the switch to another (the app restarts on
  // it; each network keeps its own wallet, settings and node), and the eCash node and enforcer the app runs itself for
  // this network, with Start and Stop. v0.5.1: how it started (a snapshot or a full sync), the older blocks it is
  // still checking, what it takes on disk and what is free, and Stop and delete.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { gigabytes, network, stack, stackStages, startWords, stoppedOwnWords, switchNetwork, type Removed, type StackProgress, type StackStatus } from "../lib/network";

  let asking: string | null = null;
  let busy = false;
  let error = "";
  let own: StackStatus | null = null;
  let timer: ReturnType<typeof setInterval> | undefined;
  // Continue and Update (v0.6.3): the install runs on its own, so this card follows it through its progress, to show
  // its stage and, when it stops, why.
  let installing = false;
  let prog: StackProgress | null = null;
  $: progWords = prog?.running ? stackStages(prog.snapshot).find(([id]) => id === prog?.stage)?.[1] ?? "" : "";

  $: others = $network.all.filter(([id]) => id !== $network.id);
  $: askName = $network.all.find(([id]) => id === asking)?.[1] ?? "";

  async function load() {
    if (!$network.stack) return;
    own = await stack.status().catch(() => own);
    if (installing) {
      prog = await stack.progress().catch(() => prog);
      if (prog && !prog.running) {
        installing = false;
        if (prog.error) error = prog.error;
      }
    }
  }

  async function install() {
    await stack.install();
    installing = true;
  }
  onMount(() => {
    load();
    timer = setInterval(load, 5000);
  });
  onDestroy(() => clearInterval(timer));

  async function go() {
    if (!asking) return;
    busy = true;
    error = "";
    try {
      await switchNetwork(asking); // the app restarts
    } catch (e) {
      error = String(e);
      busy = false;
    }
  }

  async function run(f: () => Promise<void>) {
    busy = true;
    error = "";
    try {
      await f();
    } catch (e) {
      error = String(e);
    }
    await load();
    busy = false;
  }

  // Stop and delete (v0.5.1): asked once, then the node and the install stop at once and the chain goes. When it was
  // the eCash node the app used, the app goes back to setup to choose another (App.svelte).
  const dispatch = createEventDispatcher<{ stackRemoved: Removed }>();
  let deleting = false;
  let removed: Removed | null = null;
  async function removeIt() {
    busy = true;
    error = "";
    try {
      removed = await stack.remove();
      deleting = false;
      own = null;
      if (removed.in_use) dispatch("stackRemoved", removed);
    } catch (e) {
      error = String(e);
    }
    await load();
    busy = false;
  }
</script>

<div class="card" data-testid="network-card">
  <h3>Network</h3>
  <p class="small">
    FreeBank runs on <strong>{$network.name}</strong>{$network.l1 !== $network.name ? `, alongside ${$network.l1}` : ""}.
  </p>
  {#if asking}
    <p class="small">
      FreeBank restarts on {askName}. {$network.name} stays as it is: its wallet, settings and node are kept, and you can
      switch back here. {askName} has its own.
    </p>
    <div class="row-actions">
      <button on:click={go} disabled={busy} data-testid="network-switch-go">{busy ? "Restarting…" : `Switch to ${askName}`}</button>
      <button class="secondary" on:click={() => (asking = null)} disabled={busy}>Cancel</button>
    </div>
  {:else if others.length}
    <div class="row-actions">
      {#each others as [id, name, offered]}
        <button class="secondary" on:click={() => (asking = id)} disabled={!offered} title={offered ? "" : "From the day it starts"}>
          {offered ? `Switch to ${name}` : `${name}: not yet`}
        </button>
      {/each}
    </div>
  {/if}

  {#if own?.installed}
    <h4 class="nc-own">Your {$network.l1} node</h4>
    <dl class="facts">
      <div><dt>eCash node</dt><dd>{own.node_up ? `block ${own.blocks?.toLocaleString() ?? "?"}${own.headers && own.blocks !== null && own.headers > own.blocks ? ` of ${own.headers.toLocaleString()}` : ""}, ${own.peers ?? 0} peer${own.peers === 1 ? "" : "s"}` : "stopped"}</dd></div>
      <div><dt>Enforcer</dt><dd>{own.enforcer_up ? `running${own.enforcer_tip != null ? `, block ${own.enforcer_tip.toLocaleString()}` : ""}` : "stopped"}</dd></div>
      {#if own.ports}
        <div><dt>Ports</dt><dd class="ports">network&nbsp;{own.ports.p2p} · RPC&nbsp;{own.ports.rpc} · enforcer&nbsp;{own.ports.grpc}</dd></div>
      {/if}
      <div><dt>Release</dt><dd class="mono">{own.installed}</dd></div>
      {#if startWords(own.start)}
        <div><dt>Started</dt><dd>{startWords(own.start)}</dd></div>
      {/if}
      {#if own.checked_to != null && own.checked_of}
        <div><dt>Older blocks</dt><dd>checked up to {own.checked_to.toLocaleString()} of {own.checked_of.toLocaleString()}</dd></div>
      {/if}
      {#if own.used != null}
        <div>
          <dt>On disk</dt>
          <dd data-testid="stack-disk">
            {gigabytes(own.used)} of about {own.full_gb} GB{#if own.blocks_free != null}; {gigabytes(own.blocks_free)} free there{/if}{#if own.data_free != null}, {gigabytes(own.data_free)} where its other data is{/if}
          </dd>
        </div>
      {/if}
    </dl>
    {#if own.stopped_low}<p class="soft-error">{own.stopped_low}</p>{/if}
    {#if own.stopped_own}<p class={own.stopped_own.restarted ? "small" : "soft-error"} data-testid="stack-stopped-own">{stoppedOwnWords(own.stopped_own)}</p>{/if}
    {#if own.snapshot_pending}
      <p class="small">Its snapshot isn't loaded yet. Continue carries on: a download goes on from where it stopped.</p>
      <div class="row-actions">
        <button on:click={() => run(install)} disabled={busy || installing}>Continue</button>
      </div>
    {/if}
    {#if own.update}
      <p class="small">FreeBank has a newer {$network.l1} node, {own.update}. Updating stops these, puts it in place and starts it.</p>
      <div class="row-actions">
        <button on:click={() => run(install)} disabled={busy || installing}>Update to {own.update}</button>
      </div>
    {/if}
    {#if installing && progWords}<p class="small">{progWords}…</p>{/if}
    <div class="row-actions">
      {#if !(own.node_up && own.enforcer_up) && !own.snapshot_pending}
        <button class="secondary" on:click={() => run(stack.start)} disabled={busy}>Start them</button>
      {/if}
      {#if own.node_up || own.enforcer_up}
        <button class="secondary" on:click={() => run(stack.stop)} disabled={busy}>Stop them</button>
      {/if}
      <button class="secondary danger-text" on:click={() => (deleting = true)} disabled={busy || deleting} data-testid="stack-remove">Stop and delete…</button>
    </div>
    {#if deleting}
      <div class="confirm-box">
        <p>
          This stops the FreeBank node, then this {$network.l1} node and its enforcer, and deletes their chain and data
          ({gigabytes(own.used)}). The watch-only eCash wallets FreeBank made in that node go with it: their keys come
          from your recovery words. FreeBank's own node and wallet stay, and so do the programs, so setting it
          up again downloads only the chain.
        </p>
        <div class="row-actions">
          <button class="danger" on:click={removeIt} disabled={busy} data-testid="stack-remove-go">{busy ? "Deleting…" : "Stop and delete"}</button>
          <button class="secondary" on:click={() => (deleting = false)} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}
    <p class="hint">The FreeBank node needs both: stop it first (Node tab), or it stops when they do.</p>
  {/if}
  {#if removed}
    <p class="small" data-testid="stack-removed">
      FreeBank's {$network.l1} node is stopped and deleted{removed.deleted.length > 1 ? ` (${removed.deleted.length} folders)` : ""}: {gigabytes(removed.freed)} freed.{#if removed.kept.length} Left {removed.kept.join(", ")}: it doesn't carry FreeBank's mark.{/if}
    </p>
  {/if}
  {#if error}<p class="soft-error">{error}</p>{/if}
</div>

<style>
  p.small {
    margin: 0 0 0.6rem;
  }
  .nc-own {
    margin-top: 1rem;
  }
  .ports {
    word-break: keep-all;
    font-variant-numeric: tabular-nums;
  }
  .facts {
    margin-bottom: 14px;
  }
  .facts div {
    display: flex;
    gap: 0.75rem;
  }
  .facts dt {
    min-width: 7rem;
    color: var(--muted, #8b93a1);
  }
</style>
