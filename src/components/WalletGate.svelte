<script lang="ts">
  // Mounted once at the root, desktop only:
  //   <WalletGate {connected} {localNode} />
  // When the app connects to its own node it asks whether the wallet is protected. A new wallet that
  // isn't gets the flow straight away: the passphrase first, before any address is shown. An older
  // wallet without protection gets the Home banner instead (WalletBanner). It shows whichever flow
  // $walletFlow asks for, and passes the app's "wallet-passphrase-changed" on as $passphraseChanged.
  import { onDestroy, onMount } from "svelte";
  import WalletFlow from "./WalletFlow.svelte";
  import {
    closeWalletFlow,
    loadProtection,
    openWalletFlow,
    passphraseChanged,
    protection,
    walletFlow,
  } from "../lib/walletSeed";

  export let connected = false;
  export let localNode = false;

  // Opened by itself once per connection, so "Not now" sticks until the app connects again.
  let offered = false;

  $: watchConnection(connected && localNode);
  function watchConnection(on: boolean) {
    if (on) {
      check();
    } else {
      offered = false;
      protection.set(null);
    }
  }

  async function check() {
    const p = await loadProtection();
    if (p && !p.protected && p.new_wallet && !offered && !$walletFlow) {
      offered = true;
      openWalletFlow({ kind: "protect", firstRun: true });
    }
  }

  let unlisten: (() => void) | null = null;
  onMount(async () => {
    try {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen("wallet-passphrase-changed", () => passphraseChanged.update((n) => n + 1));
    } catch {
      // Not in the desktop app.
    }
  });
  onDestroy(() => unlisten?.());
</script>

{#if $walletFlow}
  <WalletFlow request={$walletFlow} on:close={closeWalletFlow} />
{/if}
