// Gram notes (FreeBank's gram-notes release; field list in freebank docs-local/GRAMS_APP_FIELDS.md): notes, bills and
// deposits count milligrams of gold, and every sECX payout is priced at the chain's payout price. Older nodes count
// notes in sats of sECX (1 unit = 1 sat), which is what amount.ts's helpers serve.
//
// Wording (legal's conditions, app/PUNCHLIST.md): never "gold-backed", "gold reserves", "bank", "currency", "safe" or
// "stable" for grams, and never the 10% floor as what a house holds: show the house's own ratios.
import { writable, type Readable } from "svelte/store";
import { fbCall } from "./api";
import { BASE_TICKER } from "./brand";
import { ECX_PROBLEM, ecxInput, fmtEcx, parseEcx } from "./amount";

export type NoteUnit = "sat" | "mg";

const unit = writable<NoteUnit>("sat");
/** The unit of the connected node's notes: "sat" until detectNoteUnit has seen a gram node. */
export const noteUnit: Readable<NoteUnit> = { subscribe: unit.subscribe };
const notePools = writable(true);
/** Whether house note pools take orders: `getblockchaininfo`'s `note_pools_open` (false on gram nodes until a second,
 *  unconnected producer bids); absent on older nodes, whose note pools are open (and on early gram builds, which
 *  count as closed). */
export const notePoolsOpen: Readable<boolean> = { subscribe: notePools.subscribe };

/** Which unit the node's notes count: `getblockchaininfo`'s `note_unit`, "mg" on a gram node (from genesis on every
 *  network it runs) and absent on older nodes, whose notes count sats. No answer (no node yet) leaves the unit as it
 *  was and says null. */
/** A bill's face in the node's unit (sats, or mg on a gram node). Gram nodes (FreeBank v1.1.0 and later) print it as
 *  `face_mg`, a whole number; older nodes as `amount`, an 8-decimal amount of sECX. Neither: 0. */
export function billUnits(b: { face_mg?: unknown; amount?: unknown }): number {
  if (b.face_mg !== undefined && b.face_mg !== null) {
    const n = typeof b.face_mg === "number" ? b.face_mg : Number(b.face_mg);
    return Number.isSafeInteger(n) && n >= 0 ? n : 0;
  }
  return b.amount === undefined || b.amount === null ? 0 : parseEcx(String(b.amount)) ?? 0;
}
/** A bill's face (or a discount's price) as the node reads it: whole mg on a gram node, else an 8-decimal amount of
 *  sECX from sats. */
export const billArg = (units: number, unit: NoteUnit): number => (unit === "mg" ? units : units / 1e8);

export async function detectNoteUnit(): Promise<NoteUnit | null> {
  const info = await fbCall("getblockchaininfo").then(
    (i) => i as { note_unit?: string; note_pools_open?: boolean },
    () => null,
  );
  const u = unitOf(info);
  if (u) {
    unit.set(u);
    // An older node's note pools are open; a gram node's only when it says so.
    notePools.set(u === "sat" ? info?.note_pools_open !== false : info?.note_pools_open === true);
  }
  return u;
}

export function unitOf(info: { note_unit?: string } | null | undefined): NoteUnit | null {
  if (!info || typeof info !== "object") return null;
  return info.note_unit === "mg" ? "mg" : "sat";
}

export const MG_PER_GRAM = 1000;

/** Milligrams as grams, grouped, without trailing zeros: 300000 → "300", 1234567 → "1,234.567", 500 → "0.5". */
export function fmtGrams(mg: number): string {
  const neg = mg < 0;
  const s = Math.round(Math.abs(mg));
  const whole = Math.floor(s / MG_PER_GRAM).toLocaleString("en-US");
  const frac = String(s % MG_PER_GRAM).padStart(3, "0").replace(/0+$/, "");
  return `${neg ? "-" : ""}${whole}${frac ? `.${frac}` : ""}`;
}

/** Grams typed in ("1.5", or a number) as whole mg; null unless a plain decimal above zero with at most 3 places. */
export function parseGrams(v: string | number | null | undefined): number | null {
  if (v === null || v === undefined) return null;
  const t = typeof v === "number" ? (Number.isFinite(v) ? String(v) : "") : v;
  const m = /^\s*(\d*)(?:\.(\d{0,3}))?\s*$/.exec(t);
  if (!m || (m[1] === "" && (m[2] ?? "") === "")) return null;
  const mg = Number(m[1] || "0") * MG_PER_GRAM + Number((m[2] ?? "").padEnd(3, "0"));
  return Number.isSafeInteger(mg) && mg > 0 ? mg : null;
}

/** Whole mg for an input field, ungrouped: 1500 → "1.5". */
export function gramsInput(mg: number): string {
  return fmtGrams(Math.max(0, mg)).replace(/,/g, "");
}

