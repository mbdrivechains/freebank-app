<script lang="ts">
  // Home, "Deposit from eCash" (v0.2.0): how to move ECX from the eCash chain into FreeBank with BitWindow.
  // The address comes from freebankd's getdepositaddress in the wrapped form s130_<address>_<checksum>
  // (lib/deposit.ts checks the slot and checksum), with Copy and a QR code, and the plain address for other
  // tools. It is made when the panel first opens and kept for the session.
  //
  // `canShowAddresses` is false until the wallet has its passphrase: setting one replaces the wallet's seed,
  // so an address shown before would belong to the old seed. App.svelte wires it to the wallet's state.
  import Notice from "./Notice.svelte";
  import QrCode from "./QrCode.svelte";
  import { depositAddress, depositOpen, getDepositAddress } from "../lib/deposit";
  import { nice } from "../lib/errors";
  import { withUnlock } from "../lib/wallet";

  export let canShowAddresses = true;

  let busy = false;
  let error = "";
  let copied: "" | "wrapped" | "plain" = "";
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  async function fetchAddress() {
    busy = true;
    error = "";
    try {
      // A locked wallet with an empty key pool answers -12; withUnlock asks for the passphrase then.
      depositAddress.set(await withUnlock(() => getDepositAddress(), { what: "make a deposit address" }));
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }

  function show() {
    depositOpen.set(true);
    if (canShowAddresses && !$depositAddress && !busy) fetchAddress();
  }

  async function copy(text: string, which: "wrapped" | "plain") {
    try {
      await navigator.clipboard.writeText(text);
      copied = which;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = ""), 1500);
    } catch {
      error = "Couldn't copy it; select the address and copy it by hand.";
    }
  }
</script>

<div class="card deposit" data-open={$depositOpen}>
  <div class="dep-head">
    <h3>Deposit from eCash</h3>
    {#if $depositOpen}
      <button class="link-btn" on:click={() => depositOpen.set(false)}>Hide</button>
    {/if}
  </div>
  <p class="muted small">Move ECX from the eCash chain into FreeBank with BitWindow.</p>

  {#if !$depositOpen}
    <button class="secondary dep-show" on:click={show}>Show how to deposit</button>
  {:else if !canShowAddresses}
    <p class="hint dep-wait">Your deposit address shows here once your wallet has a passphrase.</p>
  {:else}
    {#if error}<Notice kind="error" message={error} on:dismiss={() => (error = "")} />{/if}
    {#if !$depositAddress}
      <button class="dep-show" on:click={fetchAddress} disabled={busy}>{busy ? "Getting your address…" : "Get my deposit address"}</button>
    {:else}
      <ol class="dep-steps">
        <li>In BitWindow, open <strong>Sidechains</strong>, then <strong>FreeBank</strong>, then <strong>Deposit</strong>.</li>
        <li>Paste your FreeBank deposit address:</li>
      </ol>
      <div class="address-display">
        <code data-testid="deposit-wrapped">{$depositAddress.wrapped}</code>
        <button on:click={() => $depositAddress && copy($depositAddress.wrapped, "wrapped")}>{copied === "wrapped" ? "Copied" : "Copy"}</button>
      </div>
      <div class="dep-qr"><QrCode text={$depositAddress.wrapped} size={184} /></div>
      <ol class="dep-steps" start="3">
        <li>Enter the amount and send.</li>
      </ol>
      <p class="hint dep-plain-note">
        Use the wrapped form in BitWindow only; other tools take the plain address:
      </p>
      <div class="address-display">
        <code data-testid="deposit-plain">{$depositAddress.plain}</code>
        <button on:click={() => $depositAddress && copy($depositAddress.plain, "plain")}>{copied === "plain" ? "Copied" : "Copy"}</button>
      </div>
      <p class="hint">
        The deposit shows under your balance once its eCash payment is in a block and a FreeBank block takes it in.
      </p>
      <Notice kind="info" dismissible={false}>
        If your eCash node started from a snapshot, deposits show only after it finishes checking the whole chain,
        which can take days.
      </Notice>
      <button class="link-btn dep-new" on:click={fetchAddress} disabled={busy}>{busy ? "…" : "New deposit address"}</button>
    {/if}
  {/if}
</div>

<style>
  .dep-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  .dep-head h3 {
    margin-bottom: 0;
  }
  .deposit > .muted {
    margin-top: 6px;
  }
  .dep-show {
    width: 100%;
    margin-top: 12px;
  }
  .dep-wait {
    margin-top: 10px;
  }
  .dep-steps {
    margin: 12px 0 10px 20px;
    font-size: 13.5px;
    line-height: 1.5;
  }
  .dep-steps li + li {
    margin-top: 4px;
  }
  .dep-qr {
    display: flex;
    justify-content: center;
    margin: 4px 0 6px;
  }
  .dep-plain-note {
    margin: 6px 0 8px;
  }
  .deposit .address-display {
    margin-bottom: 12px;
  }
  .deposit .address-display code {
    user-select: all;
    -webkit-user-select: all;
  }
  .deposit :global(.notice) {
    margin: 12px 0 4px;
  }
  .dep-new {
    margin-top: 6px;
    padding-left: 0;
  }
</style>
