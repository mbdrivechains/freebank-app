<script lang="ts">
  // Gold tokens on a gram node (the gram-notes release): the gold price, each gold token's one pool against sECX
  // (swap, add, take out) and the wallet's own gold tokens. House note pools stay closed on gram nodes, so this is the
  // whole Pools segment there. Pools come from listtokenpools; tokens also from the houses' vaults and the wallet's
  // coins. Sending a token to an address has its own command (send_token), which goes through "Approve sends on my
  // phone" as sECX sends do.
  import { R4_POOL } from "../lib/notice";
  import { createEventDispatcher } from "svelte";
  import { api, fbCall, type House } from "../lib/api";
  import { BASE_TICKER } from "../lib/brand";
  import { ECX_PROBLEM, fmtEcx, parseEcx } from "../lib/amount";
  import {
    GRAMS_PROBLEM,
    TOKEN_CHECKS,
    addSats,
    leastOut,
    mostIn,
    fmtGrams,
    getGoldFix,
    getHouseGold,
    getIssuedToken,
    listTokenPools,
    goldFixWords,
    holdings,
    listIssued,
    parseGrams,
    payoutPrice,
    shareOf,
    swapOut,
    tokenName,
    type GoldFix,
    type IssuedToken,
    type TokenPool,
  } from "../lib/grams";
  import { nice } from "../lib/errors";
  import { walletLocked, withUnlock } from "../lib/wallet";
  import { showReceipt } from "../lib/receipts";

  export let houses: House[] = [];

  const dispatch = createEventDispatcher<{ error: string }>();

  let loading = false;
  let fix: GoldFix | null = null;
  let price: number | null = null;
  const tokens = new Map<string, IssuedToken>();
  let pools: TokenPool[] = [];
  let mine = new Map<string, { units: number; frozen: number }>();
  let walletLockedNow = false;
  // Gold tokens: not a pool's shares (those are tokens too) and not a dollar kind.
  let gold: string[] = [];

  type Act = "sell" | "buy" | "add" | "remove" | "send";
  let action: { type: Act; token: string } | null = null;
  let amount = "";
  /** Send: the address, and the review shown before it goes. */
  let to = "";
  let reviewing = false;
  let busy = false;

  // What each pool action costs on top, as every credit action does (the wallet's default; checked on regtest 11-04:
  // a 1 g sale paid the pool's estimate to the sat, less this).
  const TX_FEE_SATS = 100_000;
  /** The same, as the node's fee argument (sECX), passed so the pool bounds after it can be. */
  const TX_FEE = 0.001;
  const NO_POOL = "This token's pool isn't listed yet. Refresh and try again.";
  const SWAP_NOTHING = "That's too little for the pool to give anything back. Try a larger amount.";
  /** The plain sECX a token coin carries to its new owner (the node's ISSUED_COIN_VALUE). */
  const TOKEN_COIN_SATS = 1_000;

  const g = (mg: number) => `${fmtGrams(mg)} g`;
  const s = (sats: number) => `${fmtEcx(sats)} ${BASE_TICKER}`;
  const name = (id: string) => tokenName(id, tokens);
  const poolOf = (id: string) => pools.find((p) => p.token_id === id) ?? null;
  const lpHeld = (p: TokenPool) => mine.get(p.lp_token_id)?.units ?? 0;

  export async function load() {
    loading = true;
    try {
      const ids = new Set<string>();
      for (const h of houses) {
        try {
          for (const v of (await getHouseGold(h.id)).vaults) if (v.units > 0) ids.add(v.token_id);
        } catch {
          // A house the node can't describe yet.
        }
      }
      try {
        mine = holdings(await listIssued());
        walletLockedNow = false;
      } catch (e) {
        if (!walletLocked(e)) throw e;
        walletLockedNow = true;
      }
      for (const id of mine.keys()) ids.add(id);
      const list = await listTokenPools().catch(() => null);
      for (const p of list?.pools ?? []) ids.add(p.token_id);
      for (const id of ids) {
        if (!tokens.has(id)) {
          try {
            tokens.set(id, await getIssuedToken(id));
          } catch {
            // Not a token the node knows (yet).
          }
        }
      }
      pools = (list?.pools ?? []).filter((p) => tokens.get(p.token_id)?.kind === "gold_mg");
      gold = [...ids].filter((id) => tokens.get(id)?.kind === "gold_mg");
      fix = await getGoldFix().catch(() => null);
      price = payoutPrice(fix, list);
    } catch (e) {
      dispatch("error", nice(e));
    }
    loading = false;
  }

  $: if (houses) load();

  function start(type: Act, token: string) {
    action = { type, token };
    amount = "";
    to = "";
    reviewing = false;
  }

  // What the form will do, in words, from today's reserves (null: nothing to say yet).
  $: estimate = (() => {
    if (!action) return null;
    const p = poolOf(action.token);
    if (action.type === "sell" && p) {
      const mg = parseGrams(amount);
      const out = mg === null ? null : swapOut(mg, p.units, p.sats, p.fee_bps);
      return out === null ? null : `You get about ${s(out)}, after the pool's ${(p.fee_bps / 100).toFixed(2)}% fee, and at least ${s(leastOut(out))}.`;
    }
    if (action.type === "buy" && p) {
      const sats = parseEcx(amount);
      const out = sats === null ? null : swapOut(sats, p.sats, p.units, p.fee_bps);
      return out === null ? null : `You get about ${g(out)}, after the pool's ${(p.fee_bps / 100).toFixed(2)}% fee, and at least ${g(leastOut(out))}.`;
    }
    if (action.type === "add" && p) {
      const mg = parseGrams(amount);
      const sats = mg === null ? null : addSats(mg, p);
      return sats === null ? null : `Goes in with about ${s(sats)}, at the pool's ratio, and at most ${s(mostIn(sats))}.`;
    }
    if (action.type === "remove" && p) {
      const lp = Number(amount);
      if (!Number.isInteger(lp) || lp < 1) return null;
      const back = shareOf(lp, p);
      return `You get about ${g(back.units)} and ${s(back.sats)}.`;
    }
    return null;
  })();

  async function submit() {
    if (!action) return;
    const { type, token } = action;
    const p = poolOf(token);
    busy = true;
    dispatch("error", "");
    try {
      let txid = "";
      let what = "";
      const txidOf = (r: unknown) => (typeof r === "string" ? r : (r as { txid: string }).txid);
      if (type === "sell" || type === "add") {
        const mg = parseGrams(amount);
        if (mg === null) throw new Error(GRAMS_PROBLEM);
        if (!p) throw new Error(NO_POOL);
        if (type === "sell") {
          const out = swapOut(mg, p.units, p.sats, p.fee_bps);
          if (!out) throw new Error(SWAP_NOTHING);
          txid = txidOf(await withUnlock(() => fbCall("swaptoken", [token, "sell", mg, TX_FEE, leastOut(out)]), { what: `sell ${g(mg)} of ${name(token)}` }));
          what = `Sold ${g(mg)} of ${name(token)} for ${BASE_TICKER}`;
        } else {
          const with_ = addSats(mg, p);
          if (!with_) throw new Error(SWAP_NOTHING);
          txid = txidOf(await withUnlock(() => fbCall("addtokenliquidity", [token, mg, TX_FEE, mostIn(with_)]), { what: `add ${g(mg)} to the ${name(token)} pool` }));
          what = `Added ${g(mg)} of ${name(token)}${p ? ` and ${s(addSats(mg, p) ?? 0)}` : ""} to its pool`;
        }
      } else if (type === "send") {
        const mg = parseGrams(amount);
        if (mg === null) throw new Error(GRAMS_PROBLEM);
        const addr = to.trim();
        txid = txidOf(await withUnlock(() => api.sendToken(token, mg, addr), { what: `send ${g(mg)} of ${name(token)}` }));
        what = `Sent ${g(mg)} of ${name(token)} to ${addr}`;
      } else if (type === "buy") {
        const sats = parseEcx(amount);
        if (sats === null) throw new Error(ECX_PROBLEM);
        if (!p) throw new Error(NO_POOL);
        const out = swapOut(sats, p.sats, p.units, p.fee_bps);
        if (!out) throw new Error(SWAP_NOTHING);
        txid = txidOf(await withUnlock(() => fbCall("swaptoken", [token, "buy", sats, TX_FEE, leastOut(out)]), { what: `buy ${name(token)} with ${s(sats)}` }));
        what = `Bought ${name(token)} with ${s(sats)}`;
      } else {
        const lp = Number(amount);
        if (!Number.isInteger(lp) || lp < 1) throw new Error("Enter how many shares to take out, a whole number.");
        txid = txidOf(await withUnlock(() => fbCall("removetokenliquidity", [token, lp]), { what: `take ${lp.toLocaleString()} shares out of the ${name(token)} pool` }));
        what = `Took ${lp.toLocaleString()} shares out of the ${name(token)} pool`;
      }
      showReceipt({ txid, what });
      action = null;
      await load();
    } catch (e) {
      dispatch("error", nice(e));
      // The pool moved past the bound: show its figures now, so the estimate is the new one.
      if (/above max_sats|below min_out/i.test(String(e))) await load();
    }
    busy = false;
  }
