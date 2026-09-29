<script lang="ts">
  // Asks for the wallet passphrase, over any screen. It only shows and collects: the parent decides
  // what the passphrase is for. App.svelte mounts one for withUnlock (lib/wallet.ts):
  //   <UnlockPrompt what={$unlockRequest.what} error={$unlockRequest.error} busy={$unlockRequest.busy}
  //     on:submit={(e) => submitUnlock(e.detail)} on:cancel={cancelUnlock} />
  // The field is cleared as soon as the passphrase is handed over, and when the prompt closes.
  import { createEventDispatcher, onDestroy, onMount, tick } from "svelte";

  /** Completes "Enter your wallet passphrase to …". */
  export let what = "sign this transaction";
  /** Shown under the field, e.g. after a wrong passphrase. */
  export let error = "";
  /** An unlock is in flight: the buttons wait. */
  export let busy = false;
  export let title = "Unlock your wallet";
  export let note = "It unlocks for a few seconds, just for this, then locks again.";
  export let submitLabel = "Unlock";

  const dispatch = createEventDispatcher<{ submit: string; cancel: void }>();

  let passphrase = "";
  let input: HTMLInputElement;

  function submit() {
    if (!passphrase || busy) return;
    const p = passphrase;
    passphrase = "";
    dispatch("submit", p);
  }

  function cancel() {
    passphrase = "";
    dispatch("cancel");
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !busy) cancel();
  }

  onMount(() => input?.focus());
  onDestroy(() => (passphrase = ""));

  // After a wrong passphrase the field is empty again: put the cursor back in it.
  $: if (error && !busy) tick().then(() => input?.focus());
</script>

<svelte:window on:keydown={onKey} />

<div class="unlock-back">
  <form
    class="unlock card"
    role="dialog"
    aria-modal="true"
    aria-labelledby="unlock-title"
    aria-describedby="unlock-what"
    on:submit|preventDefault={submit}
  >
    <h3 id="unlock-title">{title}</h3>
    <p id="unlock-what" class="muted small">Enter your wallet passphrase to {what}. {note}</p>
    <label class="unlock-field">
      Passphrase
      <input
        bind:this={input}
        bind:value={passphrase}
        type="password"
        autocomplete="off"
        spellcheck="false"
        disabled={busy}
        aria-invalid={error ? "true" : undefined}
      />
    </label>
    {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
    <div class="row-actions">
      <button type="submit" disabled={busy || !passphrase}>{busy ? "Unlocking…" : submitLabel}</button>
      <button type="button" class="secondary" on:click={cancel} disabled={busy}>Cancel</button>
    </div>
  </form>
</div>

<style>
  .unlock-back {
    position: fixed;
    inset: 0;
    z-index: 60; /* above the phone's prompts (50), which can ask for an unlock */
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .unlock {
    width: 100%;
    max-width: 400px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .unlock h3 {
    margin: 0;
  }
  .unlock-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    color: var(--text-secondary);
  }
</style>
