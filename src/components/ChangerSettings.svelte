<script lang="ts">
  // Settings › Node & connection: the money changer (v0.3.0): its address and its key, pinned. Every quote must carry
  // that key's signature. Both empty: no changer, and its cards don't show in Deposit and Withdraw.
  import { onMount } from "svelte";
  import { changerGet, changerInfo, changerSet } from "../lib/inout";

  let url = "";
  let key = "";
  let busy = false;
  let result = "";
  let ok = false;

  onMount(async () => {
    try {
      ({ url, key } = await changerGet());
    } catch (e) {
      result = String(e);
    }
  });

  async function save() {
    busy = true;
    result = "";
    try {
      await changerSet(url, key);
      if (!url.trim()) {
        ok = true;
        result = "No changer: its cards are hidden.";
      } else {
        const info = await changerInfo();
        ok = !!info && !info.paused;
        result = !info ? "Saved." : info.paused ? `Saved; the changer is paused: ${info.paused}` : "Saved; the changer answers with this key.";
      }
    } catch (e) {
      ok = false;
      result = String(e).replace(/^Error: /, "");
    }
    busy = false;
  }
</script>

<div class="card" data-testid="changer-settings">
  <h3>Money changer</h3>
  <p class="muted small">
    A changer sells you FreeBank ECX for eCash, or buys it, in a few blocks, from a float it keeps. You trust it with one
    order at a time. FreeBank takes only quotes signed with the key here. Leave both empty for none.
  </p>
  <form class="form" on:submit|preventDefault={save}>
    <label>
      Address
      <input type="text" bind:value={url} placeholder="https://…" spellcheck="false" autocomplete="off" />
    </label>
    <label>
      Its key
      <input type="text" bind:value={key} placeholder="64 hex characters" spellcheck="false" autocomplete="off" />
    </label>
    <button type="submit" disabled={busy}>{busy ? "Checking…" : "Save and check"}</button>
  </form>
  {#if result}<p class={ok ? "hint ok-note" : "soft-error"}>{result}</p>{/if}
</div>
