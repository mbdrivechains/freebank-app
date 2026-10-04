<script lang="ts">
  // "Keep FreeBank's node running after I close the app" (Settings; off by default). On, the node
  // starts in its own session and stays when the app closes, and the next launch manages it again.
  // Off, the app stops its node when it closes. A node that started before the setting was turned on
  // would still stop with the app on Linux: a restart fixes that, and this card offers it.
  import { onMount } from "svelte";
  import { node, type NodeStatus } from "../lib/node";
  import { phone } from "../lib/phone";

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
  // The phone's own "keep connected" (Settings › Phone), said here too, so both answers to "when FreeBank closes" are
  // in one place.
  let phoneKeep = false;
  onMount(() => {
    load();
    phone.keepInfo().then((k) => (phoneKeep = !!k?.keep)).catch(() => {});
  });

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
    await load();
    // A node started before this was on would still stop with the app: restart it now rather than ask
    // (v0.2.6, the walk-through: it said to restart it yourself).
    if (needsRestart) await restart();
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
      Your node keeps running in the background and stays in sync. Open FreeBank to stop it. Your phone reaches it while
      FreeBank is open, or always, with "Keep my phone connected when FreeBank is closed" (Phone). Turning this off turns
      that off too.
    {:else}
      FreeBank stops its node when you close the app.
    {/if}
  </p>
  {#if restarting}
    <p class="hint">Restarting your node so it keeps running when you close FreeBank…</p>
  {:else if needsRestart}
    <p class="hint">Your node started before this was on, so it would still stop when you close FreeBank.</p>
    <button class="secondary" on:click={restart}>Restart the node</button>
  {/if}
  {#if phoneKeep}
    <p class="hint">Your phone stays connected when FreeBank is closed (Settings › Phone).</p>
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
