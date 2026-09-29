// Send, Speed up and History (v0.2.0), for the screens. The desktop app builds, signs and sends in
// Rust (src-tauri/src/send.rs); the screens never build or sign through rpc_call.
//
//   sends.feeChoices()          the three speeds, worked out when the Send tab opens (kept a minute)
//   sends.prepare({…})          build and fund a send; shows its fee and total, nothing is signed
//   sends.confirm(id)           sign and send exactly that; wrap it in withUnlock
//   sends.speedUpQuote(txid)    what Speed up costs at each speed
//   sends.speedUp(txid, speed)  bumpfee for the quoted fee; wrap it in withUnlock
//   sends.history(page)         History, newest first, 25 to a page
//   sends.exportCsv()           all of History as a CSV file in Documents
//   $sendLog / loadSendLog()    this app's sends (speed, Max, change, replacements)
//
// Amounts are whole sats. The browser build (PWA) has no Rust: it sends with the node's default fee
// (sendtoaddress), reads History from listtransactions, and has no Max, speeds, Speed up or export.

import { writable } from "svelte/store";
import { api, tauriInvoke, type Transaction, type WalletTx } from "./api";
import { fmtEcx, SATS_PER_ECX } from "./amount";
import { BASE_TICKER } from "./brand";
import type { ReceiptRow } from "./receipts";

export type Speed = "next" | "hour" | "cheap";

export const SPEED_LABEL: Record<Speed, string> = {
  next: "Next block",
  hour: "Within an hour",
  cheap: "Cheapest",
};

export interface FeeChoice {
  speed: Speed;
  label: string;
  /** sats per 1,000 vbytes */
  sat_per_kvb: number;
  sat_per_vb: number;
}

export interface FeeChoices {
  /** Next block, Within an hour, Cheapest */
  choices: FeeChoice[];
  /** All three are the same: show one line. */
  same: boolean;
  /** "quiet": the mempool fits in a block; "estimates"; "partial": some missing; "none": no fee
   *  data yet on a busy mempool, so the minimum. */
  basis: "quiet" | "estimates" | "partial" | "none";
  mempool_vbytes: number;
  floor_sat_per_kvb: number;
  at: number;
}

/** A send built and funded, waiting for Confirm (five minutes). */
export interface PreparedSend {
  id: string;
  address: string;
  /** What the address gets */
  amount: number;
  /** null in the browser build: the node picks the fee as it sends */
  fee: number | null;
  total: number | null;
  sat_per_kvb: number;
  sat_per_vb: number;
  vsize: number;
  /** 0: no change, so it can't be sped up later */
  change: number;
  max: boolean;
  speed: Speed;
  label: string;
  expires_in: number;
}

export interface SentSend {
  txid: string;
  address: string;
  amount: number;
  fee: number | null;
  sat_per_vb: number;
  speed: Speed;
  label: string;
  max: boolean;
  change: number;
  time: number;
  /** Sent, but the send log couldn't be written */
  log_error: string | null;
}

export interface SendLogEntry {
  txid: string;
  time: number;
  address: string;
  amount: number;
  fee: number;
  feerate: number;
  speed: Speed;
  max: boolean;
  change: number;
  replaced_by: string | null;
  replaces: string | null;
}

export interface BumpChoice {
  speed: Speed;
  label: string;
  /** The new total fee */
  fee: number;
  /** What it adds; it comes out of the change */
  extra: number;
  sat_per_vb: number;
  /** The change can pay it */
  ok: boolean;
}

export interface BumpQuote {
  txid: string;
  old_fee: number;
  old_sat_per_vb: number;
  vsize: number;
  choices: BumpChoice[];
  same: boolean;
}

export interface Bumped {
  txid: string;
  old_txid: string;
  fee: number;
  old_fee: number;
  speed: Speed;
  label: string;
  sat_per_vb: number;
  log_error: string | null;
}

export interface HistoryItem extends Transaction {
  /** The amount exactly, negative for a send */
  sats: number;
  /** A send's fee */
  fee: number | null;
  replaceable: string | null;
  replaced_by: string | null;
  replaces: string | null;
  abandoned: boolean;
  speed: Speed | null;
  max: boolean | null;
  /** A send from this app's Send tab */
  logged: boolean;
}

