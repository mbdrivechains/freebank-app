// Addresses given out (v0.6.8; Rust given.rs): every address Receive hands out, here or to a paired phone, with when
// and a note of what it is for, listed in Settings › Wallet.

import { tauriInvoke } from "./api";

export interface GivenRow {
  address: string;
  /** "freebank" or "ecash" */
  kind: string;
  /** The FreeBank wallet it came from, when not the main one. */
  wallet: string | null;
  /** Unix seconds; 0 for one given out before the list was kept. */
  at: number;
  /** "app", or "phone: <its name>"; "" for one given out before the list was kept. */
  via: string;
  note: string;
  /** What it has received, in coins; null when its wallet wasn't asked. */
  received: number | null;
}

/** A note's length, in characters (given.rs NOTE_MAX). */
export const NOTE_MAX = 120;

export const given = {
  list: () => tauriInvoke("addresses_given") as Promise<GivenRow[]>,
  note: (address: string, kind: "freebank" | "ecash", note: string) =>
    tauriInvoke("addresses_note", { address, kind, note }) as Promise<void>,
};
