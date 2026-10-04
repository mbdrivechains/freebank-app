<script lang="ts">
  // Home's transactions: the last ten (App.svelte's list), each opening its details (TxDetails), and
  // "All transactions": History, newest first, 25 to a page, with Export CSV (desktop only), which
  // saves into Documents and says where.
  import { createEventDispatcher } from "svelte";
  import TransactionItem from "./TransactionItem.svelte";
  import TxDetails from "./TxDetails.svelte";
  import Notice from "./Notice.svelte";
  import PathText from "./PathText.svelte";
  import type { Transaction } from "../lib/api";
  import { nice } from "../lib/errors";
  import { loadSendLog, sendLog, sends, type CsvSaved, type HistoryItem } from "../lib/send";

  /** The newest transactions (App.svelte's refresh). */
  export let transactions: Transaction[] = [];
  export let balance = 0;
  /** Said under "No transactions yet" while the balance is 0. */
  export let needCoins = "";

  const dispatch = createEventDispatcher<{ changed: void }>();

  let full = false;
  let page = 0;
  let items: HistoryItem[] = [];
  let more = false;
  let loading = false;
  let error = "";
  let saved: CsvSaved | null = null;
  let exporting = false;
  let open: string | null = null;

  loadSendLog();

  // listtransactions answers oldest first: the newest ten, newest first.
  $: recent = transactions.slice(-10).reverse();

  // Sends a Speed up replaced: their rows say so.
  $: replaced = new Set($sendLog.filter((e) => e.replaced_by).map((e) => e.txid));

  async function show(p: number) {
    full = true;
    loading = true;
    error = "";
    try {
      const r = await sends.history(Math.max(0, p));
      items = r.items;
      more = r.more;
      page = r.page;
    } catch (e) {
      error = nice(e);
    }
    loading = false;
  }

  async function exportCsv() {
    exporting = true;
    error = "";
    saved = null;
    try {
      saved = await sends.exportCsv();
    } catch (e) {
      error = nice(e);
    }
    exporting = false;
  }

  function back() {
    full = false;
    saved = null;
    error = "";
  }

  // A row opens its details on a click, Enter or Space.
  function key(e: KeyboardEvent, txid: string) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      open = txid;
    }
  }

  // A Speed up from the details: App.svelte refreshes, and History reads its page again.
  function changed() {
    dispatch("changed");
    if (full) show(page);
  }
</script>

{#if !full}
  <div class="card">
    <h3>Recent payments</h3>
    {#if transactions.length === 0}
      <p class="muted">No transactions yet.</p>
      {#if balance === 0 && needCoins}<p class="hint">{needCoins}</p>{/if}
    {:else}
      <div class="tx-list">
        {#each recent as tx}
          <div class="ht-row" role="button" tabindex="0" on:click={() => (open = tx.txid)} on:keydown={(e) => key(e, tx.txid)}>
            <TransactionItem {tx} replaced={tx.confirmations < 0 || replaced.has(tx.txid)} />
          </div>
        {/each}
      </div>
      <button type="button" class="link-btn ht-more" on:click={() => show(0)}>All transactions →</button>
    {/if}
  </div>
{:else}
  <div class="card">
    <div class="notes-head">
      <h2>All transactions</h2>
      {#if sends.canExport}
        <button type="button" class="link-btn" on:click={exportCsv} disabled={exporting || items.length === 0}>
          {exporting ? "Exporting…" : "Export CSV"}
        </button>
      {/if}
    </div>
    {#if saved}
      <Notice kind="info" on:dismiss={() => (saved = null)}>
        Saved {saved.rows.toLocaleString()} transaction{saved.rows === 1 ? "" : "s"} to
        <span class="mono"><PathText path={saved.path} /></span>
      </Notice>
    {/if}
    {#if error}
      <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
    {/if}
    {#if loading && items.length === 0}
      <p class="muted">Reading your transactions…</p>
    {:else if items.length === 0 && !error}
      <p class="muted">No transactions yet.</p>
    {:else if items.length}
      <div class="tx-list" aria-busy={loading}>
        {#each items as tx}
          <div class="ht-row" role="button" tabindex="0" on:click={() => (open = tx.txid)} on:keydown={(e) => key(e, tx.txid)}>
            <TransactionItem {tx} replaced={!!tx.replaced_by || tx.confirmations < 0} />
          </div>
        {/each}
      </div>
      <div class="ht-pager">
        <button type="button" class="secondary" on:click={() => show(page - 1)} disabled={loading || page === 0}>← Newer</button>
        <span class="muted small">Page {page + 1}</span>
        <button type="button" class="secondary" on:click={() => show(page + 1)} disabled={loading || !more}>Older →</button>
      </div>
    {/if}
    <button type="button" class="link-btn ht-more" on:click={back}>← Recent transactions</button>
  </div>
{/if}

{#if open}
  <TxDetails txid={open} on:close={() => (open = null)} on:changed={changed} />
{/if}

<style>
  .ht-row {
    cursor: pointer;
  }
  .ht-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  .ht-row:focus-visible {
    outline: 2px solid var(--accent-color);
    outline-offset: -2px;
  }
  .ht-more {
    display: block;
    margin: 10px auto 0;
  }
  .ht-pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-top: 12px;
  }
  .ht-pager button {
    padding: 8px 14px;
    font-size: 14px;
  }
</style>
