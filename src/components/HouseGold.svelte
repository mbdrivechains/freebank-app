<script lang="ts">
  // House tools in grams (v0.7.2): gold for a house this node holds the keys of. A partner pledges gold tokens into
  // the house's owners' capital (pledgegold, a token the house already holds), and the partners move gold into or out
  // of its till (movetill; a token new to the house comes in this way, with every partner's key here). Both sign with
  // this wallet and pay a fee; the phone guard counts what leaves the wallet at the payout price.
  import { R1_HOUSE } from "../lib/notice";
  import { fbCall, type House } from "../lib/api";
  import { BASE_TICKER } from "../lib/brand";
  import {
    GRAMS_PROBLEM,
    fmtGrams,
    getIssuedToken,
    holdings,
    listIssued,
    parseGrams,
    tokenName,
    type HouseGold as Gold,
    type IssuedToken,
  } from "../lib/grams";
  import { nice } from "../lib/errors";
  import { withUnlock } from "../lib/wallet";
  import { showReceipt } from "../lib/receipts";

  export let houses: House[] = [];

  type Act = "pledge" | "till-in" | "till-out";
  const TX_FEE = 0.001;
  let houseId = "";
  let act: Act = "pledge";
  let partner = "0";
  let token = "";
  let amount = "";
  let busy = false;
  let error = "";
  let gold: Gold | null = null;
  let held = new Map<string, { units: number; frozen: number }>();
  const tokens = new Map<string, IssuedToken>();
  let tokensVersion = 0;

  async function know(id: string) {
    if (tokens.has(id)) return;
    try {
      tokens.set(id, await getIssuedToken(id));
      tokensVersion++;
    } catch {
      /* named by its id's start */
    }
  }

  async function loadHeld() {
    try {
      held = holdings(await listIssued());
      for (const id of held.keys()) await know(id);
    } catch (e) {
      error = nice(e);
    }
  }
  loadHeld();

  $: id = Number(houseId);
  $: house = houses.find((h) => h.id === id);
  $: if (house) loadGold(house.id);
  else gold = null;
  async function loadGold(hid: number) {
    try {
      const g = (await fbCall("gethousegold", [hid])) as Gold;
      if (house?.id !== hid) return;
      gold = g;
      for (const v of g.vaults) await know(v.token_id);
    } catch (e) {
      gold = null;
      error = nice(e);
    }
  }

  const name = (t: string) => (tokensVersion, tokenName(t, tokens));
  const g = (mg: number) => `${fmtGrams(mg)} g`;
  // What can go: the wallet's gold tokens (not frozen) into capital or the till; the house's till out of it.
  $: tillOf = (t: string) => (gold?.vaults ?? []).filter((v) => v.kind === "till" && v.token_id === t).reduce((n, v) => n + v.units, 0);
  $: choices =
    act === "till-out"
      ? [...new Set((gold?.vaults ?? []).filter((v) => v.kind === "till" && v.units > 0).map((v) => v.token_id))].map((t) => ({ t, most: tillOf(t) }))
      : [...held.entries()]
          .filter(([t, h]) => h.units - h.frozen > 0 && (tokensVersion, tokens.get(t)?.kind !== "pool_lp"))
          .map(([t, h]) => ({ t, most: h.units - h.frozen }));
  $: if (choices.length && !choices.some((c) => c.t === token)) token = choices[0].t;
  $: most = choices.find((c) => c.t === token)?.most ?? 0;
  $: inHouse = !!gold?.vaults.some((v) => v.token_id === token);

  async function submit() {
    error = "";
    const mg = parseGrams(amount);
    if (!house) return void (error = "Choose the house.");
    if (!token) return void (error = act === "till-out" ? "This house has nothing in its till." : "This wallet holds no gold tokens to move.");
    if (mg === null) return void (error = GRAMS_PROBLEM);
    if (mg > most) return void (error = `At most ${g(most)} of ${name(token)}.`);
    const p = Number(partner);
    if (act === "pledge" && !(Number.isInteger(p) && p >= 0)) return void (error = "Enter the partner's number (0 for the first partner).");
    busy = true;
    try {
      const hid = house.id;
      const [method, args, what] =
        act === "pledge"
          ? (["pledgegold", [hid, p, token, mg, TX_FEE], `pledge ${g(mg)} of ${name(token)} to house #${hid}'s owners' capital`] as const)
          : act === "till-in"
            ? (["movetill", [hid, token, mg, TX_FEE], `move ${g(mg)} of ${name(token)} into house #${hid}'s till`] as const)
            : (["movetill", [hid, token, -mg, TX_FEE], `take ${g(mg)} of ${name(token)} out of house #${hid}'s till`] as const);
      const r = await withUnlock(() => fbCall(method, [...args]), { what });
      const txid = typeof r === "string" ? r : (r as { txid: string }).txid;
      showReceipt({ txid, what: what[0].toUpperCase() + what.slice(1), rows: [{ label: "Fee", value: `${TX_FEE} ${BASE_TICKER}` }] });
      amount = "";
      await loadHeld();
      await loadGold(hid);
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }
</script>

<div class="card" data-testid="house-gold">
  <h3>Gold for your house</h3>
  <p class="muted">
    For a house your node holds the keys of. A partner pledges gold tokens into the house's owners' capital, and the
    partners move gold into or out of its till. The house can issue notes only while its counted capital is at least
    10% of its notes and deposits and its till at least 2% of its notes.
  </p>
  <p class="hint role-line">{R1_HOUSE}</p>
  <div class="form">
    <label>
      House
      <select bind:value={houseId}>
        <option value="">Choose…</option>
        {#each houses as h (h.id)}<option value={String(h.id)}>#{h.id} {h.classid}</option>{/each}
      </select>
    </label>
    {#if gold}
      <div class="house-stats" data-testid="house-gold-vaults">
        {#each gold.vaults.filter((v) => v.units > 0) as v}
          <div><span class="stat-label">{v.kind === "till" ? "Till" : `Capital, partner ${v.partner}`}</span> {g(v.units)} of {name(v.token_id)}</div>
        {:else}
          <div class="muted small">No gold in this house yet.</div>
        {/each}
      </div>
    {/if}
    <label>
      What
      <select bind:value={act}>
        <option value="pledge">Pledge to its owners' capital</option>
        <option value="till-in">Move into its till</option>
        <option value="till-out">Take out of its till</option>
      </select>
    </label>
    {#if act === "pledge"}
      <label>
        As partner number
        <input type="number" min="0" step="1" bind:value={partner} />
      </label>
    {/if}
    <label>
      Gold token
      <select bind:value={token} disabled={!choices.length}>
        {#each choices as c (c.t)}<option value={c.t}>{name(c.t)}: {g(c.most)} {act === "till-out" ? "in the till" : "in this wallet"}</option>{/each}
      </select>
    </label>
    {#if act === "pledge" && token && gold && !inHouse}
      <p class="hint">This house doesn't hold {name(token)} yet: a token new to the house comes in through its till, which all its partners agree to.</p>
    {/if}
    <label>
      Grams
      <input type="number" bind:value={amount} placeholder="0.000" step="0.001" min="0" />
    </label>
    <p class="hint">The transaction costs {TX_FEE} {BASE_TICKER}. Out of the till only while the house is open or stressed.</p>
    <button on:click={submit} disabled={busy || !house || !amount}>
      {busy ? "…" : act === "pledge" ? "Pledge" : act === "till-in" ? "Move into the till" : "Take out of the till"}
    </button>
    {#if error}<p class="blocked-why">{error}</p>{/if}
  </div>
</div>
