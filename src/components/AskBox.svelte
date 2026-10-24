<script lang="ts">
  // Ask FreeBank (v0.4.6): one question, one answer, nothing kept. On easynet's Getting started card and in the
  // header's Ask: the helper steers a newcomer and explains the app.
  import { onMount } from "svelte";
  import { askHelper } from "../lib/ask";
  import { nice } from "../lib/errors";

  export let placeholder = "How do I borrow?";
  /** Take the cursor when shown (the header's Ask dialog). */
  export let focus = false;
  let input: HTMLInputElement;
  onMount(() => {
    if (focus) input?.focus();
  });
  let q = "";
  let asked = "";
  let answer = "";
  let error = "";
  let busy = false;

  // v0.4.7: the grey example becomes the question with the right arrow, as a terminal takes a suggestion; Enter asks it.
  function takeExample(e: KeyboardEvent) {
    if (e.key === "ArrowRight" && !q && placeholder) {
      e.preventDefault();
      q = placeholder;
    }
  }

  async function ask() {
    const question = q.trim();
    if (!question || busy) return;
    busy = true;
    asked = question;
    answer = "";
    error = "";
    try {
      answer = await askHelper(question);
      q = "";
    } catch (e) {
      error = nice(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="ask" data-testid="ask-box">
  <form class="ask-form" on:submit|preventDefault={ask}>
    <input bind:this={input} bind:value={q} on:keydown={takeExample} maxlength="500" placeholder={placeholder ? `${placeholder}  →` : ""}
      title={placeholder ? "→ takes the example question" : undefined} aria-label="Your question" autocomplete="off" spellcheck="true" />
    <button type="submit" disabled={busy || !q.trim()}>{busy ? "…" : "Ask"}</button>
  </form>
  {#if asked && (answer || error || busy)}
    <p class="ask-q">{asked}</p>
  {/if}
  {#if busy}
    <p class="muted small">Asking FreeBank's helper…</p>
  {:else if answer}
    <p class="ask-a" data-testid="ask-answer">{answer}</p>
  {:else if error}
    <p class="soft-error">{error}</p>
  {/if}
  <p class="muted small ask-note">
    FreeBank's helper is an AI that answers from FreeBank's notes. Questions go to FreeBank's server and to Anthropic,
    which runs it: never type recovery words or anything private.
  </p>
</div>

<style>
  .ask {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ask-form {
    display: flex;
    gap: 8px;
  }
  .ask-form input {
    flex: 1;
    min-width: 0;
  }
  .ask-form button {
    padding: 8px 16px;
  }
  .ask-q {
    color: var(--text-color);
    font-weight: 600;
    margin: 4px 0 0;
  }
  .ask-a {
    white-space: pre-wrap;
    overflow-wrap: anywhere; /* a long link breaks instead of running past the card */
    color: var(--text-color);
    line-height: 1.5;
    margin: 0;
  }
  .ask-note {
    margin: 0;
  }
</style>
