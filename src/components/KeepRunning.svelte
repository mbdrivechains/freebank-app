<script lang="ts">
  // "Keep FreeBank's node running after I close the app" (Settings; off by default). On, the node
  // starts in its own session and stays when the app closes, and the next launch manages it again.
  // Off, the app stops its node when it closes. A node that started before the setting was turned on
  // would still stop with the app on Linux: a restart fixes that, and this card offers it.
  import { onMount } from "svelte";
  import { node, type NodeStatus } from "../lib/node";

  export let keepRunning = false;

  let st: NodeStatus | null = null;
  let saving = false;
  let restarting = false;
  let error = "";

  async function load() {
    try {
      st = await node.status();
    } catch {
      st = null;
    }
  }
  onMount(load);

  async function toggle(e: Event) {
    const on = (e.target as HTMLInputElement).checked;
    saving = true;
    error = "";
    try {
      keepRunning = (await node.setKeepRunning(on)).keep_running;
    } catch (err) {
      error = String(err);
      keepRunning = !on;
    }
    saving = false;
    load();
  }

  async function restart() {
    restarting = true;
    error = "";
    try {
      await node.restart();
    } catch (err) {
      error = String(err);
    }
    restarting = false;
    load();
  }

  $: needsRestart = keepRunning && !!st && st.managed && !st.keeps_running && st.state !== "busy";
</script>

<div class="card">
  <h3>When you close FreeBank</h3>
  <label class="keep-row">
    <input type="checkbox" checked={keepRunning} on:change={toggle} disabled={saving} />
    <span>Keep FreeBank's node running after I close the app</span>
  </label>
  <p class="hint">
    {#if keepRunning}
      Your node keeps running in the background and stays in sync. Open FreeBank to stop it. Your phone can reach it only
      while FreeBank is open.
    {:else}
      FreeBank stops its node when you close the app.
    {/if}
  </p>
  {#if needsRestart}
    <p class="hint">Your node started before this was on, so it would still stop when you close FreeBank. Restart it to keep it running.</p>
    <button class="secondary" on:click={restart} disabled={restarting}>{restarting ? "Restarting…" : "Restart the node"}</button>
  {/if}
  {#if error}<p class="soft-error">{error}</p>{/if}
</div>

<style>
  .keep-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    margin: 10px 0 6px;
    font-size: 14px;
    cursor: pointer;
  }
  .keep-row input {
    margin-top: 3px;
    flex: none;
  }
</style>
