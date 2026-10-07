<script lang="ts">
  // "Get FreeBank updates with Software Updater" (v0.2.9, opt in): only for the .deb without FreeBank's apt repository.
  // Yes writes the apt source through the computer's own password prompt; nothing is added without it. `ask` is the
  // one-time question on Home; without it, the line in Settings > App updates.
  import { aptAsked, aptOffer, enableAptUpdates, setAptAsked } from "../lib/appUpdate";

  export let ask = false;

  let busy = false;
  let done = false;
  let error = "";
  let dismissed = ask && aptAsked();

  async function yes() {
    busy = true;
    error = "";
    try {
      await enableAptUpdates();
      done = true;
      setAptAsked();
    } catch (e) {
      error = String(e);
    }
    busy = false;
  }

  function notNow() {
    setAptAsked();
    dismissed = true;
  }
</script>

{#if done}
  <div class="card" class:apt-ask={ask}>
    <p class="ok-note">Done. Software Updater offers new versions of FreeBank from now on.</p>
    {#if ask}<div class="row-actions"><button class="ghost" on:click={() => (dismissed = done = false)}>OK</button></div>{/if}
  </div>
{:else if $aptOffer && !dismissed}
  <div class:card={ask} class:apt-ask={ask}>
    {#if ask}<h2>Updates with Software Updater?</h2>{/if}
    <p class="hint">
      FreeBank can come with your computer's other updates, from FreeBank's apt repository: signed, and checked against
      the key this package installed. Your computer asks for your password once.
    </p>
    <div class="row-actions">
      <button on:click={yes} disabled={busy}>{busy ? "Waiting for your password…" : "Get updates with Software Updater"}</button>
      {#if ask}<button class="ghost" on:click={notNow} disabled={busy}>Not now</button>{/if}
    </div>
    {#if error}<p class="soft-error">{error}</p>{/if}
    {#if ask}<p class="muted small">You can turn it on later in Settings › App updates.</p>{/if}
  </div>
{/if}

<style>
  .apt-ask {
    border-color: var(--accent-color);
  }
  .apt-ask p {
    margin: 0 0 8px;
  }
</style>
