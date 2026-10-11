<script lang="ts">
  // Home (v0.4.6): a house this wallet runs that must attest its reserves soon, or is stressed for want of it. A
  // house that misses its deadline turns stressed (no new notes), and insolvent after 1008 blocks stressed. The
  // deadline counts blocks, so FreeBank watches it: Attest now, or attest automatically while FreeBank is open.
  import { onDestroy, onMount } from "svelte";
  import { api } from "../lib/api";
  import { autoAttestOff, autoAttestOn, housesDue, type HouseDue } from "../lib/houses";
  import { nice } from "../lib/errors";
  import { showReceipt } from "../lib/receipts";
  import { withUnlock } from "../lib/wallet";

  let houses: HouseDue[] = [];
  let autoWithin = 72;
  let auto = false;
  let asking = false;
  let pass = "";
  let busy = false;
  let error = "";
  let timer: ReturnType<typeof setInterval> | undefined;
  const unlisten: (() => void)[] = [];

  async function load() {
    try {
      const r = await housesDue();
      houses = r.houses;
      autoWithin = r.auto_within;
      auto = r.auto_here;
    } catch {
      /* the node is busy or restarting: the next minute tries again */
    }
  }

  onMount(() => {
    load();
    timer = setInterval(load, 60_000);
    import("@tauri-apps/api/event").then(async ({ listen }) => {
      unlisten.push(
        await listen<{ id: number; name: string; txid?: string; error?: string }>("house-attested", (e) => {
          const h = e.payload;
          if (h.txid) showReceipt({ txid: h.txid, what: `${h.name ?? `House #${h.id}`} attested its reserves (automatically)` });
          else error = `${h.name ?? `House #${h.id}`} couldn't attest automatically: ${h.error ?? "the node refused it"}. Try Attest now.`;
          load();
        }),
        await listen<{ why: string }>("auto-attest-off", (e) => {
          auto = false;
          error = e.payload.why;
        }),
      );
    });
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
    unlisten.forEach((u) => u());
  });

  async function attestNow(h: HouseDue) {
    busy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.attestHouse(h.id), { what: `attest ${h.name}'s reserves` });
      showReceipt({ txid, what: `${h.name} attested its reserves` });
      await load();
    } catch (e) {
      error = nice(e);
    } finally {
      busy = false;
    }
  }

  async function turnOn() {
    busy = true;
    error = "";
    try {
      auto = await autoAttestOn(pass);
      asking = false;
    } catch (e) {
      error = nice(e);
    } finally {
      pass = "";
      busy = false;
    }
  }

  async function turnOff() {
    auto = await autoAttestOff();
  }
</script>

{#each houses as h (h.id)}
  <div class="hd" class:hd-warn={h.status === "stressed" || h.left <= autoWithin} data-testid="house-due">
    {#if h.status === "stressed" && h.cause === "other"}
      <strong>{h.name} is stressed</strong>
      <span>Its reserves are under the floor, or a demand on it stands protested, so it can't issue new notes. An attest
        won't reopen it, and the app can't fix this yet: ask in FreeBank's Telegram group.</span>
    {:else if h.status === "stressed"}
      <strong>{h.name} is stressed</strong>
      <span>It missed its attest deadline, so it can't issue new notes. Attest its reserves to reopen it.</span>
    {:else}
      <strong>{h.name} must attest within {h.left.toLocaleString()} blocks</strong>
      <span>Its deadline is block {h.deadline.toLocaleString()}. Past it, the house turns stressed and can't issue new notes.</span>
    {/if}
    {#if h.cause !== "other"}
      <div class="hd-actions">
        <button on:click={() => attestNow(h)} disabled={busy}>Attest now</button>
      </div>
    {/if}
  </div>
{/each}

<!-- Shown whenever automatic attests are on, so they can always be turned off (the passphrase is in memory). -->
{#if houses.length || auto}
  <div class="hd-auto">
    {#if auto}
      <span>Automatic attests are on: FreeBank attests this wallet's houses by itself, {autoWithin} blocks before each
        deadline, while it's open.</span>
      <button class="link-btn" on:click={turnOff}>Turn off</button>
    {:else if asking}
      <form on:submit|preventDefault={turnOn}>
        <label for="hd-pass">Wallet passphrase, kept in memory only while FreeBank is open</label>
        <div class="hd-row">
          <input id="hd-pass" type="password" bind:value={pass} autocomplete="current-password" />
          <button type="submit" disabled={busy || !pass}>Turn on</button>
          <button type="button" class="ghost" on:click={() => ((asking = false), (pass = ""))}>Cancel</button>
        </div>
      </form>
    {:else}
      <button class="link-btn" on:click={() => (asking = true)}>Attest automatically while FreeBank is open</button>
    {/if}
  </div>
{/if}

{#if error}<p class="soft-error">{error}</p>{/if}

<style>
  .hd {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 10px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(217, 164, 65, 0.35);
    background: var(--accent-tint);
    font-size: 13.5px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .hd strong {
    color: var(--text-color);
    font-size: 14.5px;
  }
  .hd-warn {
    border-color: rgba(229, 116, 106, 0.4);
    background: rgba(229, 116, 106, 0.08);
  }
  .hd-actions button {
    font-size: 13.5px;
    padding: 8px 16px;
  }
  .hd-auto {
    margin: 0 0 14px;
    font-size: 13px;
    color: var(--text-secondary);
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    align-items: center;
  }
  .hd-auto form {
    width: 100%;
  }
  .hd-row {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .hd-row input {
    flex: 1;
    min-width: 0;
  }
</style>