export const GRAMS_PROBLEM = "Enter an amount in grams above zero, with at most 3 decimal places.";

/** Note amounts in the node's unit: format, parse and the input text, so a screen needn't know which it is. */
export interface NoteAmounts {
  unit: NoteUnit;
  /** "300 g" or "0.00300000 sECX". */
  fmt(units: number): string;
  parse(v: string | number | null | undefined): number | null;
  input(units: number): string;
  problem: string;
  /** What an amount field is in: "g" or "sECX". */
  label: string;
}

export function noteAmounts(u: NoteUnit): NoteAmounts {
  return u === "mg"
    ? { unit: u, fmt: (n) => `${fmtGrams(n)} g`, parse: parseGrams, input: gramsInput, problem: GRAMS_PROBLEM, label: "g" }
    : { unit: u, fmt: (n) => `${fmtEcx(n)} ${BASE_TICKER}`, parse: parseEcx, input: ecxInput, problem: ECX_PROBLEM, label: BASE_TICKER };
}

// --- What the node says (gram nodes only) ---

export interface HouseGold {
  capital_counted_mg: number;
  till_counted_mg: number;
  capital_raw_mg: number;
  till_raw_mg: number;
  notes_mg: number;
  capital_cap_mg: number;
  till_cap_mg: number;
  vaults: { kind: "capital" | "till"; partner: number; token_id: string; units: number }[];
  /** An issuer's freeze on the house's gold: the units stay in the vault (no payout takes them) and count zero once
   *  `counts_zero` (30 days after the freeze). */
  frozen?: { token_id: string; frozen: number; grace_ends: number; counts_zero: boolean }[];
  /** Gold + sECX escrow at the payout price + loan book less overdue bills - notes - deposits, in mg; null: no price. */
  equity_mg?: number | null;
  escrow_sats?: number;
  overdue_bills?: number;
  /** Interest accrued on demanded notes, in mg (v1.1.0); equity_mg already subtracts it. */
  demand_interest_mg?: number;
}

export interface IssuedToken {
  token_id: string;
  kind: string; // gold_mg, or a dollar kind
  ticker: string;
  upstream: string;
  backing_contract: string;
  backing_address: string;
  supply: number;
  issuer_house: number; // 0: not a house
  freeze_key: string; // empty: none
  burns_queued: number;
  /** Bonded (at least 1 kg), backing fresh and covering the supply, owning its backing entry: only a trusted token's
   *  gold enters a vault or moves the payout price. */
  trusted?: boolean;
  /** The height it stopped being trusted (0: never); its vault gold counts for about 30 days after. */
  untrusted_since?: number;
  /** Trusted, and a payout price exists: houses can issue against it. */
  issuing_ok?: boolean;
}

export interface GoldFix {
  payout_view_sats_per_gram: number | null; // the 144-block median; null: stale
  stale: boolean;
  gold_ecx_last_fix: { value: number; reporters: number; fixheight: number; ageblocks: number } | null;
  gold_usd_last_fix: { value: number; reporters: number; fixheight: number; ageblocks: number } | null;
}

export interface TokenPool {
  token_id: string;
  ticker?: string;
  /** The pool's shares are a token of their own: the wallet's coins of it (listissued) are its share. */
  lp_token_id: string;
  units: number;
  sats: number;
  lp_supply: number;
  fee_bps: number;
  create_height?: number;
  /** Absent until the pool has a view (its 144 block samples). */
  view_sats_per_gram?: number;
  deep?: boolean;
}

/** `listtokenpools` (v1.1.0): every token pool, and the chain's payout price now (null: none). */
export interface TokenPools {
  payout_price_sats_per_gram: number | null;
  pools: TokenPool[];
}

export const getHouseGold = (id: number) => fbCall("gethousegold", [id]) as Promise<HouseGold>;
export const getIssuedToken = (id: string) => fbCall("getissuedtoken", [id]) as Promise<IssuedToken>;
export const getGoldFix = () => fbCall("getgoldfix") as Promise<GoldFix>;
export const listTokenPools = () => fbCall("listtokenpools") as Promise<TokenPools>;

/** One of the wallet's token coins (`listissued`). */
export interface TokenCoin {
  txid: string;
  vout: number;
  token_id: string;
  units: number;
  frozen: number;
  confirmations: number;
  address: string;
}
export const listIssued = () => fbCall("listissued") as Promise<TokenCoin[]>;

/** The wallet's units per token, and how many of them an issuer has frozen. */
export function holdings(coins: TokenCoin[]): Map<string, { units: number; frozen: number }> {
  const m = new Map<string, { units: number; frozen: number }>();
  for (const c of coins) {
    const h = m.get(c.token_id) ?? { units: 0, frozen: 0 };
    h.units += c.units;
    h.frozen += c.frozen;
    m.set(c.token_id, h);
  }
  return m;
}

