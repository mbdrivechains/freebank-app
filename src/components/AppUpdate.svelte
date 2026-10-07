<script lang="ts">
  // The app's own updates (v0.2.4, lib/appUpdate.ts). As a notice at the top of the app when a signed release is out
  // (`notice`), and as Settings > App updates.
  import AptOffer from "./AptOffer.svelte";
  import {
    APT_PAGE,
    aptOffer,
    appUpdate,
    appUpdateLater,
    appUpdateProgress,
    checkAppUpdate,
    stageText,
    startAppUpdate,
  } from "../lib/appUpdate";
  import { megabytes, openUrl } from "../lib/node";

  export let notice = false;

  let checking = false;
  let checked = false;
  let startError = "";

  $: c = $appUpdate;
  $: p = $appUpdateProgress;
  $: updating = !!p?.running;
  $: failed = p && !p.running ? p.error : null;
  $: show = notice ? !!c?.available && (!$appUpdateLater || updating) : true;

  async function check() {
    checking = true;
    await checkAppUpdate(true);
    checking = false;
    checked = true;
  }

  async function update() {
    startError = "";
    try {
      await startAppUpdate();
    } catch (e) {
      startError = String(e);
    }
  }
</script>

{#if show}
  <div class="card app-update" class:app-update-notice={notice}>
    {#if !notice}
      <h2>App updates</h2>
      <p>This is FreeBank {c?.current ?? ""}.</p>
    {/if}

    {#if c?.available}
      <p><strong>FreeBank {c.latest} is out.</strong>{notice ? ` You have ${c.current}.` : ""}</p>
      {#if updating && p}
        <p class="hint">
          {stageText(p)}{p.stage === "download" ? ` ${megabytes(p.bytes, p.total)}` : ""}
          {#if p.note}<br />Waiting: {p.note}{/if}
        </p>
        {#if p.stage === "download" && p.total}
          <span class="bar"><span class="bar-fill" style="width:{(p.bytes / p.total) * 100}%"></span></span>
        {/if}
      {:else if c.how === "self"}
        <p class="hint">FreeBank closes and opens again on the new version.</p>
        <div class="row-actions">
          <button on:click={update}>Update and restart</button>
          {#if c.page}<button class="secondary" on:click={() => openUrl(c?.page ?? "")}>What's new</button>{/if}
          {#if notice}<button class="ghost" on:click={() => appUpdateLater.set(true)}>Later</button>{/if}
        </div>
      {:else if c.how === "apt"}
        <p class="hint">Software Updater offers it, from FreeBank's apt repository.</p>
        <div class="row-actions">
          {#if c.page}<button class="secondary" on:click={() => openUrl(c?.page ?? "")}>What's new</button>{/if}
          {#if notice}<button class="ghost" on:click={() => appUpdateLater.set(true)}>Later</button>{/if}
        </div>
      {:else if c.how === "deb"}
        <p class="hint">
          Download the new .deb from the release page and open it. Or let Software Updater keep FreeBank up to date{$aptOffer
            ? " (below)"
            : ": set up FreeBank's apt repository once"}.
        </p>
        <div class="row-actions">
          {#if c.page}<button on:click={() => openUrl(c?.page ?? "")}>Download</button>{/if}
          {#if !$aptOffer}<button class="secondary" on:click={() => openUrl(APT_PAGE)}>The apt repository</button>{/if}
          {#if notice}<button class="ghost" on:click={() => appUpdateLater.set(true)}>Later</button>{/if}
        </div>
      {:else}
        {#if c.why}<p class="hint">{c.why}</p>{/if}
        <div class="row-actions">
          {#if c.page}<button on:click={() => openUrl(c?.page ?? "")}>Download</button>{/if}
          {#if notice}<button class="ghost" on:click={() => appUpdateLater.set(true)}>Later</button>{/if}
        </div>
      {/if}
      {#if startError}<p class="soft-error">{startError}</p>{/if}
      {#if failed}<p class="soft-error">The update didn't finish: {failed}</p>{/if}
    {:else if !notice}
      {#if c?.error && checked}
        <p class="soft-error">{c.error}</p>
      {:else if checked && c}
        <p class="hint ok-note">You have the newest version.</p>
      {/if}
      <div class="row-actions">
        <button class="secondary" on:click={check} disabled={checking}>{checking ? "Checking…" : "Check for updates"}</button>
      </div>
    {/if}

    {#if !notice}<AptOffer />{/if}

    {#if !notice}
      <p class="muted small">
        FreeBank installs an update only when it carries the signature of FreeBank's release key: the same check it
        makes before installing the node.
      </p>
    {/if}
  </div>
{/if}

<style>
  .app-update-notice {
    border-color: var(--accent-color);
    background: var(--accent-tint);
  }
  .app-update p {
    margin: 0 0 8px;
  }
</style>
