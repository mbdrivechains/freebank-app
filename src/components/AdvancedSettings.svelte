<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { node, type ConnCheck, type Settings } from "../lib/node";

  export let settings: Settings;
  export let defaultDatadir = "";
  export let open = false;
  /** Why the fields can't be saved right now (e.g. the node is running); empty when they can. */
  export let lockedReason = "";
  export let saveLabel = "Save and check again";

  // saved: new settings; recheck: the saved addresses test fine, so the list above should look again; edited: typed
  // addresses differ from the saved ones (the list above shows the saved ones).
  const dispatch = createEventDispatcher<{ saved: Settings; recheck: void; edited: boolean }>();

  let rest = settings.rest;
  let enforcer = settings.enforcer;
  let datadir = settings.datadir;
  let rpcPort = settings.rpc_port;
  let p2pPort = settings.p2p_port;
  let saving = false;
  let problem = "";

  $: dirty =
    rest !== settings.rest ||
    enforcer !== settings.enforcer ||
    datadir !== settings.datadir ||
    Number(rpcPort) !== settings.rpc_port ||
    Number(p2pPort) !== settings.p2p_port;
  $: dispatch("edited", rest !== settings.rest || enforcer !== settings.enforcer);

  async function save() {
    saving = true;
    problem = "";
    try {
      const s = await node.saveSettings({
        rest,
        enforcer,
        datadir,
        rpc_port: Number(rpcPort),
        p2p_port: Number(p2pPort),
      });
      settings = s;
      rest = s.rest;
      enforcer = s.enforcer;
      dispatch("saved", s);
    } catch (e) {
      problem = String(e);
    }
    saving = false;
  }

  // Test the addresses as typed, saved or not.
  let testing = false;
  let checks: ConnCheck[] | null = null;
  async function test() {
    testing = true;
    checks = null;
    // The addresses as they were when the test started: a result for addresses edited since is dropped (the review
    // of v0.2.7: "These addresses work" appeared for ones never tested).
    const [r, e] = [rest, enforcer];
    let got: ConnCheck[];
    try {
      got = await node.testConnection(r, e);
    } catch (err) {
      got = [{ label: "Test", ok: false, detail: String(err) }];
    }
    testing = false;
    if (rest !== r || enforcer !== e) return;
    checks = got;
    // The saved addresses work now: the list above, from an earlier look, checks again (v0.2.7, the walk-through).
    if (allOk(got) && r === settings.rest && e === settings.enforcer) dispatch("recheck");
  }
  const allOk = (c: ConnCheck[] | null) => !!c && c.length > 0 && c.every((x) => x.ok);
  $: if (rest || enforcer) checks = null;

  function reset() {
    rest = "127.0.0.1:18302";
    enforcer = "127.0.0.1:50051";
    if (defaultDatadir) datadir = defaultDatadir;
    rpcPort = 8454;
    p2pPort = 8455;
  }
</script>

<details class="advanced" bind:open>
  <summary>Advanced</summary>
  <div class="adv-body">
    <label>
      eCash node REST
      <input type="text" bind:value={rest} spellcheck="false" placeholder="127.0.0.1:18302" />
    </label>
    <label>
      Enforcer
      <input type="text" bind:value={enforcer} spellcheck="false" placeholder="127.0.0.1:50051" />
    </label>
    <p class="hint">For an eCash node on another computer, connect over a private network or VPN only.</p>
    <div class="test-row">
      <button class="secondary" on:click={test} disabled={testing} type="button">
        {testing ? "Testing…" : "Test connection"}
      </button>
    </div>
    {#if checks}
      <div class="checklist small-list">
        {#each checks as c}
          <div class="check-item" class:ok={c.ok}>
            <span class="dot"></span>
            {c.label}
            <span class="check-state">{c.detail}</span>
          </div>
        {/each}
      </div>
      <!-- The list above the box reads the saved addresses (v0.2.6 and v0.2.7, the walk-throughs: Test connection went
           green while the checklist still said "not found", and Check again kept testing the old addresses). -->
      {#if allOk(checks) && !lockedReason}
        {#if rest !== settings.rest || enforcer !== settings.enforcer}
          <div class="use-these">
            <p class="hint ok-note">These addresses work. FreeBank still uses the saved ones until you use these.</p>
            <button on:click={save} disabled={saving} type="button">{saving ? "Saving…" : "Use these addresses"}</button>
          </div>
        {:else}
          <p class="hint ok-note">These are the addresses FreeBank uses, and they work.</p>
        {/if}
      {/if}
    {/if}
    <label>
      FreeBank data folder
      <input type="text" bind:value={datadir} spellcheck="false" />
    </label>
    <div class="adv-row">
      <label>
        RPC port
        <input type="number" bind:value={rpcPort} />
      </label>
      <label>
        Network port
        <input type="number" bind:value={p2pPort} />
      </label>
    </div>
    {#if problem}<p class="field-problem">{problem}</p>{/if}
    {#if lockedReason}<p class="hint">{lockedReason}</p>{/if}
    <div class="adv-actions">
      <button class="ghost" on:click={reset} type="button" disabled={!!lockedReason}>Defaults</button>
      <button class="secondary" on:click={save} disabled={!dirty || saving || !!lockedReason} type="button">
        {saving ? "Saving…" : saveLabel}
      </button>
    </div>
  </div>
</details>
