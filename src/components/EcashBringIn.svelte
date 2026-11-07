<script lang="ts">
  // "Bring in coins from Sparrow or Electrum" (v0.7.3, src-tauri/src/ecash/other.rs). Another wallet's recovery words:
  // FreeBank finds their coins on eCash and moves them all into the main eCash wallet in one payment, with eCash's replay
  // mark, so any bitcoin of the same words stays where it is. The words stay in this card until the move or Hide, go to
  // the app for the search and again for the move, and are never kept.
  import { createEventDispatcher, onDestroy } from "svelte";
  import { nice } from "../lib/errors";
  import { holdsWords, walletSeed } from "../lib/walletSeed";
  import { fmtEcx } from "../lib/amount";
  import { L1_TICKER } from "../lib/brand";
  import { ecashOtherFind, ecashOtherMove, type OtherFound } from "../lib/ecash";

  const dispatch = createEventDispatcher<{ moved: string }>();

  let open = false;
  let words = "";
  let hasPass = false;
  let pass = "";
  let busy: "" | "find" | "move" = "";
  let error = "";
  let note = "";
  let found: OtherFound | null = null;
  let sent = "";

  const coins = (sats: number) => `${fmtEcx(sats)} ${L1_TICKER}`;
  const kindWords = (k: string) => (k === "electrum" ? "Electrum segwit words" : "BIP39 words, as Sparrow's");

  // The words go once the found coins are stale (the app keeps a prepared move five minutes): look again types them
  // again.
  let stale: ReturnType<typeof setTimeout> | undefined;
  function forget() {
    words = "";
    pass = "";
    hasPass = false;
    found = null;
    note = "";
    clearTimeout(stale);
  }
  onDestroy(forget);

  function toggle() {
    open = !open;
    error = "";
    sent = "";
    if (!open) forget();
  }

  // Pasted words: FreeBank puts them in itself, then clears the clipboard (v0.4.10 review, as the restore box does).
  async function pasteWords(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!holdsWords(text)) return;
    e.preventDefault();
    const box = e.currentTarget as HTMLTextAreaElement;
    const at = box.selectionStart ?? words.length;
    words = words.slice(0, at) + text + words.slice(box.selectionEnd ?? at);
    found = null;
    const r = await walletSeed.clearPastedWords().catch(() => "not_gone" as const);
    note =
      r === "gone"
        ? "FreeBank cleared your clipboard. If you use a clipboard history, delete the words there too."
        : "FreeBank asked your clipboard to forget the words but couldn't make sure it did. Copy something else now, and delete the words from any clipboard history.";
  }

  async function find() {
    busy = "find";
    error = "";
    sent = "";
    found = null;
    try {
      found = await ecashOtherFind(words, hasPass ? pass : "");
      clearTimeout(stale);
      stale = setTimeout(() => {
        forget();
        error = "The coins were found too long ago, so FreeBank let the words go. Type them again to look again.";
      }, 5 * 60 * 1000);
    } catch (e) {
      error = nice(e);
    }
    busy = "";
  }

  async function move() {
    if (!found?.id) return;
    busy = "move";
    error = "";
    try {
      sent = await ecashOtherMove(found.id, words, hasPass ? pass : "");
      forget();
      dispatch("moved", sent);
    } catch (e) {
      error = nice(e);
    }
    busy = "";
  }
</script>