export interface HistoryPage {
  items: HistoryItem[];
  page: number;
  per_page: number;
  more: boolean;
}

export interface CsvSaved {
  path: string;
  rows: number;
}

const isPWA = api.isPWA();
const PER_PAGE = 25;

/** This app's sends, newest first (empty in the browser build). */
export const sendLog = writable<SendLogEntry[]>([]);

export async function loadSendLog(): Promise<void> {
  if (isPWA) return;
  try {
    sendLog.set((await tauriInvoke("send_log")) as SendLogEntry[]);
  } catch {
    // No node or no log yet: keep what we had.
  }
}

// The browser build's one prepared send (it has no Rust to keep it in).
let pwaPrepared: PreparedSend | null = null;

export const sends = {
  /** Max, speeds and the fee shown first: the desktop app only. */
  canChooseFee: !isPWA,
  canSpeedUp: !isPWA,
  canExport: !isPWA,

  /** The three speeds; null in the browser build. */
  async feeChoices(refresh = false): Promise<FeeChoices | null> {
    if (isPWA) return null;
    return (await tauriInvoke("fee_choices", { refresh })) as FeeChoices;
  },

  /** Build and fund a send. `amount` in sats (ignored with `max`). Nothing is signed or sent. */
  async prepare(r: { address: string; amount: number | null; max: boolean; speed: Speed }): Promise<PreparedSend> {
    if (!isPWA) {
      return (await tauriInvoke("send_prepare", {
        address: r.address,
        amount: r.max ? null : r.amount,
        max: r.max,
        speed: r.speed,
      })) as PreparedSend;
    }
    const address = r.address.trim();
    if (!address) throw new Error("Enter the address to send to.");
    if (r.max || !r.amount) throw new Error("Enter an amount in ECX above zero, with at most 8 decimal places.");
    pwaPrepared = {
      id: "pwa",
      address,
      amount: r.amount,
      fee: null,
      total: null,
      sat_per_kvb: 0,
      sat_per_vb: 0,
      vsize: 0,
      change: 0,
      max: false,
      speed: "next",
      label: "Your node's default",
      expires_in: 300,
    };
    return pwaPrepared;
  },

  /** Sign and send what prepare built. Wrap it in withUnlock. */
  async confirm(id: string): Promise<SentSend> {
    if (!isPWA) return (await tauriInvoke("send_confirm", { id })) as SentSend;
    const p = pwaPrepared;
    if (!p || p.id !== id) throw new Error("This send's quote has expired, so nothing was sent. Review it again.");
    const txid = await api.sendTransaction(p.address, p.amount / SATS_PER_ECX);
    pwaPrepared = null;
    return { ...p, txid, fee: null, time: Math.floor(Date.now() / 1000), log_error: null };
  },

  async speedUpQuote(txid: string): Promise<BumpQuote> {
    return (await tauriInvoke("speed_up_quote", { txid })) as BumpQuote;
  },

  /** Speed up for the quoted fee. Wrap it in withUnlock. */
  async speedUp(txid: string, speed: Speed): Promise<Bumped> {
    return (await tauriInvoke("send_speed_up", { txid, speed })) as Bumped;
  },

  /** A page of History (0 = the newest), newest first. */
  async history(page: number): Promise<HistoryPage> {
    if (!isPWA) return (await tauriInvoke("history", { page })) as HistoryPage;
    // listtransactions without a skip: take enough of the newest and cut the page out.
    const want = (page + 1) * PER_PAGE + 1;
    const all = (await api.getTransactions(want)).slice().reverse();
    const items = all.slice(page * PER_PAGE, (page + 1) * PER_PAGE).map(
      (t): HistoryItem => ({
        ...t,
        sats: Math.round(t.amount * SATS_PER_ECX),
        fee: null,
        replaceable: null,
        replaced_by: null,
        replaces: null,
        abandoned: false,
        speed: null,
        max: null,
        logged: false,
      }),
    );
    return { items, page, per_page: PER_PAGE, more: all.length > (page + 1) * PER_PAGE };
  },

  /** All of History as a CSV file in Documents; says where. */
  async exportCsv(): Promise<CsvSaved> {
    return (await tauriInvoke("history_csv")) as CsvSaved;
  },
};

// ---- Words and rows for the screens ----

