// Gram notes: notes, bills and deposits count milligrams of gold. Since Gold B (node v1.2.0) each house
// pays its notes in sECX at a rate it posts itself; there is no chain price, no price reporters and no gold tokens on
// FreeBank (they live on Ethereum). Older nodes count notes in sats of sECX (1 unit = 1 sat), which is what amount.ts's
// helpers serve.
//
// Wording (legal, USER_WARNINGS_2026-11 §2.6 and §2.8): never "gold-backed", "gold reserves", "bank", "currency", "safe"
// or "stable" for grams; under B also never "equity", "solvent", "proven" or "backed", and no figure for what a house
// holds in gold tokens. Never the 10% floor as what a house holds: show the house's own ratios. The overdue bills the
// chain sees are never shown as complete or as a measure of loan quality, and a failed house shows "Failed: demand
// unpaid" with its dates, never "insolvent".
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

/** `gethousecapital` (Gold B, node v1.2.0): a house's rate and capital as the chain counts them. Rates are sats a gram;
 *  the mg figures count sECX at the house's highest ask of the last 30 days. */
export interface HouseCapital {
  /** The rate in effect for the next block: what it pays a gram (bid) and charges (ask). Null: none posted. */
  bid: number | null;
  ask: number | null;
  ask_30d_high?: number | null;
  bid_90d_high?: number | null;
  /** Issuing needs a rate posted within 24 h of the chain clock. */
  rate_fresh: boolean;
  rate_posted_height?: number;
  /** Unix seconds on the chain clock (median-time based). */
  rate_posted_time?: number;
  rate_fresh_until?: number;
  escrow_sats: number;
  escrow_mg: number | null;
  /** sECX the house showed it controls at its last attestation (it can spend it afterwards; not seized in a failure). */
  till_attested_sats: number;
  till_mg: number | null;
  notes_mg: number;
  deposits_mg: number;
  pending_swaps_mg?: number;
  capital_cap_mg?: number | null;
  till_cap_mg?: number;
  demand_interest_mg?: number;
  /** The bills in its book the chain sees as overdue: never complete (a house can repay a bill for its borrower). */
  overdue_bills_seen?: number;
  overdue_bill_ids?: number[];
}

export const getHouseCapital = (id: number) => fbCall("gethousecapital", [id]) as Promise<HouseCapital>;

// --- The per-note and per-house card ---

/** What gethouse adds for the card (node v1.2.0): the protest, the cure and the failure, in blocks. */
export interface HouseState {
  effective_status: string;
  lastattestheight?: number;
  protest_height?: number;
  cure_insolvent_height?: number;
  next_cure_height?: number;
  insolventheight?: number;
  [k: string]: unknown;
}

export interface HouseCard {
  status: string;
  bid: number | null;
  ask: number | null;
  /** Its highest ask of the last 30 days: what a bill referring to it is repaid at in sECX. */
  ask30: number | null;
  rateFresh: boolean;
  /** Unix seconds (chain clock) the posted rate lapses for issuing; null: never posted. */
  rateFreshUntil: number | null;
  /** sECX escrow over notes and deposits, and the attested till over notes (both at the 30-day ask); null with nothing
   *  issued or no rate. */
  escrowRatio: number | null;
  tillRatio: number | null;
  escrowSats: number;
  tillSats: number;
  /** The block the till was checked at (its last attestation); null: never. */
  tillAt: number | null;
  /** Interest owed on demanded notes while it was suspended, in mg. */
  demandInterestMg: number;
  /** The bills the chain sees as overdue (never complete). */
  overdueSeen: number;
  /** A protested house's last block to pay before it fails; a failed house's protest and failure blocks. */
  cureUntil: number | null;
  protestedAt: number | null;
  failedAt: number | null;
}

export function houseCard(h: HouseState, c: HouseCapital): HouseCard {
  const pos = (n: unknown) => (typeof n === "number" && n > 0 ? n : null);
  const issued = c.notes_mg + Math.max(0, c.deposits_mg);
  return {
    status: h.effective_status,
    bid: pos(c.bid),
    ask: pos(c.ask),
    ask30: pos(c.ask_30d_high),
    rateFresh: c.rate_fresh === true,
    rateFreshUntil: pos(c.rate_fresh_until),
    escrowRatio: issued > 0 && c.escrow_mg !== null ? c.escrow_mg / issued : null,
    tillRatio: c.notes_mg > 0 && c.till_mg !== null ? c.till_mg / c.notes_mg : null,
    escrowSats: c.escrow_sats,
    tillSats: c.till_attested_sats,
    tillAt: pos(h.lastattestheight),
    demandInterestMg: c.demand_interest_mg ?? 0,
    overdueSeen: c.overdue_bills_seen ?? 0,
    cureUntil: h.effective_status === "stressed" ? pos(h.cure_insolvent_height) : null,
    protestedAt: pos(h.protest_height),
    failedAt: h.effective_status === "insolvent" ? pos(h.insolventheight) : null,
  };
}

