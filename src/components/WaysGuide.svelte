<script lang="ts">
  // "Ways to use FreeBank" (v0.6.12): the goal menu, then a goal's guide (src/lib/ways.ts). Opened from Home, Credit
  // and Settings > Help. "Go there" closes it on the screen a step names; "Send as a suggestion" opens the report
  // dialog over it, filled in, for the user to read before anything goes.
  import { BASE_TICKER } from "../lib/brand";
  import { openReport, reportDraft } from "../lib/report";
  import {
    COMMON,
    WAYS,
    blocksFor,
    bondFor,
    goTo,
    inBase,
    suggestion,
    wayById,
    waysOpen,
    type Target,
    type Way,
  } from "../lib/ways";

  $: way = $waysOpen ? wayById($waysOpen) : undefined;

  const IN_APP: Record<Way["inApp"], string> = {
    yes: "In the app",
    partly: "Partly in the app",
    "not yet": "Command line today",
  };

  // The bill calculator: it only converts what is typed here, and keeps nothing.
  let calcDays = "90";
  let calcPrice = "";
  let calcRate = "";
  let calcBond = "";
  $: calcBlocks = blocksFor(parseFloat(calcDays));
  $: calcFace = inBase(parseFloat(calcPrice), parseFloat(calcRate));
  $: calcBondAmt = bondFor(calcFace, parseFloat(calcBond));

  function close() {
    waysOpen.set(null);
  }
  function go(t: Target) {
    goTo.set({ view: t.view, seg: t.seg, home: t.home });
    close();
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !$reportDraft) close();
  }
  const fmt = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: 8 });
</script>

<svelte:window on:keydown={onKey} />

<div class="ways-back">
  <div class="ways card" role="dialog" aria-modal="true" aria-labelledby="ways-title" data-testid="ways-guide">
    <div class="ways-head">
      {#if way}
        <button class="link-btn" on:click={() => waysOpen.set("")} data-testid="ways-back">‹ All ways</button>
      {/if}
      <button class="link-btn ways-close" on:click={close} aria-label="Close">Close</button>
    </div>

    {#if !way}
      <h3 id="ways-title">Ways to use FreeBank</h3>
      <p class="muted">What do you want to do?</p>
      <div class="ways-menu">
        {#each WAYS as w}
          <button class="secondary ways-goal" on:click={() => waysOpen.set(w.id)} data-testid="ways-goal-{w.id}">
            <span>{w.goal}</span>
            <span class="ways-tag tag-{w.inApp.replace(' ', '-')}">{IN_APP[w.inApp]}</span>
          </button>
        {/each}
      </div>
      <button class="link-btn" on:click={() => openReport("idea", "Ways to use FreeBank: I want to ")} data-testid="ways-other">
        Something else? Tell us what you want to do ›
      </button>
      <p class="muted small">{COMMON[0]}</p>
    {:else}
      <h3 id="ways-title">{way.title}</h3>
      <p>{way.summary}</p>
      <dl class="ways-facts">
        <dt>Who lends</dt>
        <dd>{way.lends}</dd>
        <dt>Who carries the risk</dt>
        <dd>{way.risk}</dd>
        <dt>What it costs</dt>
        <dd>{way.cost}</dd>
        <dt>In the app</dt>
        <dd><span class="ways-tag tag-{way.inApp.replace(' ', '-')}">{IN_APP[way.inApp]}</span> {way.inAppNote}</dd>
      </dl>

      {#if way.calc}
        <div class="ways-calc" data-testid="ways-calc">
          <b>Bill figures</b>
          <div class="calc-row">
            <label>Days to the due date <input type="number" min="1" bind:value={calcDays} /></label>
            <span class="calc-out">{calcBlocks ? `${fmt(calcBlocks)} blocks` : "—"}</span>
          </div>
          <div class="calc-row">
            <label>Price, in your currency <input type="number" min="0" bind:value={calcPrice} placeholder="10000" /></label>
            <label>Your currency per {BASE_TICKER} <input type="number" min="0" bind:value={calcRate} /></label>
            <span class="calc-out">{calcFace ? `${fmt(calcFace)} ${BASE_TICKER}` : "—"}</span>
          </div>
          <div class="calc-row">
            <label>Bond, % of the price <input type="number" min="0" max="100" bind:value={calcBond} /></label>
            <span class="calc-out">{calcBondAmt ? `${fmt(calcBondAmt)} ${BASE_TICKER}` : "—"}</span>
          </div>
          <p class="muted small">It only converts what you type, at the rate you type, and keeps nothing. FreeBank sets no bond.</p>
        </div>
      {/if}

      <b>Steps</b>
      <ol class="ways-steps">
        {#each way.steps as s}
          <li>
            {#if s.who}<b>{s.who}:</b>{/if}
            {s.text}
            {#if s.go}
              {@const target = s.go}
              <button class="link-btn" on:click={() => go(target)}>{target.label} ›</button>
            {/if}
          </li>
        {/each}
      </ol>

      {#if way.gaps.length}
        <b>FreeBank can't do this yet</b>
        <ul class="ways-gaps">
          {#each way.gaps as g}
            <li>
              {g}
              {#if way}
                {@const w = way}
                <button class="link-btn" on:click={() => openReport("idea", suggestion(w, g))}>Send as a suggestion ›</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      <div class="ways-know">
        <b>Know before you start</b>
        <ul>
          {#each COMMON as line}<li>{line}</li>{/each}
        </ul>
      </div>
    {/if}
  </div>
</div>

<style>
  .ways-back {
    position: fixed;
    inset: 0;
    z-index: 54; /* under the report dialog (55), which "Send as a suggestion" opens over it */
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }
  .ways {
    width: 100%;
    max-width: 560px;
    max-height: calc(100vh - 32px);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin: 0;
    font-size: 14px;
    line-height: 1.45;
  }
  .ways h3 {
    margin: 0;
  }
  .ways p {
    margin: 0;
  }
  .ways-head {
    display: flex;
    justify-content: space-between;
  }
  .ways-close {
    margin-left: auto;
  }
  .ways-menu {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ways-goal {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    text-align: left;
    padding: 10px 12px;
    font-weight: 600;
  }
  .ways-tag {
    flex: none;
    font-size: 11.5px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .tag-yes {
    background: rgba(76, 175, 80, 0.2);
    color: #7bd47f;
  }
  .tag-partly {
    background: rgba(255, 193, 7, 0.18);
    color: #ffd54f;
  }
  .tag-not-yet {
    background: var(--border-color);
    color: var(--text-secondary);
  }
  .ways-facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 12px;
    margin: 0;
  }
  .ways-facts dt {
    color: var(--text-secondary);
  }
  .ways-facts dd {
    margin: 0;
  }
  .ways-calc {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
  }
  .calc-row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px 12px;
  }
  .calc-row label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12.5px;
    color: var(--text-secondary);
  }
  .calc-row input {
    width: 140px;
  }
  .calc-out {
    font-weight: 600;
    padding-bottom: 6px;
  }
  .ways-steps,
  .ways-gaps,
  .ways-know ul {
    margin: 0;
    padding-left: 20px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ways-know {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--accent-tint);
    color: var(--text-secondary);
    font-size: 13px;
  }
  .ways-know b {
    color: var(--text-color);
  }
  @media (max-width: 480px) {
    .ways-facts {
      grid-template-columns: 1fr;
    }
    .ways-facts dd {
      margin-bottom: 4px;
    }
  }
</style>
