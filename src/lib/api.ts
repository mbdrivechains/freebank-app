// API wrapper - supports both Tauri (desktop) and PWA (browser) modes

import { CONFIG_KEY } from "./brand";

export interface Transaction {
  txid: string;
  amount: number;
  confirmations: number;
  time: number;
  address: string | null;
  category: "send" | "receive" | string;
}

export interface BlockchainInfo {
  chain: string;
  blocks: number;
  headers: number;
  bestblockhash: string;
  difficulty: number;
  verification_progress: number;
}

export interface ConnectionConfig {
  host: string;
  port: number;
  user: string;
  password: string;
}

export interface NoteHolding {
  house_id: number;
  units: number;
  demanded_units: number;
  coins: number;
  house_status: string; // effective status: o/s/d/i/w
  redeemable: boolean;
  redeemable_units?: number; // node v0.2.18: how many units can be redeemed now (while suspended, the demanded ones)
  demandable: boolean;
  house_tier?: number;
  house_minted_units?: number;
}

export interface House {
  id: number;
  classid: string;          // the note-class name
  tier: number;
  lambdax10: number;        // leverage λ × 10 (tier's max notes per unit reserve)
  status: string;           // stored status
  effective_status: string; // consensus-operative status at the tip: open/stressed/deferred/insolvent/wounddown
  mintedunits: number;      // outstanding note liabilities (units, 1 = 1 sat)
  mintcapunits: number;     // max mintable given reserves
  activeescrow: number;     // pledged reserve (ECX)
  attestedratiobps: number; // last attested reserve ratio (basis points)
  lastattestheight: number;
  lastattestreserves: number; // ECX
  denominationmggold: number; // unit-of-account label (mg gold); inert in v1
  defer_interest_bps?: number; // node v0.2.18: the yearly rate on demands queued while suspended (1000 = 10%)
  [k: string]: unknown;
}

// A constant-product AMM pool: house notes on one side, base ECX on the other.
export interface Pool {
  pool_id: number;              // the house id this pool trades notes for
  house_id: number;
  fee_bps: number;              // swap fee (basis points)
  note_reserve: number;         // note units held by the pool
  btx_reserve: number;          // base ECX held by the pool (sats)
  lp_supply: number;            // total LP units outstanding
  locked_lp: number;            // permanently-locked LP units (bootstrap dust)
  spot_price_sats_x1e8: number; // marginal price, sats per note unit × 1e8
  createheight: number;
}

// This wallet's liquidity position in one pool.
export interface LpHolding {
  pool_id: number;
  lp_units: number;      // LP units this wallet holds
  lp_supply: number;     // total LP units outstanding
  share_bps: number;     // this wallet's pool share (basis points)
  my_note_units: number; // note units redeemable for lp_units at current reserves
  my_btx_sats: number;   // base ECX (sats) redeemable for lp_units at current reserves
  note_reserve: number;
  btx_reserve: number;
  fee_bps: number;
}

// A bill of exchange — the discount-house asset side. Pubkey-addressed (endorse
// to a holder pubkey). Status: a=active, r=retired, d=defaulted, x=disputed.
export interface Bill {
  id: number;
  bill_id: string;
  amount: number;        // face amount (ECX)
  escrow: number;        // acceptor's escrow bond (ECX)
  status: string;        // a / r / d / x
  issued_height: number;
  maturity_height: number;
  grace_blocks: number;
  drawer_pubkey?: string;
  acceptor_pubkey?: string;
  holder_pubkey?: string;
  roles?: string[];      // this wallet's role(s): holder / drawer / acceptor
  [k: string]: unknown;
}

/** The wallet's lock (Rust wallet_status). unlocked_until is unix seconds on the node's clock, 0 while
 *  locked, and always 0 for a wallet without a passphrase: check `encrypted` first. */
export interface WalletStatus {
  encrypted: boolean;
  unlocked_until: number;
}

/** gettransaction's answer, the fields the receipt uses. confirmations < 0: a conflicting
 *  transaction confirmed (it was replaced). */
export interface WalletTx {
  txid: string;
  confirmations: number;
  blockhash?: string;
  blocktime?: number;
  time: number;
  amount: number;
  fee?: number;
  "bip125-replaceable"?: string;
  [k: string]: unknown;
}