<div class="card" data-testid="ecash-bring-in">
  <div class="bi-head">
    <h3>Bring in coins from Sparrow or Electrum</h3>
    <button class="link-btn" on:click={toggle} data-testid="ecash-bring-in-open">{open ? "Hide" : "Show"}</button>
  </div>
  {#if !open}
    <p class="muted small">
      Move the coins of another wallet's recovery words into this eCash wallet. FreeBank uses the words for the move and
      doesn't keep them.
    </p>
  {:else}
    {#if sent}
      <p class="small" data-testid="ecash-bring-in-sent">Moved. Transaction ID: <span class="mono bi-wrap">{sent}</span>. It shows under Payments.</p>
    {/if}
    <p class="small bi-warn">
      If these words held bitcoin before eCash split from Bitcoin, the same coins are on both chains. FreeBank's move
      carries eCash's mark, so only the ECX moves and your bitcoin stays where it is. Typing words that hold bitcoin into
      any program puts that bitcoin at risk: do it only on a computer you trust.
    </p>
    <form class="bi-form" on:submit|preventDefault={find}>
      <label class="field-label" for="bi-words">The wallet's recovery words</label>
      <textarea
        id="bi-words"
        rows="3"
        bind:value={words}
        on:paste={pasteWords}
        on:input={() => (found = null)}
        autocomplete="off"
        spellcheck="false"
        data-testid="ecash-bring-in-words"
      ></textarea>
      {#if note}<p class="hint">{note}</p>{/if}
      <label class="small">
        <input type="checkbox" bind:checked={hasPass} on:change={() => (found = null)} />
        They have a passphrase of their own (Sparrow's BIP39 passphrase, Electrum's "extend this seed")
      </label>
      {#if hasPass}
        <input type="password" bind:value={pass} on:input={() => (found = null)} autocomplete="off" aria-label="The words' passphrase" />
      {/if}
      <p class="muted small">
        FreeBank looks at native segwit addresses (bc1q…): Sparrow's usual wallet (BIP39 words at m/84'/0'/0', or m/84'/1'/0' on a test network) and
        Electrum's segwit seed (m/0'), the first 1,000 receiving and change addresses of each. On eCash's own network that
        takes a few minutes.
      </p>
      <div class="row-actions">
        <button type="submit" disabled={!!busy || !words.trim()} data-testid="ecash-bring-in-find">
          {busy === "find" ? "Looking through eCash's coins…" : "Look for its coins"}
        </button>
      </div>
    </form>
    {#if found}
      {#if found.coins === 0}
        <p class="small" data-testid="ecash-bring-in-none">
          No coins found for these {kindWords(found.kind)} at {found.path} (first address
          <span class="mono bi-wrap">{found.first_address}</span>). Check the words and the passphrase, and that the
          wallet used native segwit addresses. Coins already on their way out (a move waiting for its block) don't count.
        </p>
      {:else}
        <p class="small" data-testid="ecash-bring-in-found">
          Found {found.coins} {found.coins === 1 ? "coin" : "coins"}, {coins(found.sats)}: {kindWords(found.kind)} at
          {found.path} (first address <span class="mono bi-wrap">{found.first_address}</span>).
        </p>
        {#if found.small}
          <p class="small">
            {found.small} {found.small === 1 ? "coin is" : "coins are"} worth less than the fee to move {found.small === 1 ? "it" : "them"}, and
            {found.small === 1 ? "stays" : "stay"} where {found.small === 1 ? "it is" : "they are"}.
          </p>
        {/if}
        {#if found.left_out}
          <p class="small">
            One move takes {found.taking} of them ({coins(found.taking_sats)}): once it confirms, look again for the other
            {found.left_out}.
          </p>
        {/if}
        <p class="small">
          {coins(found.arrives)} arrives in this eCash wallet, at <span class="mono bi-wrap">{found.to}</span>; the fee is
          {coins(found.fee)}.
        </p>
        <div class="row-actions">
          <button on:click={move} disabled={!!busy} data-testid="ecash-bring-in-move">{busy === "move" ? "Moving…" : "Move them in"}</button>
        </div>
      {/if}
    {/if}
  {/if}
  {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
</div>

<style>
  .bi-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .bi-head h3 {
    flex: 1;
  }
  .bi-head button {
    white-space: nowrap;
  }
  .bi-warn {
    border-left: 3px solid var(--warning-color, #c58b00);
    padding-left: 10px;
  }
  .bi-form {
    display: grid;
    gap: 8px;
  }
  .bi-form textarea {
    width: 100%;
    font-family: var(--mono-font, monospace);
  }
  .bi-wrap {
    overflow-wrap: anywhere;
  }
  p.small {
    margin: 8px 0;
  }
</style>