/** "1 sat/vB", "2.5 sat/vB": a tenth is as fine as a choice needs (a bump pays 2.009, say). */
export function rateText(satPerVb: number): string {
  return `${satPerVb.toLocaleString("en-US", { maximumFractionDigits: 1 })} sat/vB`;
}

export function shortAddr(a: string): string {
  return a.length > 20 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a;
}

export function ecxText(sats: number): string {
  return `${fmtEcx(sats)} ${BASE_TICKER}`;
}

/** The receipt's rows for a send. */
export function sendRows(s: { amount: number; fee: number | null; label: string; sat_per_vb: number; address: string }): ReceiptRow[] {
  const rows: ReceiptRow[] = [{ label: "Amount", value: ecxText(s.amount) }];
  if (s.fee !== null) {
    rows.push({ label: "Fee", value: ecxText(s.fee) });
    rows.push({ label: "Speed", value: `${s.label} · ${rateText(s.sat_per_vb)}` });
  }
  rows.push({ label: "To", value: s.address, mono: true });
  return rows;
}

/** A receipt's rows after a Speed up: the new fee and speed, and the transaction it replaced. */
export function bumpedRows(rows: ReceiptRow[], b: Bumped): ReceiptRow[] {
  const out = rows.map((r) =>
    r.label === "Fee"
      ? { ...r, value: ecxText(b.fee) }
      : r.label === "Speed"
        ? { ...r, value: `${b.label} · ${rateText(b.sat_per_vb)}` }
        : r,
  );
  out.push({ label: "Replaced", value: b.old_txid, mono: true });
  return out;
}

/** What a History row's details show: from gettransaction and the send log. */
export interface TxView {
  txid: string;
  what: string;
  sentAt: number;
  rows: ReceiptRow[];
  /** Why an unconfirmed send made elsewhere can't be sped up ("" when it can, or isn't a send). */
  note: string;
}

interface TxDetail {
  category?: string;
  address?: string;
  amount?: number;
}

const sats = (ecx: unknown): number => Math.round(Number(ecx) * SATS_PER_ECX);

export function describeTx(t: WalletTx, log: SendLogEntry[]): TxView {
  const details = (Array.isArray(t.details) ? t.details : []) as TxDetail[];
  const sent = details.find((d) => d.category === "send");
  const entry = log.find((e) => e.txid === t.txid) ?? null;
  const rows: ReceiptRow[] = [];
  let what: string;
  let note = "";
  if (sent || Number(t.amount) < 0) {
    const amount = entry ? entry.amount : Math.abs(sats(sent?.amount ?? t.amount));
    const to = entry?.address ?? sent?.address ?? "";
    what = `Sent ${ecxText(amount)}${to ? ` to ${shortAddr(to)}` : ""}`;
    rows.push({ label: "Amount", value: ecxText(amount) });
    if (t.fee !== undefined) rows.push({ label: "Fee", value: ecxText(Math.abs(sats(t.fee))) });
    if (entry) rows.push({ label: "Speed", value: SPEED_LABEL[entry.speed] ?? entry.speed });
    if (to) rows.push({ label: "To", value: to, mono: true });
    if (!entry) {
      note =
        t["bip125-replaceable"] === "no"
          ? "This send wasn't marked replaceable, so it can't be sped up. Sends made before FreeBank app v0.2.0 weren't."
          : "Only sends made in this app's Send tab can be sped up.";
    }
  } else {
    const got = details.find((d) => d.category === "receive" || d.category === "generate" || d.category === "immature");
    const amount = sats(got?.amount ?? t.amount);
    what = `Received ${ecxText(amount)}`;
    rows.push({ label: "Amount", value: ecxText(amount) });
    if (got?.address) rows.push({ label: "At", value: got.address, mono: true });
  }
  const replaces = entry?.replaces ?? (typeof t.replaces_txid === "string" ? t.replaces_txid : null);
  const replacedBy = entry?.replaced_by ?? (typeof t.replaced_by_txid === "string" ? t.replaced_by_txid : null);
  if (replaces) rows.push({ label: "Replaced", value: replaces, mono: true });
  if (replacedBy) rows.push({ label: "Replaced by", value: replacedBy, mono: true });
  const time = Number(t.time) || Number(t.timereceived) || 0;
  return { txid: t.txid, what, sentAt: time ? time * 1000 : Date.now(), rows, note };
}
