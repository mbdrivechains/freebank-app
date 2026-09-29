// Settings > Security and Home's red items (v0.2.0). The Rust security_check (src-tauri/src/security.rs)
// checks the wallet's passphrase, the node's RPC and ZMQ ports, the peer port, the wallet backups the app
// made, the node program's signature, who else can read the keys on disk, and the eCash side. It runs
// when the app connects (App.svelte), on each visit to Home, and on "Check again". Red items stay on Home
// until a check finds them fixed. The desktop app only: the browser build can't look at files or ports.

import { derived, writable } from "svelte/store";
import { api, tauriInvoke } from "./api";

export type Level = "ok" | "info" | "warn" | "red";

export interface SecurityItem {
  /** wallet, rpc, zmq, p2p, backups, signature, files, stack */
  id: string;
  level: Level;
  title: string;
  /** What was found and why it matters. */
  detail: string;
  /** How to fix it; "" when there is nothing to do. */
  fix: string;
  /** Files "Show in folder" can open (unencrypted backups). */
  files?: string[];
}

export interface SecurityReport {
  /** Worst first. */
  items: SecurityItem[];
  /** ms since the epoch */
  at: number;
  /** The last run failed (the items are the run before). */
  error: string;
}

export const security = writable<SecurityReport | null>(null);
export const securityReds = derived(security, (s) => s?.items.filter((i) => i.level === "red") ?? []);
/** Home's "How to fix" asks Settings to bring its Security card into view. */
export const focusSecurity = writable(false);

let running: Promise<void> | null = null;

/** Run every check now; a run already under way is shared. Never throws. */
export function runSecurityCheck(): Promise<void> {
  if (api.isPWA()) return Promise.resolve();
  if (running) return running;
  running = (async () => {
    try {
      const items = await tauriInvoke("security_check");
      security.set({ items: Array.isArray(items) ? (items as SecurityItem[]) : [], at: Date.now(), error: "" });
    } catch (e) {
      security.update((s) => ({ items: s?.items ?? [], at: Date.now(), error: String(e) }));
    } finally {
      running = null;
    }
  })();
  return running;
}

/** Open the folder of a backup the app made (the Rust side refuses any other path). */
export function revealFile(path: string): Promise<void> {
  return tauriInvoke("security_reveal", { path }) as Promise<void>;
}
