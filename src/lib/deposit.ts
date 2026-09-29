// Home's Deposit panel and the balance card's pending line (v0.2.0): the node's deposit address, and
// the wallet's coins on their way. Both go through rpc_call, whose allowlist
// (src-tauri/src/security.rs, RPC_ALLOWED) names every fbCall here.

import { writable } from "svelte/store";
import { fbCall } from "./api";
import { sha256Hex } from "./sha256";

/** FreeBank's sidechain slot (freebankd's THIS_SIDECHAIN). */
export const SLOT = 130;

export interface DepositAddress {
  /** s130_<address>_<checksum>: what BitWindow's Deposit takes. */
  wrapped: string;
  /** The FreeBank address inside it: what other tools take. */
  plain: string;
}

/** The first 6 hex characters of SHA-256("s130_<address>_"), as freebankd's GenerateDepositAddress. */
export function depositChecksum(address: string, slot = SLOT): string {
  return sha256Hex(`s${slot}_${address}_`).slice(0, 6);
}

/** Read the node's getdepositaddress answer. freebankd returns the wrapped form; a bare address is
 *  wrapped here. A wrapper for another slot, or with a wrong checksum, is refused, never shown. */
export function parseDepositAddress(answer: string): DepositAddress {
  const t = String(answer).trim();
  const m = /^s(\d+)_([1-9A-HJ-NP-Za-km-z]{25,64})_([0-9a-f]{6})$/.exec(t);
  if (m) {
    const [, slot, plain, sum] = m;
    if (Number(slot) !== SLOT) {
      throw new Error(`The node gave a deposit address for sidechain ${slot}, not FreeBank's (${SLOT}).`);
    }
    if (depositChecksum(plain) !== sum) throw new Error("The node's deposit address has a wrong checksum.");
    return { wrapped: t, plain };
  }
  if (/^[1-9A-HJ-NP-Za-km-z]{25,64}$/.test(t)) return { wrapped: `s${SLOT}_${t}_${depositChecksum(t)}`, plain: t };
  throw new Error("The node's answer isn't a deposit address.");
}

/** A new deposit address from the node's wallet. */
export async function getDepositAddress(): Promise<DepositAddress> {
  return parseDepositAddress(String(await fbCall("getdepositaddress")));
}

/** This session's deposit address, so going back to Home doesn't make a new one each time. */
export const depositAddress = writable<DepositAddress | null>(null);
/** The Deposit panel is open (this session). */
export const depositOpen = writable(false);

/** The wallet's coins on their way, in sats: unconfirmed (incoming, not in a block yet) and
 *  immature (a deposit or newly mined coins, in a block but spendable only after the next one). */
export interface Pending {
  unconfirmed: number;
  immature: number;
}

const toSats = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) && v > 0 ? Math.round(v * 1e8) : 0);

export async function getPending(): Promise<Pending> {
  const w = (await fbCall("getwalletinfo")) as { unconfirmed_balance?: number; immature_balance?: number };
  return { unconfirmed: toSats(w?.unconfirmed_balance), immature: toSats(w?.immature_balance) };
}
