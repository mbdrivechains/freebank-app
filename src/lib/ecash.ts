// The eCash tab (v0.2.6): the app's own eCash wallets in the eCash node (src-tauri/src/ecash/). Amounts are sats;
// ECX and sECX are at par, so the sECX helpers in amount.ts format and parse them too.

import { tauriInvoke } from "./api";

export interface Balance {
  /** Spendable: confirmed, and the wallet's own unconfirmed change. */
  trusted: number;
  /** Coming in, not confirmed yet. */
  pending: number;
}

export interface EcashStatus {
  /** Why the eCash wallet can't be used now (null: it can). */
  problem: string | null;
  /** The eCash node took BitWindow's default login. */
  default_login: boolean;
  state: "none" | "ready" | "missing";
  /** Wallets of other recovery words stay in the eCash node (watch-only). */
  earlier: boolean;
  has_words: boolean;
  main: Balance | null;
  bids: Balance | null;
}

export interface EcashTx {
  wallet: "main" | "bids";
  txid: string;
  category: string;
  /** Signed: out is negative. */
  sats: number;
  fee: number;
  confirmations: number;
  time: number;
  address: string | null;
}

export interface EcashQuote {
  id: string;
  address: string;
  to_bids: boolean;
  sats: number;
  fee: number;
  total: number;
}

export const ecashStatus = () => tauriInvoke("ecash_status") as Promise<EcashStatus>;
export const ecashSetup = (passphrase: string) => tauriInvoke("ecash_setup", { passphrase }) as Promise<EcashStatus>;
export const ecashReceive = () => tauriInvoke("ecash_receive") as Promise<string>;
export const ecashHistory = () => tauriInvoke("ecash_history") as Promise<EcashTx[]>;
/** No amount: everything the main wallet can spend, less the fee. */
export const ecashPrepare = (address: string, amount: string | null, toBids: boolean) =>
  tauriInvoke("ecash_send_prepare", { address, amount, toBids }) as Promise<EcashQuote>;
export const ecashConfirm = (id: string, passphrase: string) =>
  tauriInvoke("ecash_send_confirm", { id, passphrase }) as Promise<string>;
/** From the bidding wallet back to the main one; no amount: all of it. Shows the fee before anything goes. */
export const ecashBidsWithdrawPrepare = (amount: string | null) =>
  tauriInvoke("ecash_bids_withdraw_prepare", { amount }) as Promise<EcashQuote>;
export const ecashBidsWithdrawConfirm = (id: string) =>
  tauriInvoke("ecash_bids_withdraw_confirm", { id }) as Promise<string>;

export interface BmmRound {
  height: number;
  /** "live", "won", "lost", "replaced", "rejected", "failed". */
  outcome: string;
  fee: number;
  at: number;
  txid: string;
}

export interface BmmStatus {
  on: boolean;
  bid: number;
  daily_cap: number;
  spent_today: number;
  won_today: number;
  /** What the bidding loop said last: [unix time, words]. */
  last: [number, string] | null;
  rounds: BmmRound[];
}

export const bmmStatus = () => tauriInvoke("bmm_status") as Promise<BmmStatus>;
/** Amounts in ECX as typed. */
export const bmmSet = (on: boolean, bid: string, dailyCap: string) =>
  tauriInvoke("bmm_set", { on, bid, dailyCap }) as Promise<BmmStatus>;

export interface EcashLogin {
  /** The RPC address in use, host:port. */
  rpc: string;
  l1_rpc: string;
  l1_datadir: string;
  user: string;
  has_password: boolean;
}

export const ecashLoginGet = () => tauriInvoke("ecash_login_get") as Promise<EcashLogin>;
/** password: null keeps the saved one; "" forgets it. */
export const ecashLoginSet = (rpc: string, datadir: string, user: string, password: string | null) =>
  tauriInvoke("ecash_login_set", { rpc, datadir, user, password }) as Promise<EcashStatus>;