// --- The per-note card (design draft 9, R2-ops-14) ---

export interface HouseCard {
  status: string;
  /** Counted owners' capital over notes + deposits, and counted till over notes; null with nothing issued. */
  capitalRatio: number | null;
  tillRatio: number | null;
  /** "GOLDA (acme-gold)", one per token in its vaults, in vault order. */
  issuers: string[];
  /** A failed house: gold still in its vaults per gram of notes, which holders' claims are paid from. */
  goldPerGram: number | null;
  /** Its tokens that fail the chain's checks (the node's `trusted: false`), in words: "GOLDB (beta-gold) has failed
   *  the chain's checks on gold tokens since block 812 (…): its gold stops counting about 30 days after". */
  untrusted: string[];
  /** Units an issuer has frozen in its vaults (no payout takes them). */
  frozenMg: number;
  /** Owners' equity in mg (null: no price, or an older node). */
  equityMg: number | null;
  /** Interest owed on demanded notes, in mg (already taken off the equity). */
  demandInterestMg: number;
}

/** What the node's `trusted` checks, in words. Public copy never calls a token "trusted" (no endorsement of issuers):
 *  it says the token passes or fails these. */
export const TOKEN_CHECKS = "an issuer bond, backing reports that cover its supply, and redemptions paid on time";

/** "GOLDA (acme-gold)", or the id's start for a token the node hasn't described. */
export function tokenName(id: string, tokens: Map<string, IssuedToken>): string {
  const t = tokens.get(id);
  return t ? (t.upstream ? `${t.ticker} (${t.upstream})` : t.ticker) : `${id.slice(0, 8)}…`;
}

export function houseCard(status: string, depositUnits: number, g: HouseGold, tokens: Map<string, IssuedToken>): HouseCard {
  const issued = g.notes_mg + Math.max(0, depositUnits);
  const ids = [...new Set(g.vaults.filter((v) => v.units > 0).map((v) => v.token_id))];
  return {
    status,
    capitalRatio: issued > 0 ? g.capital_counted_mg / issued : null,
    tillRatio: g.notes_mg > 0 ? g.till_counted_mg / g.notes_mg : null,
    issuers: ids.map((id) => tokenName(id, tokens)),
    goldPerGram: status === "insolvent" && g.notes_mg > 0 ? (g.capital_raw_mg + g.till_raw_mg) / g.notes_mg : null,
    untrusted: ids
      .filter((id) => tokens.get(id)?.trusted === false)
      .map((id) => {
        const since = tokens.get(id)?.untrusted_since;
        return `${tokenName(id, tokens)} has failed the chain's checks on gold tokens${since ? ` since block ${since.toLocaleString("en-US")}` : ""} (${TOKEN_CHECKS}): its gold stops counting about 30 days after`;
      }),
    frozenMg: (g.frozen ?? []).reduce((n, f) => n + f.frozen, 0),
    equityMg: g.equity_mg ?? null,
    demandInterestMg: g.demand_interest_mg ?? 0,
  };
}

/** A ratio as a percent with one decimal: 0.1 → "10.0%". */
export function pct1(r: number): string {
  return `${(r * 100).toFixed(1)}%`;
}

/** Plain words for a house's status. */
export function statusWords(s: string): string {
  return (
    { open: "Open", stressed: "Stressed: a demand went unpaid", deferred: "Deferred: paying demands from a queue", insolvent: "Failed: paying claims from what it has left", wounddown: "Wound down" } as Record<string, string>
  )[s] ?? s;
}

function list(xs: string[]): string {
  return xs.length <= 1 ? xs.join("") : `${xs.slice(0, -1).join(", ")} and ${xs[xs.length - 1]}`;
}

/** The disclosure shown wherever a note is issued or received (design draft 9 with legal's condition-4 additions,
 *  as GRAMS_APP_FIELDS.md gives it), filled in from the card. */
export function disclosure(c: HouseCard): string {
  const issuers = c.issuers.length ? list(c.issuers) : "no one yet";
  const x = c.capitalRatio === null ? "a share" : pct1(c.capitalRatio);
  return (
    `This house holds gold tokens issued by ${issuers} and lends the rest. Its owners' capital, ${x} of notes, takes ` +
    `losses first. Notes are paid in ${BASE_TICKER} at the gold price; ${BASE_TICKER} is slow to move back to eCash, ` +
    `needs eCash miners' approval, and can trade below ECX. If more of its borrowers fail than its capital covers ` +
    `(for example, borrowers who earn ECX, when ECX falls against gold), or a gold issuer fails, it may not pay in ` +
    `full. If no gold price is available, it may pay in gold tokens.`
  );
}

