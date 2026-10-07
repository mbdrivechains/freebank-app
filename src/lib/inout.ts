// In and out (v0.3.0): Deposit at par from the app's eCash wallet (src-tauri/src/ecash/deposit.rs) and Withdraw at
// par through the peg (src-tauri/src/withdraw.rs). Amounts are integer sats.

import { tauriInvoke } from "./api";

/** What FreeBank keeps of each deposit, for the maker of the block that credits it (freebankd's
 *  SIDECHAIN_DEPOSIT_FEE). */
export const FREEBANK_DEPOSIT_FEE = 1_000;

export interface DepositQuote {
  id: string;
  /** What goes into FreeBank's treasury. */
  sats: number;
  /** What FreeBank credits: sats less its deposit fee. */
  credited: number;
  /** The eCash fee. */
  fee: number;
  /** What leaves the eCash wallet. */
  total: number;
  /** The FreeBank address credited, and the wallet it is in (null: the main one). */
  address: string;
  fb_wallet: string | null;
}

export interface Deposit {
  txid: string;
  sats: number;
  fee: number;
  address: string;
  time: number;
  /** signed, sent, confirmed (on eCash, waiting for FreeBank), credited, failed */
  state: "signed" | "sent" | "confirmed" | "credited" | "failed";
  confirmations: number;
}

export const depositPrepare = (amount: string | null, max = false) =>
  tauriInvoke("deposit_prepare", { amount, max }) as Promise<DepositQuote>;
export const depositConfirm = (id: string, passphrase: string) =>
  tauriInvoke("deposit_confirm", { id, passphrase }) as Promise<string>;
export const depositList = () => tauriInvoke("deposit_list") as Promise<Deposit[]>;

export function depositLine(d: Deposit): string {
  switch (d.state) {
    case "signed":
      return "Waiting to go out";
    case "sent":
      return "Sent: waiting for an eCash block";
    case "confirmed":
      return `On eCash (${d.confirmations} confirmation${d.confirmations === 1 ? "" : "s"}): waiting for a FreeBank block`;
    case "credited":
      return `Credited on FreeBank: ${(Math.max(0, d.sats - FREEBANK_DEPOSIT_FEE) / 1e8).toFixed(8)} ECX`;
    case "failed":
      return "Not sent: another deposit used FreeBank's treasury first, or a coin was spent. Your eCash stays in your wallet; if FreeBank credits it after all, it shows here.";
  }
}

export interface WithdrawQuote {
  id: string;
  /** What the eCash address receives. */
  sats: number;
  /** The FreeBank fee of the withdrawal transaction. */
  fee: number;
  /** The eCash fee for the payout. */
  mainchain_fee: number;
  /** What leaves the FreeBank wallet. */
  total: number;
  address: string;
  /** A pasted address, not one of the app's own eCash wallet. */
  pasted: boolean;
}

export interface Withdrawal {
  id: string;
  sats: number;
  mainchain_fee: number;
  destination: string;
  time: number;
  state: "pending" | "waiting" | "bundled" | "paid" | "cancelling" | "refunded" | "failed" | "unknown";
}

export const withdrawPrepare = (amount: string, address: string | null) =>
  tauriInvoke("withdraw_prepare", { amount, address }) as Promise<WithdrawQuote>;
export const withdrawConfirm = (id: string) => tauriInvoke("withdraw_confirm", { id }) as Promise<Withdrawal>;
export const withdrawList = () => tauriInvoke("withdraw_list") as Promise<Withdrawal[]>;
export const withdrawCancel = (id: string) => tauriInvoke("withdraw_cancel", { id }) as Promise<void>;

export function withdrawalLine(w: Withdrawal): string {
  switch (w.state) {
    case "pending":
      return "Made: waiting for a FreeBank block";
    case "failed":
      return "Didn't go through: nothing left your wallet for it";
    case "waiting":
      return "Waiting for a bundle (you can still cancel)";
    case "bundled":
      return "In a bundle, waiting for eCash miners (can't be cancelled)";
    case "paid":
      return "Paid on eCash";
    case "cancelling":
      return "Cancelling: back in your wallet after the next FreeBank block";
    case "refunded":
      return "Cancelled: back in your wallet";
    case "unknown":
      return "Your node doesn't know this one";
  }
}

// The money changer (src-tauri/src/changer.rs; the bot is distribution/changer): sell FreeBank ECX for eCash fast
// ("out"), or buy it below par with eCash ("in"). Trusted up to one order.

export interface ChangerSide {
  most_per_order: number;
  left_today: number;
}

export interface ChangerInfo {
  out_bps: number;
  in_bps: number;
  min_order: number;
  sides: { out: ChangerSide; in: ChangerSide };
  paused: string | null;
}

export interface ChangerQuote {
  id: string;
  side: "out" | "in";
  /** What the user pays in (FreeBank ECX for out, eCash for in). */
  amount: number;
  /** What the user gets on the other chain. */
  payout: number;
  discount_bps: number;
  fee: number;
  payout_to: string;
  expires: number;
  blocks_left: number;
}

export interface ChangerOrder {
  id: string;
  side: "out" | "in";
  amount: number;
  payout: number;
  time: number;
  state: "paying" | "paid_in" | "waiting" | "said_paid" | "done" | "refunded" | "held" | "overdue" | "failed";
  note: string | null;
}

export const changerInfo = () => tauriInvoke("changer_info") as Promise<ChangerInfo | null>;
export const changerQuote = (side: "out" | "in", amount: string, address: string | null) =>
  tauriInvoke("changer_quote", { side, amount, address }) as Promise<ChangerQuote>;
export const changerPay = (id: string, passphrase: string | null) =>
  tauriInvoke("changer_pay", { id, passphrase }) as Promise<ChangerOrder>;
export const changerOrders = () => tauriInvoke("changer_orders") as Promise<ChangerOrder[]>;
export const changerGet = () => tauriInvoke("changer_get") as Promise<{ url: string; key: string }>;
export const changerSet = (url: string, key: string) => tauriInvoke("changer_set", { url, key }) as Promise<void>;

export function changerLine(o: ChangerOrder): string {
  switch (o.state) {
    case "paying":
      return "Paying in…";
    case "paid_in":
    case "waiting":
      return "Paid in: waiting for the changer to pay";
    case "said_paid":
      return o.note ?? "The changer says it has paid; not seen in your wallet yet";
    case "done":
      return "Paid out by the changer: seen in your wallet";
    case "refunded":
      return "Refunded by the changer";
    case "held":
      return `Held by the changer${o.note ? `: ${o.note}` : ""}`;
    case "overdue":
      return "Overdue: the changer hasn't paid an hour after you paid in";
    case "failed":
      return "Didn't go out";
  }
}

export const pct = (bps: number) => `${(bps / 100).toFixed(2)}%`;
