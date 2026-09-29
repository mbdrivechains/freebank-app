// Receipts for every action that makes a transaction, in the page instead of alert(). On macOS the
// webview has no alert() handler (wry and WKWebView), so an alert did nothing and the txid was lost.
//
//   showReceipt({ txid, what, rows? })  App.svelte lists it under the tabs, newest first, until closed
//   <TxReceipt {txid} {what} …>         or place one yourself, with extra rows and actions (its slots)
//
// A receipt follows its transaction: Unconfirmed, then 1, 2 and 3 confirmations (Confirmed), checked
// with gettransaction on each new block.

import { readable, writable } from "svelte/store";
import { api } from "./api";
import { EXPLORER_URL } from "./brand";

/** A transaction counts as Confirmed at this many confirmations, on Home and on receipts. */
export const CONFIRMED_AT = 3;

export function explorerTxUrl(txid: string): string {
  return `${EXPLORER_URL}/tx/${encodeURIComponent(txid)}`;
}

/** An extra line on a receipt, e.g. { label: "Fee", value: "0.00000226 ECX" }. */
export interface ReceiptRow {
  label: string;
  value: string;
  mono?: boolean;
}

export interface Receipt {
  id: number;
  txid: string;
  /** What was done, in words: "Sent 1.50000000 ECX to X…". */
  what: string;
  /** ms since the epoch */
  sentAt: number;
  rows: ReceiptRow[];
}

/** TxReceipt's `status` event, on every change. */
export interface TxStatus {
  txid: string;
  /** 0 while unconfirmed (and when replaced) */
  confirmations: number;
  /** confirmations >= CONFIRMED_AT */
  confirmed: boolean;
  /** A conflicting transaction confirmed instead (gettransaction's confirmations < 0). */
  replaced: boolean;
  /** This wallet doesn't know the txid. */
  missing: boolean;
}

const KEEP = 5;
let nextId = 1;

/** The receipts App.svelte shows, newest first (at most 5). */
export const receipts = writable<Receipt[]>([]);

/** Show a receipt under the tabs. Returns its id, for dismissReceipt. */
export function showReceipt(r: { txid: string; what: string; rows?: ReceiptRow[]; sentAt?: number }): number {
  const id = nextId++;
  const receipt: Receipt = { id, txid: r.txid, what: r.what, rows: r.rows ?? [], sentAt: r.sentAt ?? Date.now() };
  receipts.update((list) => [receipt, ...list].slice(0, KEEP));
  return id;
}

export function dismissReceipt(id: number): void {
  receipts.update((list) => list.filter((r) => r.id !== id));
}

export function clearReceipts(): void {
  receipts.set([]);
}

const TIP_POLL_MS = 5000;

/** The node's block height, polled every 5 s while anything listens (a live receipt). null until the
 *  first answer; it changes only when a block arrives. */
export const tip = readable<number | null>(null, (set) => {
  let alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const poll = async () => {
    try {
      set(await api.getBlockCount());
    } catch {
      // A node that is busy or restarting: try again on the next round.
    }
    if (alive) timer = setTimeout(poll, TIP_POLL_MS);
  };
  poll();
  return () => {
    alive = false;
    clearTimeout(timer);
  };
});