</script>

<div class="card">
  <div class="notes-head">
    <h2>Gold price</h2>
    <button class="link-btn" on:click={load} disabled={loading}>{loading ? "…" : "Refresh"}</button>
  </div>
  <p>{goldFixWords(fix, price)}</p>
  <p class="muted small">Notes are paid out in {BASE_TICKER} at this price. It comes from the gold price reporters give, and from deep gold token pools.</p>
</div>

{#if walletLockedNow}
  <div class="card locked-read">
    <p>Your wallet is locked, so your gold tokens and pool shares can't be listed.</p>
    <button on:click={() => withUnlock(() => listIssued(), { what: "see your gold tokens" }).then(load, (e) => dispatch("error", nice(e)))}>Unlock</button>
  </div>
{/if}

{#if gold.some((id) => (mine.get(id)?.units ?? 0) > 0)}
  <div class="card">
    <h2>My gold tokens</h2>
    {#each gold.filter((id) => (mine.get(id)?.units ?? 0) > 0) as id}
      {@const h = mine.get(id)}
      {@const t = tokens.get(id)}
      <div class="note-row">
        <div class="note-top">
          <div><span class="note-house">{name(id)}</span></div>
          <div class="note-units">{g(h?.units ?? 0)}</div>
        </div>
        <div class="house-stats">
          {#if h && h.frozen > 0}<div><span class="stat-label">Frozen by its issuer</span> {g(h.frozen)}</div>{/if}
          {#if t && t.trusted === false}<div class="blocked-why">Fails the chain's checks on gold tokens ({TOKEN_CHECKS}): houses can't take it in, and its pool doesn't set the gold price.</div>{/if}
        </div>
        <div class="note-actions">
          <button on:click={() => start("send", id)}>Send</button>
        </div>
        {#if action && action.type === "send" && action.token === id}
          {@const mg = parseGrams(amount)}
          <div class="note-form">
            {#if !reviewing}
              <label>Grams to send (you can send {g((h?.units ?? 0) - (h?.frozen ?? 0))})<input type="number" bind:value={amount} placeholder="0.000" step="0.001" min="0" /></label>
              <label>To a FreeBank address<input type="text" bind:value={to} placeholder="address" autocomplete="off" spellcheck="false" /></label>
              <div class="note-form-actions">
                <button on:click={() => (reviewing = true)} disabled={mg === null || !to.trim()}>Review</button>
                <button class="secondary" on:click={() => (action = null)}>Cancel</button>
              </div>
            {:else}
              <p>Send <strong>{g(mg ?? 0)}</strong> of {name(id)} to <span class="mono" style="word-break: break-all">{to.trim()}</span>?</p>
              <p class="hint">A send can't be taken back. The transaction costs {s(TX_FEE_SATS)}, and {s(TOKEN_COIN_SATS)} goes with the token to its new owner, both paid from your {BASE_TICKER}.</p>
              <div class="note-form-actions">
                <button on:click={submit} disabled={busy}>{busy ? "…" : "Send"}</button>
                <button class="secondary" on:click={() => (reviewing = false)} disabled={busy}>Back</button>
              </div>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<div class="card">
  <div class="notes-head">
    <h2>Gold token pools</h2>
  </div>
  <p class="muted">Each gold token has one pool, which swaps it for {BASE_TICKER} at a price set by what it holds. Swap here, or put the token and {BASE_TICKER} in its pool to earn a share of its fees.</p>
  {#if pools.length === 0}
    <div class="empty">
      <p>{loading ? "Looking for pools…" : "No gold token pools yet."}</p>
    </div>
  {:else}
    {#each pools as p}
      {@const t = tokens.get(p.token_id)}
      {@const lp = lpHeld(p)}
      {@const back = shareOf(lp, p)}
      <div class="note-row">
        <div class="note-top">
          <div><span class="note-house">{name(p.token_id)}</span></div>
        </div>
        <div class="house-stats">
          <div><span class="stat-label">Price now</span> {p.units > 0 ? `${s(Math.round((p.sats * 1000) / p.units))} a gram` : "none: the pool is empty"}</div>
          <div><span class="stat-label">Fee</span> {(p.fee_bps / 100).toFixed(2)}% of each swap, to the pool</div>
          <div><span class="stat-label">In the pool</span> {g(p.units)} and {s(p.sats)}</div>
          <div>
            <span class="stat-label">Sets the gold price</span>
            {#if t && t.trusted === false}No: the token fails the chain's checks{:else if !p.deep}No: the pool holds under 5% of the token's supply{:else}Yes, when houses hold the token{/if}
          </div>
          {#if (mine.get(p.token_id)?.units ?? 0) > 0}<div><span class="stat-label">You hold</span> {g(mine.get(p.token_id)?.units ?? 0)}</div>{/if}
          {#if lp > 0}
            <div><span class="stat-label">Your share</span> {((lp / p.lp_supply) * 100).toFixed(2)}% ({lp.toLocaleString()} shares: about {g(back.units)} and {s(back.sats)})</div>
          {/if}
        </div>
        <div class="note-actions">
          <button on:click={() => start("sell", p.token_id)}>Sell</button>
          <button on:click={() => start("buy", p.token_id)}>Buy</button>
          <button on:click={() => start("add", p.token_id)}>Add</button>
          {#if lp > 0}<button on:click={() => start("remove", p.token_id)}>Take out</button>{/if}
        </div>
        {#if action && action.type !== "send" && action.token === p.token_id}
          <div class="note-form">
            {#if action.type === "sell"}
              <label>Grams to sell<input type="number" bind:value={amount} placeholder="0.000" step="0.001" min="0" /></label>
            {:else if action.type === "buy"}
              <label>{BASE_TICKER} to spend<input type="number" bind:value={amount} placeholder="0.00000000" step="0.00000001" min="0" /></label>
            {:else if action.type === "add"}
              <label>Grams to add<input type="number" bind:value={amount} placeholder="0.000" step="0.001" min="0" /></label>
              <p class="hint role-line" data-testid="notice-pool">{R4_POOL}</p>
            {:else}
              <label>Shares to take out (you have {lp.toLocaleString()})<input type="number" bind:value={amount} placeholder="shares" min="1" max={lp} /></label>
            {/if}
            {#if estimate}<p class="hint">{estimate}</p>{/if}
            <p class="hint">The transaction itself costs {s(TX_FEE_SATS)}, paid from your {BASE_TICKER}.</p>
            {#if action.type !== "remove"}
              <p class="hint">The estimate is at the pool's price now. If other swaps move it more than 1% before yours is made, nothing happens and you can try again.</p>
            {/if}
            <div class="note-form-actions">
              <button on:click={submit} disabled={busy || !amount}>
                {busy ? "…" : action.type === "sell" ? "Sell" : action.type === "buy" ? "Buy" : action.type === "add" ? "Add" : "Take out"}
              </button>
              <button class="secondary" on:click={() => (action = null)}>Cancel</button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>
