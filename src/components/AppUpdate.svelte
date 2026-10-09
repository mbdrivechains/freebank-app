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
    autoUpdate,
    checkAppUpdate,
    restartForUpdate,
    setAutoUpdate,
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
  $: a = $autoUpdate;
  // Put in place by an automatic update and waiting for the next start: said instead of "is out".
  $: waiting = a?.installed ?? null;
  $: show = notice ? (!!c?.available || !!waiting) && (!$appUpdateLater || updating) : true;
  let autoError = "";
  async function toggleAuto(on: boolean) {
    autoError = "";
    try {
      await setAutoUpdate(on);
    } catch (e) {
      autoError = String(e);
    }
  }
  async function restartNow() {
    startError = "";
    try {
      await restartForUpdate();
    } catch (e) {
      startError = String(e);
    }
  }

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

    {#if waiting}
      <p><strong>FreeBank {waiting} is in place.</strong> It runs next time you open FreeBank.</p>
      {#if updating && p}
        <p class="hint">{stageText(p)}{p.note ? ` ${p.note}` : ""}</p>
      {:else}
        <div class="row-actions">
          <button on:click={restartNow}>Restart now</button>
          {#if notice}<button class="ghost" on:click={() => appUpdateLater.set(true)}>Later</button>{/if}
        </div>
      {/if}
      {#if startError}<p class="soft-error">{startError}</p>{/if}
    {:else if c?.available}
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

    {#if !notice && a}
      <label class="check-row">
        <input type="checkbox" checked={a.on} disabled={!a.can && !a.on} on:change={(e) => toggleAuto(e.currentTarget.checked)} />
        Update FreeBank by itself
      </label>
      <p class="hint">
        {#if a.can}
          Off unless you turn it on. When on, FreeBank fetches a new signed version in the background (also while its
          window is closed, if it keeps your phone connected), checks it, and puts it in place; it runs the next time
          you open FreeBank.
        {:else}
          This copy of FreeBank can't replace itself (only the Mac app in your Applications folder and the AppImage
          can), so this does nothing here.
        {/if}
      </p>
      {#if a.on && a.failed}
        <p class="soft-error">The automatic update to {a.failed.version} didn't go ahead: {a.failed.reason} It isn't
          tried again by itself until a newer version is out; "Update and restart" tries it now.</p>
      {/if}
      {#if autoError}<p class="soft-error">{autoError}</p>{/if}
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
  /* As KeepRunning's switch row. */
  .check-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    margin: 10px 0 6px;
    font-size: 14px;
    cursor: pointer;
  }
  .check-row input {
    margin-top: 3px;
    flex: none;
  }
</style>
