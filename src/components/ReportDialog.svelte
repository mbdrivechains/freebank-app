<script lang="ts">
  // "Report a problem or suggest something": to FreeBank's server without an account, or on GitHub.
  // lib/report.ts says where each goes. Nothing goes without the user pressing a button, and they see
  // every detail that would go with it.
  import { onMount } from "svelte";
  import { nice } from "../lib/errors";
  import { openUrl } from "../lib/node";
  import { githubUrl, report, reportDraft, type ReportDetails, type ReportKind } from "../lib/report";

  let kind: ReportKind = $reportDraft?.kind ?? "problem";
  let text = $reportDraft?.text ?? "";
  let contact = "";
  let withDetails = true;
  let details: ReportDetails | null = null;
  let sending = false;
  let sent = "";
  let error = "";
  let box: HTMLTextAreaElement;

  const KINDS: [ReportKind, string][] = [
    ["problem", "A problem"],
    ["idea", "An idea"],
    ["security", "A security problem"],
  ];
  $: detailLine = details
    ? `FreeBank app ${details.app} · ${details.os}${details.node ? ` · node ${details.node}` : ""}`
    : "";

  onMount(async () => {
    box?.focus();
    box?.setSelectionRange(text.length, text.length);
    try {
      details = await report.details();
    } catch {
      details = null;
    }
  });

  function close() {
    reportDraft.set(null);
  }

  async function send() {
    sending = true;
    error = "";
    try {
      const id = await report.send(kind, text, contact, withDetails && !!details);
      sent = id || "—";
    } catch (e) {
      error = nice(e);
    } finally {
      sending = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !sending) close();
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="report-back">
  <div class="report card" role="dialog" aria-modal="true" aria-labelledby="report-title">
    <h3 id="report-title">Report a problem or suggest something</h3>
    {#if sent}
      <p>Sent to FreeBank. Thank you. Its reference is <span class="mono">{sent}</span>.</p>
      {#if kind === "security"}
        <p class="muted small">Only the FreeBank team reads it. Please keep it to yourself until it is fixed.</p>
      {:else}
        <p class="muted small">Good reports become GitHub issues, without your contact or details.</p>
      {/if}
      <div class="row-actions"><button on:click={close}>Close</button></div>
    {:else}
      <div class="kinds" role="radiogroup" aria-label="What is it?">
        {#each KINDS as [k, label]}
          <label class="kind" class:on={kind === k}>
            <input type="radio" name="report-kind" value={k} bind:group={kind} />{label}
          </label>
        {/each}
      </div>
      <textarea
        bind:this={box}
        bind:value={text}
        rows="6"
        maxlength="8000"
        placeholder={kind === "idea"
          ? "What would you like FreeBank to do, and what would it help you with?"
          : "What you did, what you expected, and what FreeBank did instead."}
      ></textarea>
      <p class="muted small">
        Leave out addresses, amounts, transaction ids and anything else private{kind === "security"
          ? ""
          : ": GitHub issues are public"}.
      </p>
      {#if details}
        <label class="check">
          <input type="checkbox" bind:checked={withDetails} />
          <span>Include <span class="mono">{detailLine}</span></span>
        </label>
      {/if}
      <label class="field">
        <span class="field-label">Your email or other contact, if you'd like a reply (optional)</span>
        <input type="text" bind:value={contact} maxlength="200" autocomplete="off" />
      </label>
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      <div class="row-actions">
        <button on:click={send} disabled={sending || !text.trim()}>{sending ? "Sending…" : "Send to FreeBank"}</button>
        <button class="secondary" on:click={() => openUrl(githubUrl(kind, text, withDetails ? details : null))}>
          {kind === "security" ? "Report privately on GitHub" : "Open on GitHub"}
        </button>
        <button class="secondary" on:click={close} disabled={sending}>Cancel</button>
      </div>
      <p class="muted small">
        <strong>Send to FreeBank</strong> needs no account: it goes to FreeBank's server, where only the FreeBank team reads
        it, with nothing but what you see here. <strong>{kind === "security" ? "Report privately on GitHub" : "Open on GitHub"}</strong>
        needs a GitHub account{kind === "security"
          ? " and stays private"
          : ": it opens the form in your browser, filled in, for you to check and post"}.
      </p>
    {/if}
  </div>
</div>

<style>
  .report-back {
    position: fixed;
    inset: 0;
    z-index: 55; /* above the phone's prompts (50), under an unlock (60) */
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .report {
    width: 100%;
    max-width: 520px;
    max-height: calc(100vh - 32px);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .report h3 {
    margin: 0;
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .kind {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-radius: 8px;
    border: 1px solid var(--border-color);
    font-size: 13px;
    cursor: pointer;
  }
  .kind.on {
    border-color: var(--accent-color);
  }
  .kind input {
    accent-color: var(--accent-color);
  }
  textarea {
    width: 100%;
    resize: vertical;
    font: inherit;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: 13px;
  }
  .check input {
    margin-top: 3px;
    accent-color: var(--accent-color);
  }
  .mono {
    font-family: ui-monospace, Menlo, Consolas, monospace;
    font-size: 12px;
  }
</style>
