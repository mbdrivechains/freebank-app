<script lang="ts">
  // Settings › Wallet (v0.6.8): the addresses this wallet has given out, FreeBank's and eCash's, newest first: when, to
  // whom (this screen or a paired phone), what each has received, and a note of what it is for. Those given out before
  // FreeBank kept this list come from the wallets' own address books, without a date.
  import { onMount } from "svelte";
  import { BASE_TICKER } from "../lib/brand";
  import { given, NOTE_MAX, type GivenRow } from "../lib/given";
  import { when } from "../lib/walletSeed";

  let rows: GivenRow[] | null = null;
  let error = "";
  let shown = 10;
  let editing: string | null = null;
  let draft = "";
  let copied = "";

  async function load() {
    try {
      rows = await given.list();
      error = "";
    } catch (e) {
      error = String(e);
    }
  }
  onMount(load);

  async function saveNote(r: GivenRow) {
    try {
      await given.note(r.address, r.kind === "ecash" ? "ecash" : "freebank", draft);
      editing = null;
      await load();
    } catch (e) {
      error = String(e);
    }
  }
  async function copy(a: string) {
    await navigator.clipboard.writeText(a);
    copied = a;
  }
  const coin = (r: GivenRow) => (r.kind === "ecash" ? "ECX" : BASE_TICKER);
  const from = (r: GivenRow) =>
    r.at === 0 ? "given out before FreeBank kept this list" : `${when(r.at)}${r.via && r.via !== "app" ? `, to ${r.via.replace(/^phone: /, "your phone ")}` : ""}`;
</script>

<div class="card" data-testid="given-addresses">
  <h3>Addresses you gave out</h3>
  <p class="muted small">
    Each address Receive handed out, here or to a paired phone, with a note of what it is for if you add one. FreeBank
    addresses are your FreeBank wallet's; eCash ones your eCash wallet's.
  </p>
  {#if error}<p class="soft-error">{error}</p>{/if}
  {#if rows === null && !error}
    <p class="muted small">Looking…</p>
  {:else if rows && rows.length === 0}
    <p class="muted small">None yet: press Receive on Home, or in the eCash tab.</p>
  {:else if rows}
    <ul class="given">
      {#each rows.slice(0, shown) as r (r.kind + r.address)}
        <li>
          <div class="given-top">
            <span class="pill">{r.kind === "ecash" ? "eCash" : "FreeBank"}</span>
            <code class="given-addr">{r.address}</code>
            <button class="link-btn inline" on:click={() => copy(r.address)}>{copied === r.address ? "Copied" : "Copy"}</button>
          </div>
          <div class="muted small">
            {from(r)}{r.wallet ? ` · wallet "${r.wallet}"` : ""}{r.received !== null ? ` · received ${r.received} ${coin(r)}` : ""}
          </div>
          {#if editing === r.address}
            <form class="given-note" on:submit|preventDefault={() => saveNote(r)}>
              <input type="text" bind:value={draft} maxlength={NOTE_MAX} placeholder="What is it for?" />
              <button class="secondary" type="submit">Save</button>
              <button class="link-btn inline" type="button" on:click={() => (editing = null)}>Cancel</button>
            </form>
          {:else}
            <div class="small">
              {#if r.note}<span>{r.note}</span>{/if}
              <button class="link-btn inline" on:click={() => { editing = r.address; draft = r.note; }}>{r.note ? "Change note" : "Add a note"}</button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
    {#if rows.length > shown}
      <button class="link-btn" on:click={() => (shown += 20)}>Show more ({rows.length - shown} more)</button>
    {/if}
  {/if}
</div>

<style>
  .given {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
  }
  .given li {
    padding: 10px 0;
    border-top: 1px solid var(--border-color);
  }
  .given-top {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .given-addr {
    overflow-wrap: anywhere;
    font-size: 12.5px;
    min-width: 0;
  }
  .given-note {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }
  .given-note input {
    flex: 1;
    min-width: 0;
  }
</style>
