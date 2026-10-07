<script lang="ts">
  // Home's Receive (v0.2.6, the UX walk-through: it used to open empty and make a fresh address on every visit). It
  // shows this session's address at once, with a QR code and Copy; "New address" makes another. Addresses wait until
  // the wallet has its passphrase (setting one replaces the wallet's seed).
  import { onMount } from "svelte";
  import { writable } from "svelte/store";
  import QrCode from "./QrCode.svelte";
  import { api, tauriInvoke } from "../lib/api";
  import { BASE_TICKER } from "../lib/brand";
  import { nice } from "../lib/errors";
  import { withUnlock } from "../lib/wallet";
  import { walletList } from "../lib/wallets";

  export let canShowAddresses = true;

  // The wallet chosen in the header: each keeps its own address (the walk-through's second run found Savings showing
  // the main wallet's). Its name shows above the code once there is more than one.
  $: chosen = $walletList.find((w) => w.active);
  $: key = chosen?.name ?? "";
  $: address = $sessionAddress[key] ?? "";

  let busy = false;
  let error = "";
  let copied = false;

  async function fresh() {
    busy = true;
    error = "";
    try {
      // A locked wallet with an empty key pool answers -12; withUnlock asks for the passphrase then.
      const k = key;
      const a = await withUnlock(() => api.getNewAddress(), { what: "make a new address" });
      sessionAddress.update((m) => ({ ...m, [k]: a }));
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(address);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      error = "Couldn't copy it; select the address and copy it by hand.";
    }
  }

  // A members-only house's notes go only to its member addresses (v0.2.8; Michael, 2026-10-05: "yes to both"): a new
  // address wouldn't be one, so the wallet's member addresses show too.
  let members: { house: number; name: string | null; address: string }[] = [];
  let shownMember: string | null = null;
  async function loadMembers() {
    try {
      members = (await tauriInvoke("member_addresses")) as typeof members;
    } catch {
      members = [];
    }
  }
  async function copyText(t: string) {
    try {
      await navigator.clipboard.writeText(t);
    } catch {
      error = "Couldn't copy it; select the address and copy it by hand.";
    }
  }

  onMount(() => {
    if (canShowAddresses && !address) fresh();
    if (canShowAddresses) loadMembers();
  });
</script>

<script lang="ts" context="module">
  /** This session's receive address for each wallet (by name; "" the main one), kept while the app is open. */
  const sessionAddress = writable<Record<string, string>>({});
</script>

<div class="card" data-testid="receive">
  <h3>Receive {BASE_TICKER}</h3>
  {#if !canShowAddresses}
    <p class="hint">Your addresses show here once your wallet has a passphrase.</p>
  {:else}
    {#if error}<p class="soft-error">{error}</p>{/if}
    {#if address}
      {#if $walletList.length > 1 && chosen}<p class="muted small" data-testid="receive-wallet">Into {chosen.label}</p>{/if}
      <div class="qr-placeholder"><QrCode text={address} size={200} /></div>
      <div class="address-display">
        <code>{address}</code>
        <button on:click={copy}>{copied ? "Copied" : "Copy"}</button>
      </div>
    {:else if busy}
      <p class="muted small">Getting your address…</p>
    {/if}
    <button class="link-btn" on:click={fresh} disabled={busy}>New address</button>
    {#if members.length}
      <div class="member-addresses" data-testid="member-addresses">
        <p class="small"><strong>To be paid in a members-only house's notes</strong>, give its member address:</p>
        {#each members as m (m.house)}
          <div class="member-row">
            <span class="small">Your address at {m.name || `house #${m.house}`}</span>
            <div class="address-display">
              <code>{m.address}</code>
              <button class="secondary" on:click={() => copyText(m.address)}>Copy</button>
              <button class="link-btn inline" on:click={() => (shownMember = shownMember === m.address ? null : m.address)}>
                {shownMember === m.address ? "Hide code" : "Show code"}
              </button>
            </div>
            {#if shownMember === m.address}<div class="qr-placeholder"><QrCode text={m.address} size={180} /></div>{/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .member-addresses {
    margin-top: 1rem;
    border-top: 1px solid var(--border-color, rgba(127, 127, 127, 0.25));
    padding-top: 0.75rem;
  }
  .member-row {
    margin-top: 0.5rem;
  }
</style>
