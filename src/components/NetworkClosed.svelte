<script lang="ts">
  // A network that has closed (v0.6.8: beta, on 26 October 2026; network.rs `closed`), said plainly on Home, with the
  // way on. Setup says the same in its own words. The wallet and its files stay on this computer.
  import { network, switchNetwork } from "../lib/network";

  let busy = false;
  let error = "";

  async function toEasynet() {
    busy = true;
    error = "";
    try {
      await switchNetwork("easynet"); // the app restarts
    } catch (e) {
      error = String(e);
      busy = false;
    }
  }
</script>

{#if $network.closed}
  <div class="card closed" data-testid="network-closed">
    <h3>FreeBank's {$network.name.toLowerCase()} has closed</h3>
    <p>
      It closed on {$network.closed}: its seed node, houses and block makers are off, so it gets no new FreeBank blocks
      and nothing sent on it will confirm. Your {$network.name.toLowerCase()} wallet stays on this computer, with its
      words.
    </p>
    <p>
      FreeBank's test network is now <strong>easynet</strong>. FreeBank sets it all up itself in a few minutes, and its
      faucet gives you test coins.
    </p>
    <div class="row-actions">
      <button on:click={toEasynet} disabled={busy}>{busy ? "Switching…" : "Switch to easynet"}</button>
    </div>
    {#if error}<p class="soft-error">{error}</p>{/if}
  </div>
{/if}

<style>
  .closed {
    border-color: var(--accent-color);
  }
</style>
