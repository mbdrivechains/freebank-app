// First run and the Node tab: typed wrappers over the Rust node commands (desktop app only).

import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

export interface Settings {
  rest: string;
  enforcer: string;
  datadir: string;
  rpc_port: number;
  p2p_port: number;
  installed_tag: string | null;
  grpcurl: string | null;
  /** The data folder the app created when it installed, if it did. */
  datadir_created: string | null;
}

export interface SetupInfo {
  settings: Settings;
  platform_error: string | null;
  suggested_tag: string;
  current_tag: string | null;
  installed: boolean;
  default_datadir: string;
  app_version: string;
}

export interface StackCheck {
  found: boolean;
  rest_ok: boolean;
  on_beta: boolean;
  enforcer_ok: boolean;
  l1_blocks: number | null;
  detail: string;
}

export type RpcState = "down" | "warming" | "up" | "locked" | "busy";

export interface Probe {
  state: RpcState;
  message: string;
  blocks: number | null;
  headers: number | null;
}

export interface DatadirCheck {
  kind: "new" | "ours" | "other";
  message: string;
  away: string | null;
  has_wallet: boolean;
}

export interface InstallProgress {
  running: boolean;
  done: boolean;
  stage: string;
  tag: string | null;
  bytes: number;
  total: number | null;
  note: string | null;
  error: string | null;
  cancelled: boolean;
}

export interface NodeProgress {
  running: boolean;
  exited: string | null;
  rpc: Probe;
  explorer_tip: number | null;
  peers: number | null;
  log_line: string | null;
}

export interface Peer {
  addr: string;
  subver: string;
  inbound: boolean;
  synced_blocks: number | null;
}

export interface Versions {
  app: string;
  node: string | null;
  commit: string | null;
}

export interface NodeStatus {
  state: RpcState;
  activity: string | null;
  message: string;
  managed: boolean;
  installed: boolean;
  exited: string | null;
  log_line: string | null;
  version: string;
  blocks: number;
  headers: number;
  explorer_tip: number | null;
  l1_blocks: number | null;
  peers: Peer[];
  tag: string | null;
  explorer: string;
  datadir: string;
  rest: string;
  enforcer: string;
  release: string | null;
  p2p_port: number;
  versions: Versions;
}

export interface ConnCheck {
  label: string;
  ok: boolean;
  detail: string;
}

export interface UpdateInfo {
  installed: string | null;
  latest: string | null;
  available: boolean;
  error: string | null;
}

export interface Removed {
  datadir: string;
  wallets: string[];
}

/** One line of the Obliterate list. The screen sends back each ticked id with the path it showed;
 * the app acts only on its own fresh list, and refuses a tick whose item now names another path. */
export interface WipeItem {
  id: string;
  kind: "app" | "node" | "aside" | "cache";
  label: string;
  path: string;
  size: number;
  /** Ticked when the list opens. */
  checked: boolean;
  /** Can be ticked at all. */
  allowed: boolean;
  note: string;
  /** The wallets in it (the node's folder and folders setup moved aside). */
  wallets: string[];
}

export interface WipeTick {
  id: string;
  path: string;
}

export interface ObliteratePlan {
  items: WipeItem[];
  wallets: string[];
  /** Everything the node's wallet holds, spendable or not yet. */
  balance: number | null;
  /** How much of balance isn't spendable yet (unconfirmed or newly mined), when any. */
  pending: number | null;
  balance_note: string | null;
  backups: string[];
  /** Why it can't run right now. */
  blocked: string | null;
}

/** How to remove the app itself: "deb" (sudo apt remove freebank), "appimage" (delete the file at
 * path), "mac" (drag it to the Trash) or "other" (delete the program at path). */
export interface RemoveApp {
  kind: "deb" | "appimage" | "mac" | "other";
  path: string | null;
}

export interface Obliterated {
  removed: string[];
  /** Deleted when the app closes. */
  at_exit: string[];
  /** On the list but not ticked. */
  kept: string[];
  backups: string[];
  app_removed: boolean;
  app: RemoveApp;
}

