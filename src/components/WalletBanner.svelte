<script lang="ts">
  // On Home, the one thing the wallet needs next, until it is done (desktop only):
  //   no passphrase yet (every v0.1.1 install) → Protect my wallet
  //   FreeBank's recovery words don't cover it → Set up recovery words
  //   the words aren't written down and checked → Check my words
  //   coins still on older addresses → Move my coins
  //   no backup since the wallet got its seed → Back up now (this one can be put off)
  import { onMount } from "svelte";
  import PathText from "./PathText.svelte";
  import { api } from "../lib/api";
  import { fmtEcx } from "../lib/amount";
  import { BASE_TICKER } from "../lib/brand";
  import { nice } from "../lib/errors";
  import { bannerAsksPassphrase, loadProtection, openWalletFlow, protection, walletSeed, type MovePlan } from "../lib/walletSeed";

  const desktop = !api.isPWA();
  let plan: MovePlan | null = null;
  let backingUp = false;
  let saved: string[] = [];
  let backupError = "";
  let later = false;

  onMount(() => {
    if (desktop) loadProtection();
  });

  // Coins on older addresses matter once the wallet is protected; asked again whenever the state changes.
  $: if (desktop && $protection?.protected && $protection.words_confirmed) loadPlan();
  async function loadPlan() {
    plan = await walletSeed.movePlan().catch(() => null);
  }

  async function backup() {
    backingUp = true;
    backupError = "";
    try {
      saved = await walletSeed.backupNow();
      await loadProtection();
    } catch (e) {
      backupError = nice(e);
    }
    backingUp = false;
  }
</script>

{#if desktop && $protection}
  {@const p = $protection}
  {#if $bannerAsksPassphrase}
    <div class="wb wb-warn" role="alert">
      <strong>Your wallet has no passphrase</strong>
      <span>Anyone who can read this computer's files can spend its coins. Choose a passphrase and get your recovery words.</span>
      <button on:click={() => openWalletFlow({ kind: "protect" })}>Protect my wallet</button>
    </div>
  {:else if p.app_seed !== "matches"}
    <div class="wb wb-warn">
      <strong>Set up your recovery words</strong>
      <span>
        {p.app_seed === "other"
          ? "FreeBank's recovery words don't cover this wallet."
          : "This wallet has no recovery words yet."} Without them, losing this computer or the wallet file loses its coins.
      </span>
      <button on:click={() => openWalletFlow({ kind: "protect" })}>Set up recovery words</button>
    </div>
  {:else if !p.words_confirmed}
    <div class="wb">
      <strong>Check your recovery words</strong>
      <span>Make sure your paper copy is right: FreeBank shows the words again and asks for three of them.</span>
      <button on:click={() => openWalletFlow({ kind: "confirm-words" })}>Check my words</button>
    </div>
  {:else if plan && plan.coins > 0}
    <div class="wb">
      <strong>Move your coins to your recovery words</strong>
      <span>{fmtEcx(plan.total_sats)} {BASE_TICKER} sits on addresses from before your recovery words, which don't cover it.</span>
      <button on:click={() => openWalletFlow({ kind: "move" })}>Move my coins…</button>
    </div>
  {:else if p.backup_due && !later}
    <div class="wb wb-quiet">
      <strong>Back up your wallet</strong>
      <span>There's no backup since your wallet got its recovery words.</span>
      {#each saved as s}<span class="mono small">Saved to <PathText path={s} /></span>{/each}
      {#if backupError}<span class="soft-error">{backupError}</span>{/if}
      <div class="wb-actions">
        <button class="secondary" on:click={backup} disabled={backingUp}>{backingUp ? "Backing up…" : "Back up now"}</button>
        <button class="ghost" on:click={() => (later = true)} disabled={backingUp}>Later</button>
      </div>
    </div>
  {/if}
{/if}

<style>
  .wb {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 14px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(217, 164, 65, 0.35);
    background: var(--accent-tint);
    font-size: 13.5px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .wb strong {
    color: var(--text-color);
    font-size: 14.5px;
  }
  .wb-warn {
    border-color: rgba(229, 116, 106, 0.4);
    background: rgba(229, 116, 106, 0.08);
  }
  .wb-quiet {
    border-color: var(--border-color);
    background: var(--bg-secondary);
  }
  .wb button {
    align-self: flex-start;
    font-size: 13.5px;
    padding: 8px 16px;
  }
  .wb-actions {
    display: flex;
    gap: 8px;
  }
</style>
