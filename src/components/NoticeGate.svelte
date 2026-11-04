<script lang="ts">
  // The first-run notice (v0.7.0, legal's November 2026 texts): the standard notice, and on a test network that its
  // coins have no value. Shown over everything until "I understand", once per device and notice version; the answer
  // stays on this device (lib/notice.ts) and nothing is sent. The full notice stays in Settings › Help › About and legal.
  import { network } from "../lib/network";
  import { L3_TESTNET, agreeNotice, noticeAgreed, noticeN } from "../lib/notice";

  let open = !noticeAgreed();
  // easynet's FreeBank counts notes in grams from v1.1.0; mainnet's still counts ECX.
  $: paras = noticeN($network.id === "easynet");
  $: test = $network.id !== "mainnet";

  function agree() {
    agreeNotice();
    open = false;
  }
</script>

{#if open}
  <div class="notice-back">
    <div class="notice-gate card" role="dialog" aria-modal="true" aria-labelledby="notice-title" data-testid="notice-gate">
      <h3 id="notice-title">Before you use FreeBank</h3>
      {#each paras as p}<p>{p}</p>{/each}
      {#if test}<p class="test-net"><strong>{L3_TESTNET($network.name)}</strong></p>{/if}
      <p class="muted small">The full notice is in Settings › Help › About and legal. Your answer stays on this computer.</p>
      <div class="row-actions">
        <button on:click={agree} data-testid="notice-agree">I understand</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .notice-back {
    position: fixed;
    inset: 0;
    z-index: 65; /* over the app and the unlock prompt (60), under the closing dialog (70) */
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .notice-gate {
    width: 100%;
    max-width: 520px;
    max-height: calc(100vh - 32px);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .notice-gate h3,
  .notice-gate p {
    margin: 0;
  }
  .notice-gate p {
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .notice-gate .test-net {
    color: var(--text-color);
  }
</style>
