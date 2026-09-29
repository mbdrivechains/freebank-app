<script lang="ts">
  // A new passphrase typed twice, with a strength hint (advice, not a rule); or, with `twice` off, the
  // current passphrase once. The parent reads `value` (empty until the fields agree) and `valid`, and
  // can call clear(). The fields are cleared when this goes away.
  import { onDestroy, onMount } from "svelte";
  import { passphraseStrength } from "../lib/walletSeed";

  /** The passphrase, once the fields agree; "" until then. */
  export let value = "";
  export let valid = false;
  export let twice = true;
  export let label = "Passphrase";
  export let autofocus = true;
  export let disabled = false;

  let first = "";
  let second = "";
  let input: HTMLInputElement;

  $: strength = passphraseStrength(first);
  $: mismatch = twice && second.length > 0 && second !== first;
  $: valid = first.length > 0 && (!twice || second === first);
  $: value = valid ? first : "";

  export function clear() {
    first = "";
    second = "";
  }

  onMount(() => {
    if (autofocus) input?.focus();
  });
  onDestroy(clear);
</script>

<div class="pp">
  <label class="pp-field">
    {label}
    <input bind:this={input} type="password" bind:value={first} autocomplete="off" spellcheck="false" {disabled} />
  </label>
  {#if twice}
    {#if first}
      <div class="pp-strength" aria-live="polite">
        <span class="pp-bar"><span class="pp-fill s{strength.level}" style="width:{(strength.level + 1) * 25}%"></span></span>
        <span class="pp-label">{strength.label}</span>
      </div>
    {/if}
    <label class="pp-field">
      {label} again
      <input
        type="password"
        bind:value={second}
        autocomplete="off"
        spellcheck="false"
        {disabled}
        aria-invalid={mismatch ? "true" : undefined}
      />
    </label>
    {#if mismatch}<p class="pp-mismatch" role="alert">The two don't match yet.</p>{/if}
  {/if}
</div>

<style>
  .pp {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .pp-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    color: var(--text-secondary);
  }
  .pp-strength {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--text-secondary);
    margin-top: -4px;
  }
  .pp-bar {
    width: 72px;
    height: 5px;
    border-radius: 3px;
    background: var(--bg-inset);
    border: 1px solid var(--border-color);
    overflow: hidden;
    flex: none;
  }
  .pp-fill {
    display: block;
    height: 100%;
  }
  .pp-fill.s0 {
    background: var(--error-color);
  }
  .pp-fill.s1 {
    background: #e0a458;
  }
  .pp-fill.s2,
  .pp-fill.s3 {
    background: var(--success-color);
  }
  .pp-mismatch {
    font-size: 12.5px;
    color: #e0a458;
    margin-top: -4px;
  }
</style>
