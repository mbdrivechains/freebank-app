<script lang="ts">
  // Home: the security checks' red items (lib/security.ts), until a check finds them fixed. The checks run
  // again each time Home opens. "How to fix" dispatches `open`; App.svelte shows Settings, whose Security
  // card scrolls into view with the details.
  import { createEventDispatcher, onMount } from "svelte";
  import { focusSecurity, runSecurityCheck, securityReds } from "../lib/security";
  import { bannerAsksPassphrase, passphraseChanged, protection } from "../lib/walletSeed";

  const dispatch = createEventDispatcher<{ open: void }>();

  // The wallet-passphrase item defers to the wallet's banner above, which says the same with a button
  // that fixes it (v0.2.0 wallet). Settings > Security still lists it.
  $: reds = $securityReds.filter((r) => !(r.id === "wallet" && $bannerAsksPassphrase));

  onMount(() => {
    runSecurityCheck();
  });

  // Check again as soon as the wallet gets (or changes) its passphrase, so Home never shows a fixed item
  // as still red (seen on beta, 2026-09-29: "Your wallet has no passphrase" right after protecting it).
  let seenEncrypted: boolean | undefined;
  let seenChanges = $passphraseChanged;
  $: if ($protection && $protection.encrypted !== seenEncrypted) {
    if (seenEncrypted !== undefined) runSecurityCheck();
    seenEncrypted = $protection.encrypted;
  }
  $: if ($passphraseChanged !== seenChanges) {
    seenChanges = $passphraseChanged;
    runSecurityCheck();
  }

  function open() {
    focusSecurity.set(true);
    dispatch("open");
  }
</script>

{#if reds.length}
  <div class="sec-alert" role="alert">
    <div class="sec-alert-head">
      Security: {reds.length === 1 ? "one thing" : `${reds.length} things`} to fix
    </div>
    <ul>
      {#each reds as r (r.id)}
        <li>{r.title}</li>
      {/each}
    </ul>
    <button class="secondary" on:click={open}>How to fix</button>
  </div>
{/if}

<style>
  .sec-alert {
    margin-bottom: 14px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(229, 116, 106, 0.35);
    border-left: 3px solid var(--error-color);
    background: rgba(229, 116, 106, 0.08);
    color: #f0c4be;
    font-size: 13.5px;
  }
  .sec-alert-head {
    font-weight: 650;
    color: var(--error-color);
  }
  ul {
    margin: 6px 0 10px 18px;
    line-height: 1.5;
  }
  button {
    padding: 7px 14px;
    font-size: 13px;
    color: var(--text-color);
    border-color: rgba(229, 116, 106, 0.45);
  }
</style>
