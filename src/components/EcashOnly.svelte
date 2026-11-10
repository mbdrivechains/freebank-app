<script lang="ts">
  // v0.7.5: a network where FreeBank isn't open (eCash's mainnet for now): only the eCash wallet works. No FreeBank node
  // is installed or started. The eCash node is the one the user names here (the eCash login form, as in Settings ›
  // Node & connection); no node is built in (legal, 11-09): with nothing typed the form looks on this computer, as it
  // does on every network. The wallet is the sealed recovery words: while there are none, WalletGate's flow opens by
  // itself, and while they're unchecked a card offers them again. The network switch is here too, to go back.
  import EcashPanel from "./EcashPanel.svelte";
  import EcashLogin from "./EcashLogin.svelte";
  import NetworkCard from "./NetworkCard.svelte";
  import { openWalletFlow, protection } from "../lib/walletSeed";

  let nodeOpen = false;
  let panelKey = 0;
</script>

<div class="card ecash-only" data-testid="ecash-only">
  <p><strong>FreeBank isn't open on eCash's mainnet yet.</strong> Only the eCash wallet works here.</p>
</div>

{#if $protection && !$protection.protected}
  <div class="card">
    <h3>Your wallet</h3>
    {#if $protection.seed_file_problem}
      <p class="soft-error">This network's saved recovery words can't be read ({$protection.seed_file_problem}). Setting
        the wallet up again keeps that file aside, not deleted.</p>
    {/if}
    <p class="muted">Set up the wallet first: a passphrase, then new recovery words or ones you have.</p>
    <button on:click={() => openWalletFlow({ kind: "protect", firstRun: true })}>Set up the wallet</button>
  </div>
{:else}
  {#if $protection && !$protection.words_confirmed}
    <!-- The words were never checked (the app closed while they showed, say): see them again behind the passphrase. -->
    <div class="card">
      <h3>Your recovery words</h3>
      <p class="muted">You haven't checked your recovery words for this wallet yet. Without them nobody can bring it back.</p>
      <button on:click={() => openWalletFlow({ kind: "confirm-words" })}>See and check them</button>
    </div>
  {/if}
  {#key panelKey}<EcashPanel on:settings={() => (nodeOpen = true)} />{/key}
{/if}

<details class="advanced" bind:open={nodeOpen}>
  <summary>The eCash node, and the network</summary>
  <!-- Legal (11-09, L5e): say where the app connects when the field is empty. Any node is checked for eCash mainnet's
       pinned block before the wallet uses it (ecash/conn.rs), so something else listening there is refused. -->
  <p class="hint">Name an eCash mainnet node you run or can use, with its login. It sees your wallet's addresses and
    balances. Left empty, the app looks for an eCash node on this computer. Use a private network or VPN for one on
    another computer.</p>
  <EcashLogin on:saved={() => (panelKey += 1)} />
  <NetworkCard />
</details>

<style>
  .ecash-only p {
    margin: 0;
  }
</style>
