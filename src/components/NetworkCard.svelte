<script lang="ts">
  // Settings › Node & connection (v0.5.0): which network FreeBank runs on, the switch to another (the app restarts on
  // it; each network keeps its own wallet, settings and node), and the eCash node and enforcer the app runs itself for
  // this network, with Start and Stop.
  import { onDestroy, onMount } from "svelte";
  import { network, stack, switchNetwork, type StackStatus } from "../lib/network";

  let asking: string | null = null;
  let busy = false;
  let error = "";
  let own: StackStatus | null = null;
  let timer: ReturnType<typeof setInterval> | undefined;

  $: others = $network.all.filter(([id]) => id !== $network.id);
  $: askName = $network.all.find(([id]) => id === asking)?.[1] ?? "";

  async function load() {
    if (!$network.stack) return;
    own = await stack.status().catch(() => own);
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
    </dl>
    {#if own.update}
      <p class="small">FreeBank has a newer {$network.l1} node, {own.update}. Updating stops these, puts it in place and starts it.</p>
      <div class="row-actions">
        <button on:click={() => run(stack.install)} disabled={busy}>Update to {own.update}</button>
      </div>
    {/if}
    <div class="row-actions">
      {#if own.node_up || own.enforcer_up}
        <button class="secondary" on:click={() => run(stack.stop)} disabled={busy}>Stop them</button>
      {:else}
        <button class="secondary" on:click={() => run(stack.start)} disabled={busy}>Start them</button>
      {/if}
    </div>
    <p class="hint">The FreeBank node needs both: stop it first (Node tab), or it stops when they do.</p>
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
  .facts div {
    display: flex;
    gap: 0.75rem;
  }
  .facts dt {
    min-width: 7rem;
    color: var(--muted, #8b93a1);
  }
</style>
