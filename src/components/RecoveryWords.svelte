<script lang="ts">
  // The 24 recovery words in a numbered grid. The grid can't be selected, copied, cut or dragged. "Copy
  // words" puts them on the clipboard after a warning (people copy them whatever they
  // are told): the app writes it, as a numbered table and as plain words, and
  // clears it after 60 seconds. The parent holds the words and wipes them.
  import { createEventDispatcher } from "svelte";
  import { nice } from "../lib/errors";
  import { walletSeed } from "../lib/walletSeed";

  const dispatch = createEventDispatcher<{ copied: void }>();

  export let words: string[] = [];

  const mac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);
  let copy: "idle" | "ask" | "done" = "idle";
  let copyError = "";
  $: if (words.length === 0) copy = "idle";

  async function copyNow() {
    copyError = "";
    try {
      await walletSeed.copyWords(words);
      copy = "done";
      dispatch("copied");
    } catch (e) {
      copyError = nice(e);
      copy = "idle";
    }
  }
</script>

<!-- svelte-ignore a11y-no-noninteractive-element-interactions -->
<ol
  class="rw"
  aria-label="Your recovery words"
  on:copy|preventDefault
  on:cut|preventDefault
  on:dragstart|preventDefault
  on:contextmenu|preventDefault
>
  {#each words as w, i}
    <li><span class="rw-n">{i + 1}</span><span class="rw-w">{w}</span></li>
  {/each}
</ol>
<div class="rw-copy">
  {#if copy === "ask"}
    <p class="rw-warn">
      Other apps can read the clipboard, and so can clipboard history if you use one{mac
        ? "; Universal Clipboard can pass it to your other Apple devices"
        : ""}. FreeBank clears it after 60 seconds.
    </p>
    <div class="row-actions">
      <button on:click={copyNow}>Copy anyway</button>
      <button class="secondary" on:click={() => (copy = "idle")}>Cancel</button>
    </div>
  {:else if copy === "done"}
    <p class="hint">
      Copied: as a numbered list for Notes or mail, and as plain words for a password manager. FreeBank clears the
      clipboard in 60 seconds. The words restore in FreeBank or a BIP85 tool; Electrum takes them but can't show
      FreeBank coins.
    </p>
  {:else}
    <button class="secondary" on:click={() => (copy = "ask")}>Copy words</button>
  {/if}
  {#if copyError}<p class="soft-error" role="alert">{copyError}</p>{/if}
</div>

<style>
  .rw {
    list-style: none;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
    padding: 12px;
    border-radius: 10px;
    border: 1px solid var(--border-color);
    background: var(--bg-inset);
    user-select: none;
    -webkit-user-select: none;
    cursor: default;
  }
  .rw li {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 5px 6px;
    border-radius: 6px;
    background: var(--bg-secondary);
    min-width: 0;
  }
  .rw-n {
    font-size: 11px;
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
    width: 1.6em;
    text-align: right;
    flex: none;
  }
  .rw-w {
    font-family: ui-monospace, Menlo, Consolas, monospace;
    font-size: 13.5px;
    font-weight: 600;
    color: var(--text-color);
    overflow-wrap: anywhere;
  }
  .rw-copy {
    margin-top: 10px;
  }
  .rw-warn {
    font-size: 13px;
    color: var(--text-secondary);
    margin: 0 0 8px;
  }
  @media (max-width: 380px) {
    .rw {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>
