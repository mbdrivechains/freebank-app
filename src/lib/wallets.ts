// Several wallets in one app (v0.2.6; src-tauri/src/wallets.rs). The header's switcher chooses the wallet Home, Send,
// Receive and Credit use; the phone always uses the main one.

import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

export interface WalletView {
  /** null: the main wallet. */
  name: string | null;
  label: string;
  kind: "main" | "words" | "file";
  /** sECX; null when the node didn't answer. */
  balance: number | null;
  encrypted: boolean | null;
  active: boolean;
}

/** The wallets as last read: the header's switcher shows when there are more than one. */
export const walletList = writable<WalletView[]>([]);

export async function loadWallets(): Promise<WalletView[]> {
  const w = (await tauriInvoke("wallets_list")) as WalletView[];
  walletList.set(w);
  return w;
}

export const walletSelect = (name: string | null) => tauriInvoke("wallet_select", { name }) as Promise<void>;
export async function walletForget(name: string) {
  walletList.set((await tauriInvoke("wallet_forget", { name })) as WalletView[]);
}
export async function walletAddWords(label: string, passphrase: string) {
  walletList.set((await tauriInvoke("wallet_add_words", { label, passphrase })) as WalletView[]);
}
export async function walletAddFile(label: string, data: string) {
  walletList.set((await tauriInvoke("wallet_add_file", { label, data })) as WalletView[]);
}
