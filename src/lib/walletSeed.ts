// The wallet's protection (v0.2.0): the passphrase first, the app's 24 recovery words, backup and
// restore. Desktop only: the Rust commands in src-tauri/src/recovery/.
//
//   $protection          null until loaded, then the wallet's state (Rust wallet_protection)
//   $walletProtected     null until known; true once the wallet has a passphrase and its HD seed comes
//                        from FreeBank's words. Always true in the browser build, which can't manage it.
//   $holdAddresses       true while a NEW wallet (no transactions yet) isn't protected: address screens
//                        wait (App.svelte's canShowAddresses: Receive and the Deposit panel), and Home's
//                        banner offers "Protect my wallet". An address handed out before the passphrase
//                        would come from the seed encryptwallet replaces.
//   loadProtection()     asks again; call it after anything that changes the wallet
//   openWalletFlow(req)  opens the wallet flow over any screen (WalletGate.svelte shows it)
//   $passphraseChanged   goes up by one each time the passphrase changes: anything holding the old one
//                        (the phone module's "Let my phone send") must forget it
//
// Passphrases and words pass through here to Rust and aren't kept: never log them, never put them on
// the clipboard.

import { derived, writable } from "svelte/store";
import { api, tauriInvoke } from "./api";

export type AppSeed = "none" | "matches" | "other";

export interface Protection {
  encrypted: boolean;
  /** Unix time (node clock) the wallet locks again; 0 while locked or without a passphrase. */
  unlocked_until: number;
  hd_seed_id: string | null;
  /** Whether FreeBank's saved words give this wallet's HD seed. */
  app_seed: AppSeed;
  protected: boolean;
  /** The user has shown they have the current words (three given back, or all typed to restore). */
  words_confirmed: boolean;
  seed_set_at: number | null;
  backup_at: number | null;
  /** No backup since the wallet got its current HD seed. */
  backup_due: boolean;
  txcount: number;
  /** No transactions yet. */
  new_wallet: boolean;
  /** The app runs this node and can restart it. */
  node_is_ours: boolean;
  seed_file_problem: string | null;
}

export interface WalletInfo extends Protection {
  wallet_file: string | null;
  wallet_name: string;
  balance_sats: number;
  unconfirmed_sats: number;
  immature_sats: number;
  keypool: number;
  keypool_change: number;
  seed_file: string;
  backup_path: string | null;
  bip85_path: string;
}

export type SetupStage = "check" | "aside" | "start" | "encrypt" | "restart" | "seed" | "save" | "scan" | "done";

export interface SetupProgress {
  running: boolean;
  done: boolean;
  kind: "new" | "restore-words" | "restore-file" | "";
  stages: SetupStage[];
  stage: SetupStage;
  note: string | null;
  error: string | null;
  /** Another program runs the node, which stopped to finish encrypting: start it again there. */
  waiting_for_node: boolean;
  moved_aside: string | null;
  words_ready: boolean;
  scan_at: number | null;
  scan_to: number | null;
}

export interface WordsCheck {
  count: number;
  /** Positions (from 1) of words that aren't recovery words. */
  unknown: number[];
  ok: boolean;
  checksum_failed: boolean;
}

export interface Revealed {
  words: string[] | null;
  xprv: string | null;
  matches_wallet: boolean | null;
}

export interface Changed {
  seed_file: "updated" | "none" | "kept";
  note: string | null;
}

export interface BackupFile {
  token: string;
  size: number;
  encrypted: boolean | null;
  hd: boolean;
}

export interface MovePlan {
  coins: number;
  total_sats: number;
  later: number;
  has_notes: boolean;
}

export interface Moved {
  txid: string;
  coins: number;
  sent_sats: number;
  fee_sats: number;
  to: string;
  later: number;
}

export const WORD_COUNT = 24;

/** The largest file taken as a wallet backup (the app checks it too). */
export const MAX_BACKUP_BYTES = 64 * 1024 * 1024;

function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