// --- Prices ---

/** The payout price now, sats per gram: the chain's own (listtokenpools: the oracle and the counting pools' view,
 *  capped), else the oracle's 144-block median when that call failed; null when there is none, and payouts then wait
 *  or pay in kind. */
export function payoutPrice(fix: GoldFix | null, list: TokenPools | null): number | null {
  if (list) return list.payout_price_sats_per_gram;
  return fix && !fix.stale ? fix.payout_view_sats_per_gram : null;
}

/** The gold price in words for a Prices line: "0.00110000 sECX a gram, from 3 reporters, 2 blocks ago", or why there
 *  is none. */
export function goldFixWords(fix: GoldFix | null, satsPerGram: number | null): string {
  const f = fix?.gold_ecx_last_fix;
  if (satsPerGram) {
    const from = f ? `, from ${f.reporters} reporter${f.reporters === 1 ? "" : "s"}, ${ageWords(f.ageblocks)}` : "";
    return `${fmtEcx(satsPerGram)} ${BASE_TICKER} a gram${from}`;
  }
  return f
    ? `None right now: the reporters' last price is ${ageWords(f.ageblocks)}, too old to pay at. Payouts in ${BASE_TICKER} wait, and bills don't fall overdue.`
    : `None yet: no reporter has given a gold price. Payouts in ${BASE_TICKER} wait, and bills don't fall overdue.`;
}

function ageWords(blocks: number): string {
  return blocks === 0 ? "this block" : `${blocks.toLocaleString("en-US")} block${blocks === 1 ? "" : "s"} ago`;
}

/** What a swap pays out, worked out as the node does (pool.cpp PoolSwapOut): in less the fee, against the pool's
 *  reserves; null where the node refuses (nothing in, an empty side, or it would empty the out side). */
export function swapOut(amountIn: number, reserveIn: number, reserveOut: number, feeBps: number): number | null {
  if (!(amountIn > 0) || !(reserveIn > 0) || !(reserveOut > 0)) return null;
  const inFee = BigInt(Math.floor(amountIn)) * BigInt(10000 - feeBps);
  const out = (inFee * BigInt(reserveOut)) / (BigInt(reserveIn) * 10000n + inFee);
  return out === 0n || out >= BigInt(reserveOut) ? null : Number(out);
}

/** The sats that go in with `units` of the token when adding to a pool (the wallet's rule: the pool's ratio, rounded
 *  up). */
export function addSats(units: number, p: TokenPool): number | null {
  if (!(units > 0) || !(p.units > 0)) return null;
  return Number((BigInt(Math.floor(units)) * BigInt(p.sats) + BigInt(p.units) - 1n) / BigInt(p.units));
}

/** How far a pool's price may move between the estimate and the node building the transaction (v1.1.0's bounds):
 *  1%. Beyond it the node refuses and nothing happens. */
export const POOL_SLIP_BPS = 100;

/** The least a swap may give: its estimate less the margin, rounded down (swaptoken's min_out). */
export function leastOut(estimate: number): number {
  return Number((BigInt(Math.floor(estimate)) * BigInt(10_000 - POOL_SLIP_BPS)) / 10_000n);
}

/** The most sats an add may take: its estimate plus the margin, rounded up (addtokenliquidity's max_sats). */
export function mostIn(estimate: number): number {
  return Number((BigInt(Math.floor(estimate)) * BigInt(10_000 + POOL_SLIP_BPS) + 9_999n) / 10_000n);
}

/** What `lp` shares are worth now: their part of the pool's units and sats (rounded down). */
export function shareOf(lp: number, p: TokenPool): { units: number; sats: number } {
  if (!(lp > 0) || !(p.lp_supply > 0)) return { units: 0, sats: 0 };
  return {
    units: Number((BigInt(Math.floor(lp)) * BigInt(p.units)) / BigInt(p.lp_supply)),
    sats: Number((BigInt(Math.floor(lp)) * BigInt(p.sats)) / BigInt(p.lp_supply)),
  };
}

/** "about 33.00000000 sECX at today's price" for mg of notes, or null with no price. */
export function payoutWords(mg: number, satsPerGram: number | null, spreadBps = 0): string | null {
  if (!satsPerGram) return null;
  const bps = Math.min(10_000, Math.max(0, Math.floor(spreadBps)));
  const sats = Math.floor((Math.floor((mg * satsPerGram) / MG_PER_GRAM) * (10_000 - bps)) / 10_000);
  const spread = bps > 0
    ? `, after the house's ${(bps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}% spread (a house that isn't open owes one while its till is under 2% of its notes)`
    : "";
  return `about ${fmtEcx(sats)} ${BASE_TICKER} at today's price${spread}`;
}