/** "restarted" | "external" (another program runs the node) | "saved" (the node isn't running) */
export type SetTagResult = "restarted" | "external" | "saved";

// Shared by the footer, the Node tab and Settings.
export const versions = writable<Versions | null>(null);
export const update = writable<UpdateInfo | null>(null);

export async function checkForUpdate(force = false): Promise<UpdateInfo | null> {
  try {
    const u = (await tauriInvoke("update_check", { force })) as UpdateInfo;
    update.set(u);
    return u;
  } catch {
    return null;
  }
}

export type SettingsInput = Pick<Settings, "rest" | "enforcer" | "datadir" | "rpc_port" | "p2p_port">;

export const BITWINDOW_URL = "https://releases.drivechain.info";

// Same rules as the Rust side (and freebankd), so the form can answer as you type.
const SEED_TAG = "ecxfreebank.com";
export function tagProblem(t: string): string {
  if (!t) return "Please enter a name.";
  if (t.length > 64) return "The name can be at most 64 characters.";
  if (/[^\x20-\x7e]/.test(t)) return "Use plain letters, digits, punctuation and spaces.";
  if (t.includes("#")) return "The name can't contain #.";
  if (/^["']|["']$/.test(t)) return "No quotes around the name, please.";
  if (/^ | $/.test(t)) return "No spaces at the start or end.";
  if (t === SEED_TAG) return `${SEED_TAG} is the FreeBank seed's name; pick your own.`;
  return "";
}

const TAG_ALPHABET = "abcdefghjkmnpqrstuvwxyz23456789";
export function randomTag(): string {
  const b = new Uint8Array(4);
  crypto.getRandomValues(b);
  return "freebank-" + Array.from(b, (x) => TAG_ALPHABET[x % TAG_ALPHABET.length]).join("");
}

export async function openUrl(url: string): Promise<void> {
  try {
    await tauriInvoke("plugin:shell|open", { path: url });
  } catch {
    window.open(url, "_blank", "noopener");
  }
}

export const node = {
  setupInfo: () => tauriInvoke("setup_info") as Promise<SetupInfo>,
  saveSettings: (input: SettingsInput) => tauriInvoke("setup_save", { input }) as Promise<Settings>,
  stackCheck: () => tauriInvoke("stack_check") as Promise<StackCheck>,
  probe: () => tauriInvoke("node_probe") as Promise<Probe>,
  datadirCheck: () => tauriInvoke("datadir_check") as Promise<DatadirCheck>,
  installStart: (tag: string, moveAside: boolean) =>
    tauriInvoke("install_start", { tag, moveAside }) as Promise<void>,
  installProgress: () => tauriInvoke("install_progress") as Promise<InstallProgress>,
  installCancel: () => tauriInvoke("install_cancel") as Promise<void>,
  start: () => tauriInvoke("node_start") as Promise<void>,
  stop: () => tauriInvoke("node_stop") as Promise<void>,
  progress: () => tauriInvoke("node_progress") as Promise<NodeProgress>,
  status: () => tauriInvoke("node_status") as Promise<NodeStatus>,
  connectLocal: () => tauriInvoke("connect_local") as Promise<boolean>,
  testConnection: (rest: string, enforcer: string) =>
    tauriInvoke("test_connection", { rest, enforcer }) as Promise<ConnCheck[]>,
  setTag: (tag: string) => tauriInvoke("node_set_tag", { tag }) as Promise<SetTagResult>,
  updateStart: () => tauriInvoke("update_start") as Promise<void>,
  updateProgress: () => tauriInvoke("update_progress") as Promise<InstallProgress>,
  removePrograms: () => tauriInvoke("remove_programs") as Promise<Removed>,
  deleteChainData: () => tauriInvoke("delete_chain_data") as Promise<void>,
  obliteratePlan: () => tauriInvoke("obliterate_plan") as Promise<ObliteratePlan>,
  walletBackup: () => tauriInvoke("wallet_backup") as Promise<string[]>,
  obliterate: (ticks: WipeTick[]) => tauriInvoke("obliterate", { ticks }) as Promise<Obliterated>,
  quit: () => tauriInvoke("app_quit") as Promise<void>,
};
