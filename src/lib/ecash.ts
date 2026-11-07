// The eCash tab (v0.2.6): the app's own eCash wallets in the eCash node (src-tauri/src/ecash/). Amounts are sats;
// ECX and sECX are at par, so the sECX helpers in amount.ts format and parse them too.

import { tauriInvoke } from "./api";
import type { CsvSaved } from "./send";

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
  /** Core's category, or "deposit" ("deposit_failed": it can't confirm): a deposit into FreeBank, from the app's record. */
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

/** v0.7.3: the main eCash wallet for Sparrow or Electrum (ecash_export): its public side, no passphrase. */
export interface EcashExport {
  /** "test": Bitcoin's test prefixes (easynet), so the other wallet runs as testnet. */
  chain: "main" | "test" | "regtest";
  fingerprint: string;
  path: string;
  zpub: string;
  xpub: string;
  /** Receive and change in one (BIP389), for Sparrow's Import Wallet › Output Descriptor. */
  descriptor: string;
  first_address: string;
  /** The eCash node, host:port, and its data folder when FreeBank runs it: for Sparrow's Bitcoin Core server. */
  node: string | null;
  node_dir: string | null;
}

/** Its private side, after the passphrase: the eCash root (Sparrow) and the account's zprv (Electrum). */
export interface EcashSecret {
  master_xprv: string;
  zprv: string;
}

/** v0.7.3: another wallet's coins on eCash, from its words (ecash_other_find), and their move into the main wallet. */
export interface OtherFound {
  /** The prepared move ("" when nothing was found). */
  id: string;
  kind: "bip39" | "electrum";
  path: string;
  first_address: string;
  coins: number;
  sats: number;
  /** What this move takes, of those. */
  taking: number;
  taking_sats: number;
  /** Coins worth less than the fee to move them (they stay), and coins beyond what one move takes. */
  small: number;
  left_out: number;
  /** The main eCash wallet's address they go to. */
  to: string;
  arrives: number;
  fee: number;
}

export const ecashStatus = () => tauriInvoke("ecash_status") as Promise<EcashStatus>;
export const ecashOtherFind = (words: string, passphrase: string) =>
  tauriInvoke("ecash_other_find", { words, passphrase }) as Promise<OtherFound>;
export const ecashOtherMove = (id: string, words: string, passphrase: string) =>
  tauriInvoke("ecash_other_move", { id, words, passphrase }) as Promise<string>;
export const ecashExport = () => tauriInvoke("ecash_export") as Promise<EcashExport>;
export const ecashExportPrivate = (passphrase: string) =>
  tauriInvoke("ecash_export_private", { passphrase }) as Promise<EcashSecret>;
export const ecashSetup = (passphrase: string) => tauriInvoke("ecash_setup", { passphrase }) as Promise<EcashStatus>;
export const ecashReceive = () => tauriInvoke("ecash_receive") as Promise<string>;
export const ecashHistory = () => tauriInvoke("ecash_history") as Promise<EcashTx[]>;
/** v0.6.8: every payment of both eCash wallets as a CSV file in Documents; says where. */
export const ecashHistoryCsv = () => tauriInvoke("ecash_history_csv") as Promise<CsvSaved>;
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
