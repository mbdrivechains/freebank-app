<script lang="ts">
  // Settings, "Security": every check the app makes on its own setup (src-tauri/src/security.rs), worst
  // first. Each says what it found, why it matters and how to fix it. The checks run when the app connects,
  // on each visit to Home, when this card opens, and on "Check again"; they only look, never change
  // anything. An unencrypted backup gets "Show in folder": FreeBank never deletes a backup by itself.
  import { onDestroy, onMount, tick } from "svelte";
  import Notice from "./Notice.svelte";
  import PathText from "./PathText.svelte";
  import { nice } from "../lib/errors";
  import { focusSecurity, revealFile, runSecurityCheck, security, type Level } from "../lib/security";

  let card: HTMLElement;
  let checking = false;
  let revealError = "";

  async function check() {
    checking = true;
    await runSecurityCheck();
    checking = false;
  }

  async function reveal(path: string) {
    revealError = "";
    try {
      await revealFile(path);
    } catch (e) {
      revealError = nice(e);
    }
  }

  // Home's "How to fix" brings this card into view. The cards above it fill in as their data arrives,
  // which pushes it down, so it is kept in view while the page settles, until the user scrolls or types.
  function keepInView(el: HTMLElement, ms = 2500) {
    const page = el.closest("main") ?? document.body;
    const again = () => el.scrollIntoView({ block: "start" });
    const grown = new ResizeObserver(again);
    const stop = () => {
      grown.disconnect();
      clearTimeout(timer);
      for (const ev of ["wheel", "touchstart", "keydown", "mousedown"]) window.removeEventListener(ev, stop, true);
    };
    for (const ev of ["wheel", "touchstart", "keydown", "mousedown"]) window.addEventListener(ev, stop, true);
    const timer = setTimeout(stop, ms);
    grown.observe(page);
    again();
    return stop;
  }

  let stopKeeping: (() => void) | null = null;
  onMount(async () => {
    if ($focusSecurity) {
      focusSecurity.set(false);
      await tick();
      if (card) stopKeeping = keepInView(card);
    }
    check();
  });
  onDestroy(() => stopKeeping?.());

  const LABEL: Record<Level, string> = { red: "Fix this", warn: "Worth fixing", info: "Good to know", ok: "OK" };

  $: items = $security?.items ?? [];
  $: reds = items.filter((i) => i.level === "red").length;
  $: warns = items.filter((i) => i.level === "warn").length;
  $: summary = !items.length
    ? checking
      ? "Checking…"
      : ""
    : reds
      ? `${reds === 1 ? "One thing" : `${reds} things`} to fix${warns ? `, and ${warns} worth fixing` : ""}.`
      : warns
        ? `Nothing urgent; ${warns === 1 ? "one thing is" : `${warns} things are`} worth fixing.`
        : "All clear.";
  $: at = $security?.at ? new Date($security.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";
</script>

<div class="card sec-card" id="security" bind:this={card}>
  <div class="sec-head">
    <h3>Security</h3>
    <button class="link-btn" on:click={check} disabled={checking}>{checking ? "Checking…" : "Check again"}</button>
  </div>
  <p class="muted small">
    FreeBank checks your wallet's passphrase, which ports your node opens, the wallet backups it made, the node's
    program, and who else can read your keys. It only looks; it never changes anything by itself.
  </p>
  {#if summary}
    <p class="sec-summary" class:bad={reds > 0}>{summary}{#if at}<span class="muted small"> Checked at {at}.</span>{/if}</p>
  {/if}
  {#if $security?.error}
    <Notice kind="error" dismissible={false}>The checks couldn't run: {nice($security.error)}</Notice>
  {/if}
  {#if revealError}
    <Notice kind="error" message={revealError} on:dismiss={() => (revealError = "")} />
  {/if}

  <ul class="sec-list">
    {#each items as it (it.id)}
      <li class="sec-item sec-{it.level}" data-id={it.id}>
        <div class="sec-top">
          <span class="sec-dot" aria-hidden="true"></span>
          <span class="sec-title">{it.title}</span>
          <span class="sec-level">{LABEL[it.level] ?? it.level}</span>
        </div>
        <p class="sec-detail">{it.detail}</p>
        {#if it.fix}
          <p class="sec-fix"><span class="sec-fix-label">How to fix:</span> {it.fix}</p>
        {/if}
        {#each it.files ?? [] as f}
          <div class="sec-file">
            <span class="mono sec-path"><PathText path={f} /></span>
            <button class="secondary sec-btn" on:click={() => reveal(f)}>Show in folder</button>
          </div>
        {/each}
      </li>
    {/each}
  </ul>
</div>

<style>
  .sec-card {
    margin-top: 16px;
    scroll-margin-top: 12px;
  }
  .sec-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  .sec-head h3 {
    margin-bottom: 0;
  }
  .sec-card > .muted {
    margin-top: 8px;
  }
  .sec-summary {
    margin: 12px 0 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--success-color);
  }
  .sec-summary.bad {
    color: var(--error-color);
  }
  .sec-summary .muted {
    font-weight: 400;
    margin-left: 6px;
  }
  .sec-list {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 14px;
  }
  .sec-item {
    --sec-color: var(--text-secondary);
    border: 1px solid var(--border-color);
    border-left: 3px solid var(--sec-color);
    border-radius: 10px;
    padding: 10px 12px;
    background: var(--bg-inset);
    user-select: text;
    -webkit-user-select: text;
  }
  .sec-red {
    --sec-color: var(--error-color);
    background: rgba(229, 116, 106, 0.06);
  }
  .sec-warn {
    --sec-color: #e0a54b;
  }
  .sec-info {
    --sec-color: #7aa7d9;
  }
  .sec-ok {
    --sec-color: var(--success-color);
  }
  .sec-top {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .sec-dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--sec-color);
    transform: translateY(-1px);
  }
  .sec-title {
    flex: 1;
    font-size: 14px;
    font-weight: 600;
  }
  .sec-level {
    flex: none;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--sec-color);
  }
  .sec-detail,
  .sec-fix {
    margin-top: 6px;
    font-size: 13px;
    line-height: 1.45;
    color: var(--text-secondary);
    overflow-wrap: anywhere;
  }
  .sec-ok .sec-detail {
    font-size: 12.5px;
  }
  .sec-fix {
    color: var(--text-color);
  }
  .sec-fix-label {
    font-weight: 600;
  }
  .sec-file {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
  }
  .sec-path {
    flex: 1;
    min-width: 0;
    font-size: 12px;
  }
  .sec-btn {
    flex: none;
    padding: 6px 10px;
    font-size: 12.5px;
  }
</style>
