<script lang="ts">
  // Settings for the node on this computer: its connection (Advanced, with Test connection), and
  // the two deliberate actions, "Delete chain data" and "Remove FreeBank". Each asks first.
  import { createEventDispatcher, onMount } from "svelte";
  import AdvancedSettings from "./AdvancedSettings.svelte";
  import { node, type NodeStatus, type Removed, type SetupInfo } from "../lib/node";

  const dispatch = createEventDispatcher<{ removed: Removed }>();

  let info: SetupInfo | null = null;
  let st: NodeStatus | null = null;
  let loadError = "";

  async function load() {
    try {
      [info, st] = await Promise.all([node.setupInfo(), node.status()]);
      loadError = "";
    } catch (e) {
      loadError = String(e);
    }
  }
  onMount(load);

  $: external = !!st && !st.managed && st.state !== "down" && st.state !== "busy";
  $: lockedReason = st?.managed
    ? "Stop the node on the Node tab to change these."
    : external
      ? "A FreeBank node started by another program is running; stop it there to change these."
      : "";

  let confirm: "wipe" | "remove" | null = null;
  let busy = false;
  let error = "";
  let done = "";

  async function wipe() {
    busy = true;
    error = "";
    try {
      await node.deleteChainData();
      done = "Chain data deleted. Your node is starting again and will download the chain from its peers; the Node tab shows how far it has got.";
      confirm = null;
    } catch (e) {
      error = String(e);
    }
    busy = false;
    load();
  }

  async function remove() {
    busy = true;
    error = "";
    try {
      const r = await node.removePrograms();
      confirm = null;
      dispatch("removed", r);
    } catch (e) {
      error = String(e);
    }
    busy = false;
  }
</script>

{#if loadError}
  <p class="soft-error">{loadError}</p>
{/if}

{#if info && st}
  <div class="card">
    <h3>Connection</h3>
    <p class="muted small">Where your FreeBank node finds eCash beta, and where it keeps its data.</p>
    <div class="settings-adv">
      <AdvancedSettings
        settings={info.settings}
        defaultDatadir={info.default_datadir}
        open
        {lockedReason}
        saveLabel="Save"
        on:saved={load}
      />
    </div>
    <p class="hint">Saved changes take effect the next time FreeBank starts.</p>
  </div>

  <div class="card">
    <h3>Start over</h3>

    <div class="maint">
      <div class="maint-text">
        <strong>Delete chain data</strong>
        <span class="muted small">Removes the downloaded blocks and chain state, then downloads them again. Your wallet stays.</span>
      </div>
      <button class="secondary" on:click={() => { confirm = "wipe"; error = ""; done = ""; }} disabled={busy || confirm === "wipe"}>Delete…</button>
    </div>
    {#if confirm === "wipe"}
      <div class="confirm-box">
        <p>
          This stops your node and deletes <code>blocks</code>, <code>chainstate</code> and <code>indexes</code> in
          <span class="path">{st.datadir}</span>
          Your wallet (<code>wallet.dat</code>) and <code>freebank.conf</code> stay. Your node then starts again and re-syncs,
          which takes a while.
        </p>
        {#if external}<p class="hint">A node started by another program is running; stop it there first.</p>{/if}
        <div class="row-actions">
          <button on:click={wipe} disabled={busy || external}>{busy ? "Deleting…" : "Delete and re-sync"}</button>
          <button class="secondary" on:click={() => (confirm = null)} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}
    {#if done}<p class="hint ok-note">{done}</p>{/if}

    <div class="maint">
      <div class="maint-text">
        <strong>Remove FreeBank</strong>
        <span class="muted small">Stops the node and removes the programs this app downloaded. Your data folder and wallet stay.</span>
      </div>
      <button class="secondary" on:click={() => { confirm = "remove"; error = ""; done = ""; }} disabled={busy || confirm === "remove"}>Remove…</button>
    </div>
    {#if confirm === "remove"}
      <div class="confirm-box">
        <p>
          This stops your node and removes the FreeBank node program and grpcurl from the app's own folder.
          Nothing in <span class="path">{st.datadir}</span> is touched, so your wallet and settings stay.
          You can install FreeBank again at any time.
        </p>
        <div class="row-actions">
          <button on:click={remove} disabled={busy}>{busy ? "Removing…" : "Remove"}</button>
          <button class="secondary" on:click={() => (confirm = null)} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if error}<p class="soft-error">{error}</p>{/if}
  </div>
{/if}
