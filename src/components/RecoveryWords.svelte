<script lang="ts">
  // The 24 recovery words in a numbered grid. They can't be selected, copied, cut or dragged: they are
  // written down by hand, never put on the clipboard. The parent holds the words and wipes them.
  export let words: string[] = [];
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
  @media (max-width: 380px) {
    .rw {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>
