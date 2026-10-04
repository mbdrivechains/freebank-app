<script lang="ts">
  // Settings › Wallet: your wallets (v0.2.6; operator, 2026-10-04: "Both kinds", the phone on the main one). The main
  // wallet, wallets made from your same recovery words (each brought back by the words and its number), and wallet
  // files you already have (each with its own passphrase and backup). The header's switcher chooses which one Home,
  // Send, Receive and Credit use.
  import { onMount } from "svelte";
  import { fmtEcx } from "../lib/amount";
  import { BASE_TICKER } from "../lib/brand";
  import { loadWallets, walletAddFile, walletAddWords, walletForget, walletList } from "../lib/wallets";
  import { node } from "../lib/node";

  /** The node runs under another program (BitWindow's FreeBank, say): it must be started again there. */
  let external = false;

  let adding: "" | "words" | "file" = "";
  let label = "";
  let pass = "";
  let fileData = "";
  let fileName = "";
  let busy = false;
  let error = "";
  let note = "";
  /** The wallet whose Remove was pressed, waiting for "Remove" again. */
  let removing: string | null = null;
  let fileInput: HTMLInputElement;

  onMount(() => {
    loadWallets().catch((e) => (error = String(e)));
    node
      .status()
      .then((st) => (external = !st.managed && st.state !== "down" && st.state !== "busy"))
      .catch(() => {});
  });

  const say = (e: unknown) => String(e).replace(/^Error: /, "");
  const KIND: Record<string, string> = { main: "your recovery words", words: "your recovery words", file: "a wallet file" };

  function open(k: "words" | "file") {
    adding = adding === k ? "" : k;
    label = "";
    pass = "";
    fileData = "";
    fileName = "";
    error = "";
    note = "";
  }

  function pick(e: Event) {
    const f = (e.target as HTMLInputElement).files?.[0];
    if (!f) return;
    fileName = f.name;
    const r = new FileReader();
    r.onload = () => {
      const bytes = new Uint8Array(r.result as ArrayBuffer);
      let bin = "";
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
      fileData = btoa(bin);
    };
    r.readAsArrayBuffer(f);
  }

  async function add() {
    busy = true;
    error = "";
    note = "";
    try {
      if (adding === "words") {
        await walletAddWords(label, pass);
        note = `${label} is ready. Choose it in the header to use it.`;
      } else {
        await walletAddFile(label, fileData);
        note = `${label} is open. Choose it in the header to use it; back it up on its own, as your words don't cover it.`;
      }
      adding = "";
    } catch (e) {
      error = say(e);
    }
    pass = "";
    busy = false;
  }

  async function forget(name: string, l: string) {
    error = "";
    note = "";
    removing = null;
    try {
      await walletForget(name);
      note = `${l} is off the list. It stays open until your node restarts, and its file stays where it is.`;
    } catch (e) {
      error = say(e);
    }
  }
</script>

<div class="card" data-testid="wallets">
  <h3>Your wallets</h3>
  <ul class="wl-list">
    {#each $walletList as w (w.name ?? "main")}
      <li>
        <div>
          <strong>{w.label}{#if w.active && $walletList.length > 1}<span class="wl-use"> · in use</span>{/if}</strong>
          <span class="muted small">from {KIND[w.kind]}{w.encrypted === false ? " · no passphrase" : ""}</span>
        </div>
        <span class="wl-bal">{w.balance === null ? "—" : `${fmtEcx(Math.round(w.balance * 1e8))} ${BASE_TICKER}`}</span>
        <span class="wl-act">
          {#if w.name && removing !== w.name}
            <button class="link-btn inline" on:click={() => (removing = w.name)}>Remove</button>
          {/if}
        </span>
        {#if w.name && removing === w.name}
          <p class="small wl-confirm">
            Take {w.label} off the list? Its file stays where it is, and you can open it again.
            <button class="link-btn inline" on:click={() => w.name && forget(w.name, w.label)}>Remove</button>
            <button class="link-btn inline" on:click={() => (removing = null)}>Keep</button>
          </p>
        {/if}
      </li>
    {/each}
  </ul>
  {#if note}<p class="hint ok-note">{note}</p>{/if}
  {#if error}<p class="soft-error">{error}</p>{/if}

  <div class="row-actions">
    <button class="secondary" on:click={() => open("words")}>Add one from my words</button>
    <button class="secondary" on:click={() => open("file")}>Open a wallet file</button>
  </div>

  {#if adding}
    <form class="form wl-add" on:submit|preventDefault={add}>
      <label>
        Name
        <input type="text" bind:value={label} maxlength="32" placeholder={adding === "words" ? "e.g. Savings" : "e.g. Old BitWindow wallet"} />
      </label>
      {#if adding === "words"}
        <p class="hint">
          A new wallet from your same recovery words, with your wallet passphrase.
          {#if external}
            Your node stops once to finish it: it was started by another program, so start it again there, and FreeBank
            carries on.
          {:else}
            FreeBank restarts its node once to finish it (a minute or two).
          {/if}
        </p>
        <label>
          Wallet passphrase
          <input type="password" bind:value={pass} autocomplete="current-password" />
        </label>
      {:else}
        <p class="hint">
          A FreeBank wallet file (a backup, BitWindow's FreeBank, another node). FreeBank copies it into your node's wallet
          folder and opens it there; it keeps its own passphrase, and your words don't cover it.
        </p>
        <input class="wl-file" type="file" bind:this={fileInput} on:change={pick} />
        <div class="row-actions">
          <button type="button" class="secondary" on:click={() => fileInput.click()}>{fileName ? "Choose another file" : "Choose the file…"}</button>
          {#if fileName}<span class="muted small">{fileName}</span>{/if}
        </div>
      {/if}
      <div class="row-actions">
        <button type="button" class="secondary" on:click={() => (adding = "")}>Cancel</button>
        <button type="submit" disabled={busy || !label.trim() || (adding === "words" ? !pass : !fileData)}>
          {busy ? (adding === "words" ? (external ? "Adding… start your node again when it stops" : "Adding… your node restarts") : "Opening…") : "Add"}
        </button>
      </div>
    </form>
  {/if}
</div>

<style>
  .wl-list {
    list-style: none;
    padding: 0;
    margin: 8px 0 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .wl-list li {
    display: grid;
    grid-template-columns: 1fr auto 4.5em;
    align-items: baseline;
    gap: 10px;
  }
  .wl-list li > div {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .wl-bal {
    font-variant-numeric: tabular-nums;
  }
  .wl-act {
    text-align: right;
  }
  .wl-use {
    font-weight: normal;
    color: var(--text-secondary);
  }
  .wl-confirm {
    grid-column: 1 / -1;
    margin: 0;
  }
  .wl-file {
    display: none;
  }
  .wl-add {
    margin-top: 12px;
  }
</style>