// Detect if running in Tauri
const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

// Storage for PWA mode connection config
let pwaConfig: ConnectionConfig | null = null;

// Load saved config from localStorage (PWA mode)
function loadSavedConfig(): ConnectionConfig | null {
  if (isTauri) return null;
  try {
    const saved = localStorage.getItem(CONFIG_KEY);
    if (saved) return JSON.parse(saved);
  } catch {}
  return null;
}

// Save config to localStorage (PWA mode)
function saveConfig(config: ConnectionConfig): void {
  if (!isTauri) {
    localStorage.setItem(CONFIG_KEY, JSON.stringify(config));
  }
}

// Expose the saved config so the UI can prefill the connect form.
export function getSavedConfig(): ConnectionConfig | null {
  return pwaConfig || loadSavedConfig();
}

// Make JSON-RPC call directly (PWA mode)
async function rpcCall(method: string, params: unknown[] = []): Promise<unknown> {
  const config = pwaConfig || loadSavedConfig();
  if (!config) {
    throw new Error('Not connected - configure RPC settings first');
  }

  const url = `http://${config.host}:${config.port}/`;
  const auth = btoa(`${config.user}:${config.password}`);

  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Basic ${auth}`,
    },
    body: JSON.stringify({
      jsonrpc: '1.0',
      id: 'pwa',
      method,
      params,
    }),
  });

  // Core answers RPC errors with HTTP 500 (404 for an unknown method) and a JSON body: read the body
  // first, and pass its code on the way the Rust commands do ("RPC error -13: …", lib/errors.ts).
  const data = await response.json().catch(() => null);
  if (data?.error) {
    throw new Error(`RPC error ${data.error.code}: ${data.error.message || 'unknown error'}`);
  }
  if (!response.ok || !data) {
    throw new Error(`HTTP error: ${response.status} ${response.statusText}`);
  }

  return data.result;
}

// Dynamic import for Tauri (only loads in Tauri environment)
export async function tauriInvoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke(cmd, args);
}

// Generic FreeBank RPC (notes/houses/pools/bills) — one path for both modes.
// Desktop routes through the Rust rpc_call passthrough; PWA fetches JSON-RPC directly.
export async function fbCall(method: string, params: unknown[] = []): Promise<unknown> {
  if (isTauri) return tauriInvoke('rpc_call', { method, params });
  return rpcCall(method, params);
}

export const api = {
  /** Check if running as PWA (not Tauri) */
  isPWA(): boolean {
    return !isTauri;
  },

  /** Connect to freebankd node */
  async connectNode(config: ConnectionConfig): Promise<boolean> {
    if (isTauri) {
      return tauriInvoke('connect_node', { config }) as Promise<boolean>;
    } else {
      // PWA mode - test connection with getblockchaininfo
      pwaConfig = config;
      try {
        await rpcCall('getblockchaininfo');
        saveConfig(config);
        return true;
      } catch (e) {
        pwaConfig = null;
        throw e;
      }
    }
  },

  /** Check connection status */
  async getConnectionStatus(): Promise<boolean> {
    if (isTauri) {
      return tauriInvoke('get_connection_status') as Promise<boolean>;
    } else {
      try {
        await rpcCall('getblockchaininfo');
        return true;
      } catch {
        return false;
      }
    }
  },

  /** Get blockchain info */
  async getBlockchainInfo(): Promise<BlockchainInfo> {
    if (isTauri) {
      return tauriInvoke('get_blockchain_info') as Promise<BlockchainInfo>;
    } else {
      return rpcCall('getblockchaininfo') as Promise<BlockchainInfo>;
    }
  },

  /** Get wallet balance */
  async getBalance(): Promise<number> {
    if (isTauri) {
      return tauriInvoke('get_balance') as Promise<number>;
    } else {
      return rpcCall('getbalance') as Promise<number>;
    }
  },

  /** Generate new receiving address */
  async getNewAddress(): Promise<string> {
    if (isTauri) {
      return tauriInvoke('get_new_address') as Promise<string>;
    } else {
      return rpcCall('getnewaddress', ['', 'legacy']) as Promise<string>;
    }
  },

  /** Send ECX to address */
  async sendTransaction(address: string, amount: number): Promise<string> {
    if (isTauri) {
      return tauriInvoke('send_transaction', { address, amount }) as Promise<string>;
    } else {
      return rpcCall('sendtoaddress', [address, amount]) as Promise<string>;
    }
  },

  /** Get recent transactions */
  async getTransactions(count?: number): Promise<Transaction[]> {
    if (isTauri) {
      return tauriInvoke('get_transactions', { count }) as Promise<Transaction[]>;
    } else {
      return rpcCall('listtransactions', ['*', count || 10]) as Promise<Transaction[]>;
    }
  },

  // ---- The wallet's lock (v0.2.0: the passphrase is required; lib/wallet.ts withUnlock) ----

  /** Encrypted or not, and until when it is unlocked */
  async walletStatus(): Promise<WalletStatus> {
    if (isTauri) return tauriInvoke('wallet_status') as Promise<WalletStatus>;
    const info = (await rpcCall('getwalletinfo')) as { unlocked_until?: number };
    return info.unlocked_until === undefined
      ? { encrypted: false, unlocked_until: 0 }
      : { encrypted: true, unlocked_until: Math.max(0, info.unlocked_until) };
  },

  /** Unlock for `seconds` (clamped to 1..300). A wrong passphrase rejects with "RPC error -14: …". */
  async walletUnlock(passphrase: string, seconds: number): Promise<WalletStatus> {
    if (isTauri) return tauriInvoke('wallet_unlock', { passphrase, seconds: Math.round(seconds) }) as Promise<WalletStatus>;
    if (!passphrase) throw new Error('Please enter your wallet passphrase.');
    await rpcCall('walletpassphrase', [passphrase, Math.min(300, Math.max(1, Math.round(seconds)))]);
    return api.walletStatus();
  },

  /** Lock now (nothing to do for a wallet without a passphrase) */
  async walletLock(): Promise<WalletStatus> {
    if (isTauri) return tauriInvoke('wallet_lock') as Promise<WalletStatus>;
    await rpcCall('walletlock').catch((e) => {
      if (!/RPC error -15:/.test(String(e))) throw e;
    });
    return api.walletStatus();
  },

  // ---- Following a transaction (TxReceipt) ----

  async getBlockCount(): Promise<number> {
    return fbCall('getblockcount') as Promise<number>;
  },

  /** A wallet transaction; "RPC error -5: …" when this wallet doesn't know it */
  async getTransaction(txid: string): Promise<WalletTx> {
    return fbCall('gettransaction', [txid]) as Promise<WalletTx>;
  },

  async getBlockHeader(hash: string): Promise<{ hash: string; height: number; time: number }> {
    return fbCall('getblockheader', [hash, true]) as Promise<{ hash: string; height: number; time: number }>;
  },

  /** The node's network info (for its version: subversion "/FreeBank:0.2.15/") */
  async getNetworkInfo(): Promise<{ subversion: string; version: number }> {
    return fbCall('getnetworkinfo') as Promise<{ subversion: string; version: number }>;
  },

  // ---- FreeBank notes (M1) ----

  /** List this wallet's note holdings, one row per house */
  async listMyNotes(): Promise<NoteHolding[]> {
    return fbCall('listmynotes') as Promise<NoteHolding[]>;
  },

  /** Mint house notes (a house op; the single-wallet regtest plays the house) */
  async mintNote(houseId: number, units: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('mintnote', [houseId, units, fee])) as { txid: string };
    return r.txid;
  },

  /** Transfer notes to a FreeBank P2PKH address (empty address = self-transfer) */
  async transferNote(houseId: number, units: number, toAddress: string, fee = 0.001): Promise<string> {
    const r = (await fbCall('transfernote', [houseId, units, fee, toAddress])) as { txid: string };
    return r.txid;
  },

  /** Redeem notes for base ECX 1:1 from the house reserves */
  async redeemNote(houseId: number, units: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('redeemnote', [houseId, units, fee])) as { txid: string };
    return r.txid;
  },

  /** Lodge a demand against a suspended house (option clause) */
  async demandNote(houseId: number, units: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('demandnote', [houseId, units, fee])) as { txid: string };
    return r.txid;
  },

  // ---- FreeBank houses (M2) ----

  /** The house directory — every note-issuing house, with soundness fields */
  async listHouses(): Promise<House[]> {
    return fbCall('listhouses') as Promise<House[]>;
  },

  /** Charter a new note-issuing house. escrowEcx = pledged reserve (ECX); solo by default. */
  async registerHouse(
    classid: string,
    tier: number,
    escrowEcx: number,
    denommg = 1000,
    threshold = 1,
    fee = 0.001,
  ): Promise<string> {
    const r = (await fbCall('registerhouse', [tier, threshold, classid, denommg, [escrowEcx], fee])) as { txid: string };
    return r.txid;
  },

  /** Attest a house's liquid reserves — the recurring soundness proof that keeps it Open */
  async attestHouse(houseId: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('attesthouse', [houseId, fee])) as { txid: string };
    return r.txid;
  },

  // ---- FreeBank pools (M3): note ⇄ ECX AMM ----

  /** The pool directory — every note/ECX constant-product pool, with reserves and spot price */
  async listPools(): Promise<Pool[]> {
    return fbCall('listpools') as Promise<Pool[]>;
  },

  /** This wallet's liquidity positions, one row per pool it has LP units in */
  async listMyLp(): Promise<LpHolding[]> {
    return fbCall('listmylp') as Promise<LpHolding[]>;
  },

  /** Swap across a pool. direction "noteforbtx" = note units in, sats out; "btxfornote" = sats in, note units out. */
  async swapNote(
    poolId: number,
    direction: "noteforbtx" | "btxfornote",
    amountIn: number,
    minOut: number,
    fee = 0.001,
  ): Promise<string> {
    const r = (await fbCall('swapnote', [poolId, direction, amountIn, minOut, fee])) as { txid: string };
    return r.txid;
  },

  /** Seed a new pool for a house (pool id = house id): initial note units + base ECX sats + fee bps */
  async createPool(poolId: number, noteUnits: number, btxSats: number, feeBps: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('createpool', [poolId, noteUnits, btxSats, feeBps, fee])) as { txid: string };
    return r.txid;
  },

  /** Add liquidity to an existing pool: note units + base ECX sats (deposited pro-rata) */
  async addLiquidity(poolId: number, noteUnits: number, btxSats: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('addpoolliquidity', [poolId, noteUnits, btxSats, fee])) as { txid: string };
    return r.txid;
  },

  /** Withdraw liquidity from a pool by burning LP units */
  async removeLiquidity(poolId: number, lpUnits: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('removepoolliquidity', [poolId, lpUnits, fee])) as { txid: string };
    return r.txid;
  },

  // ---- FreeBank bills (M4) ----

  /** Bills this wallet participates in, with our role(s) */
  async listMyBills(): Promise<Bill[]> {
    return fbCall('listmybills') as Promise<Bill[]>;
  },

  /** A fresh pubkey to receive a bill / endorsement (share it out-of-band) */
  async getNewBillPubkey(): Promise<string> {
    const r = (await fbCall('getnewbillpubkey')) as { pubkey: string };
    return r.pubkey;
  },

  /** Draw + accept a bill. bodyHex = hex of the (encrypted) body; maturityHeight absolute. */
  async issueBill(bodyHex: string, amount: number, escrow: number, maturityHeight: number, graceBlocks = 1008, fee = 0.001): Promise<string> {
    const r = (await fbCall('issuebill', [bodyHex, amount, escrow, maturityHeight, graceBlocks, fee])) as { txid: string };
    return r.txid;
  },

  /** Endorse (transfer) a bill to a new holder pubkey */
  async endorseBill(id: number, toPubkey: string, fee = 0.001): Promise<string> {
    const r = (await fbCall('endorsebill', [id, toPubkey, fee])) as { txid: string };
    return r.txid;
  },

  /** Retire (settle) a bill */
  async retireBill(id: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('retirebill', [id, fee])) as { txid: string };
    return r.txid;
  },

  /** Claim the escrow bond on a defaulted bill */
  async claimBillEscrow(id: number, fee = 0.001): Promise<string> {
    const r = (await fbCall('claimbillescrow', [id, fee])) as { txid: string };
    return r.txid;
  },
};