/** A ratio as a percent with one decimal: 0.1 → "10.0%". */
export function pct1(r: number): string {
  return `${(r * 100).toFixed(1)}%`;
}

const blk = (n: number) => `block ${n.toLocaleString("en-US")}`;

/** Plain words for a house's status (Gold B: a house fails only when a demand goes unpaid past its cure). */
export function statusWords(s: string, c?: Pick<HouseCard, "cureUntil" | "protestedAt" | "failedAt">): string {
  if (s === "stressed") {
    // cure_insolvent_height is an estimate: the cure counts 600 s clock blocks, so with slower blocks the house fails
    // before it (legal, 11-08).
    return `Protested: a demand went unpaid${c?.cureUntil ? `; it has until about ${blk(c.cureUntil)} to pay or suspend payment` : ""}`;
  }
  if (s === "insolvent") {
    const when = [c?.protestedAt ? `protested at ${blk(c.protestedAt)}` : "", c?.failedAt ? `failed at ${blk(c.failedAt)}` : ""]
      .filter(Boolean)
      .join(", ");
    return `Failed: demand unpaid${when ? ` (${when})` : ""}`;
  }
  return ({ open: "Open", deferred: "Suspended: demands are queued and earn interest until paid", wounddown: "Wound down" } as Record<string, string>)[s] ?? s;
}

/** The house's rate in words: "pays 0.001 sECX a gram, asks 0.00101", or that it has none posted. */
export function rateWords(c: Pick<HouseCard, "bid" | "ask">): string {
  return c.bid
    ? `pays ${fmtEcx(c.bid)} ${BASE_TICKER} a gram${c.ask ? `, asks ${fmtEcx(c.ask)} ${BASE_TICKER}` : ""}`
    : "no rate posted";
}

/** The overdue bills the chain sees, in words that never claim to be complete (legal). */
export function overdueWords(n: number): string {
  return `${n.toLocaleString("en-US")} bill${n === 1 ? "" : "s"} the chain sees as overdue. A house can repay a bill for its borrower, so not every bad loan shows.`;
}

/** The disclosure shown wherever a note is issued or received (Gold B: legal's N and F sentences, USER_WARNINGS §2.6),
 *  filled in from the card. */
export function disclosure(c: HouseCard): string {
  const rate = c.bid ? ` (now ${fmtEcx(c.bid)} ${BASE_TICKER} a gram)` : " (it has none posted now)";
  const escrow = c.escrowRatio === null ? "" : ` Its ${BASE_TICKER} escrow is ${pct1(c.escrowRatio)} of its notes and deposits.`;
  return (
    `A note is a claim on the house that issued it, not on FreeBank, its developers or any gold. Notes are ` +
    `denominated in grams of gold and paid in ${BASE_TICKER} at a rate the house sets itself${rate}, or in gold ` +
    `tokens on Ethereum if the house delivers them. FreeBank checks only a house's ${BASE_TICKER}, never its gold ` +
    `tokens, and shows whether the house pays.${escrow} If a house fails, holders are paid from what the chain can ` +
    `seize, mainly its ${BASE_TICKER} escrow, and that may not cover them in full. Moving ${BASE_TICKER} back to eCash ` +
    `is slow and needs eCash miners' approval, and ${BASE_TICKER} can trade below ECX.`
  );
}

// --- Prices ---

/** "about 33.00000000 sECX at the house's rate now" for mg of notes at its bid, or null with no rate posted. */
export function payoutWords(mg: number, bid: number | null, spreadBps = 0): string | null {
  if (!bid) return null;
  const bps = Math.min(10_000, Math.max(0, Math.floor(spreadBps)));
  const sats = Math.floor((Math.floor((mg * bid) / MG_PER_GRAM) * (10_000 - bps)) / 10_000);
  const spread = bps > 0
    ? `, after the house's ${(bps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}% spread (a house that isn't open charges one while its till is under 2% of its notes)`
    : "";
  return `about ${fmtEcx(sats)} ${BASE_TICKER} at the house's rate now${spread}`;
}
