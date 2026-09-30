<script lang="ts">
  // Closing the window with "Keep FreeBank's node running after I close the app" on: the app holds
  // the first close (node/background.rs) and this says what happens to the node. "Close FreeBank"
  // leaves it running; "Stop the node and close" stops it first. Closing the window again within a
  // minute closes it without asking.
  import { onDestroy, onMount } from "svelte";
  import { node, onQuitRequested, type QuitAsk } from "../lib/node";
  import { phone } from "../lib/phone";
  import { nice } from "../lib/errors";

  let ask: QuitAsk | null = null;
  let busy: "" | "close" | "stop" | "phone" = "";
  let error = "";
  let off: (() => void) | null = null;

  onMount(async () => {
    try {
      off = await onQuitRequested((a) => {
        if (typeof a?.outlives !== "boolean") return;
        ask = a;
        error = "";
        // Asked again: whatever was under way didn't close the app.
        busy = "";
      });
    } catch {
      off = null;
    }
  });
  onDestroy(() => off?.());

  async function close() {
    busy = "close";
    error = "";
    try {
      await node.quit();
    } catch (e) {
      error = nice(e);
      busy = "";
    }
  }

  async function stopAndClose() {
    busy = "stop";
    error = "";
    try {
      await node.stop();
      await node.quit();
    } catch (e) {
      error = nice(e);
      busy = "";
    }
  }

  // "Keep your phone connected when FreeBank is closed": a background part takes the phone link over.
  async function keepPhone() {
    busy = "phone";
    error = "";
    try {
      await phone.keepConnectedQuit();
    } catch (e) {
      error = nice(e);
      busy = "";
    }
  }

  function stay() {
    if (!busy) ask = null;
  }
</script>

<svelte:window on:keydown={(e) => e.key === "Escape" && ask && stay()} />

{#if ask}
  <div class="quit-back">
    <div class="quit card" role="dialog" aria-modal="true" aria-labelledby="quit-title">
      {#if ask.outlives && ask.phone}
        <h3 id="quit-title">Close FreeBank</h3>
        <p>
          Your node keeps running, and a small background part of FreeBank keeps your phone connected while this
          computer is on and awake. Opening FreeBank takes the phone back; "Stop everything" stops it all.
        </p>
        {#if ask.phone_send}
          <p class="muted small">
            Phone sends keep working within each phone's limit: your wallet passphrase stays in that part's memory (never
            on disk) until you open FreeBank again.
          </p>
        {/if}
      {:else if ask.outlives}
        <h3 id="quit-title">Close FreeBank</h3>
        <p>The node keeps running. Open FreeBank to stop it.</p>
      {:else}
        <h3 id="quit-title">Your node stops this time</h3>
        <p>
          Your node started before "Keep FreeBank's node running" was on, so it stops when FreeBank closes{ask.phone
            ? ", and your phone disconnects"
            : ""}. From its next start it keeps running after you close the app.
        </p>
      {/if}
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      <div class="row-actions">
        {#if ask.outlives && ask.phone}
          <button on:click={keepPhone} disabled={!!busy}>{busy === "phone" ? "Closing…" : "Keep the phone connected"}</button>
          <button class="secondary" on:click={stopAndClose} disabled={!!busy}>
            {busy === "stop" ? "Stopping the node…" : "Stop everything and close"}
          </button>
        {:else}
          <button on:click={close} disabled={!!busy}>{busy === "close" ? "Closing…" : "Close FreeBank"}</button>
          {#if ask.outlives}
            <button class="secondary" on:click={stopAndClose} disabled={!!busy}>
              {busy === "stop" ? "Stopping the node…" : "Stop the node and close"}
            </button>
          {/if}
        {/if}
      </div>
      <button class="link-btn" on:click={stay} disabled={!!busy}>Keep FreeBank open</button>
    </div>
  </div>
{/if}

<style>
  .quit-back {
    position: fixed;
    inset: 0;
    z-index: 70; /* above everything, the unlock prompt (60) included: the window is closing */
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .quit {
    width: 100%;
    max-width: 400px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .quit h3,
  .quit p {
    margin: 0;
  }
  .quit p {
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .quit .link-btn {
    align-self: flex-start;
  }
</style>