export const walletSeed = {
  protection: () => tauriInvoke("wallet_protection") as Promise<Protection>,
  info: () => tauriInvoke("wallet_info") as Promise<WalletInfo>,
  /** words: null for new words. fresh: move the current wallet aside first (restore into a new wallet). */
  setupStart: (passphrase: string, words: string | null, fresh: boolean) =>
    tauriInvoke("wallet_setup_start", { passphrase, words, fresh }) as Promise<void>,
  setupProgress: () => tauriInvoke("wallet_setup_progress") as Promise<SetupProgress>,
  /** The new words, once. */
  setupWords: () => tauriInvoke("wallet_setup_words") as Promise<string[] | null>,
  wordsConfirmed: () => tauriInvoke("wallet_words_confirmed") as Promise<void>,
  checkWords: (words: string) => tauriInvoke("seed_check_words", { words }) as Promise<WordsCheck>,
  reveal: (passphrase: string, what: "words" | "xprv") => tauriInvoke("wallet_reveal", { passphrase, what }) as Promise<Revealed>,
  changePassphrase: (old: string, next: string) =>
    tauriInvoke("wallet_change_passphrase", { old, new: next }) as Promise<Changed>,
  backupNow: () => tauriInvoke("wallet_backup_now") as Promise<string[]>,
  /** The chosen file's bytes, as base64 in plain JSON. */
  restoreFileCheck: (bytes: Uint8Array) => tauriInvoke("wallet_restore_file_check", { data: toBase64(bytes) }) as Promise<BackupFile>,
  restoreFileStart: (token: string) => tauriInvoke("wallet_restore_file_start", { token }) as Promise<void>,
  movePlan: () => tauriInvoke("wallet_move_plan") as Promise<MovePlan>,
  moveCoins: () => tauriInvoke("wallet_move_coins") as Promise<Moved>,
};

const desktop = !api.isPWA();

export const protection = writable<Protection | null>(null);

export const walletProtected = derived(protection, (p): boolean | null => (!desktop ? true : p ? p.protected : null));

export const holdAddresses = derived(protection, (p) => desktop && !!p && !p.protected && p.new_wallet);

/** Home's banner (WalletBanner) is asking for the passphrase: the wallet has none. The Security
 *  panel's wallet-passphrase item defers to it on Home (SecurityAlerts); Settings > Security still
 *  lists it. */
export const bannerAsksPassphrase = derived(protection, (p) => desktop && !!p && !p.encrypted);

export const passphraseChanged = writable(0);

/** Ask the node again. A node that is starting or unreachable leaves the last answer in place. */
export async function loadProtection(): Promise<Protection | null> {
  if (!desktop) return null;
  try {
    const p = await walletSeed.protection();
    protection.set(p);
    return p;
  } catch {
    return null;
  }
}

/** Which flow WalletGate shows over the screens. */
export type WalletFlowRequest =
  /** The passphrase first, then new or restored words. `firstRun`: opened by itself for a new wallet. */
  | { kind: "protect"; firstRun?: boolean }
  /** Settings: the words typed into a new wallet (the current one moves aside). */
  | { kind: "restore-words" }
  /** Show the saved words again (behind the passphrase) and ask for three of them back. */
  | { kind: "confirm-words" }
  /** Move coins onto the new words' addresses. */
  | { kind: "move" }
  /** Settings: a checked backup file into the wallet's place. */
  | { kind: "restore-file"; file: BackupFile & { name: string } };

export const walletFlow = writable<WalletFlowRequest | null>(null);

export function openWalletFlow(req: WalletFlowRequest): void {
  walletFlow.set(req);
}

export function closeWalletFlow(): void {
  walletFlow.set(null);
  loadProtection();
}

/** A rough strength for the hint under a new passphrase: never a rule, only advice. */
export function passphraseStrength(p: string): { level: 0 | 1 | 2 | 3; label: string } {
  if (!p) return { level: 0, label: "" };
  const pool =
    (/[a-z]/.test(p) ? 26 : 0) + (/[A-Z]/.test(p) ? 26 : 0) + (/\d/.test(p) ? 10 : 0) + (/[^a-zA-Z0-9]/.test(p) ? 33 : 0);
  let bits = p.length * Math.log2(Math.max(pool, 2));
  // Repeats ("aaaa", "abab") count for less.
  if (new Set(p).size < p.length / 2) bits *= 0.5;
  if (p.length < 8 || bits < 40) return { level: 0, label: "Weak: anyone who gets your wallet file could guess it" };
  if (bits < 60) return { level: 1, label: "Fair: longer is better" };
  if (bits < 80) return { level: 2, label: "Good" };
  return { level: 3, label: "Strong" };
}

/** "1 Oct 2026, 14:05" in the user's own format. */
export function when(unix: number | null): string {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** Three different word positions (1 to 24), in order, for "give three words back". */
export function threePositions(): number[] {
  const picked = new Set<number>();
  const r = new Uint32Array(1);
  while (picked.size < 3) {
    crypto.getRandomValues(r);
    picked.add((r[0] % WORD_COUNT) + 1);
  }
  return [...picked].sort((a, b) => a - b);
}
