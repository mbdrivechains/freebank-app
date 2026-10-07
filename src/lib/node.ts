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
  /** The data folder the app created when it last installed, if it did. */
  datadir_created: string | null;
  /** Every data folder the app created. */
  datadirs_created: string[];
  /** The folders FreeBank moved aside, during setup or a restore. */
  moved_aside: string[];
  /** "Keep FreeBank's node running after I close the app". */
  keep_running: boolean;
}

export interface SetupInfo {
  settings: Settings;
  platform_error: string | null;
  suggested_tag: string;
  current_tag: string | null;
  installed: boolean;
  /** The installed release, when an earlier app build put it there without checking its signature:
   * it isn't started, and setup installs again. */
  unverified: string | null;
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
  /** Warming only because the node answered too late (busy connecting blocks). */
  busy?: boolean;
  blocks: number | null;
  headers: number | null;
}

export interface DatadirCheck {
  /** "earlier": a folder FreeBank set up for an earlier install; Setup asks Use it or Start fresh. */
  kind: "new" | "ours" | "other" | "earlier";
  message: string;
  away: string | null;
  has_wallet: boolean;
  /** For "earlier": the name on its blocks and the last block its log shows. */
  tag: string | null;
  height: number | null;
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
  /** On the Update progress: "update", or "refetch" (the installed release downloaded and checked again). */
  what: string | null;
}

export interface NodeProgress {
  running: boolean;
  exited: string | null;
  rpc: Probe;
  explorer_tip: number | null;
  peers: number | null;
  log_line: string | null;
  /** The node is rebuilding its data with -reindex, once, for a new release. */
  reindexing: boolean;
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
  /** The installed freebankd wasn't checked against its signature by this app, so it isn't started. */
  unverified: boolean;
  /** Started before the app last closed, and managed again since this launch. */
  adopted: boolean;
  /** For such a node: since when it has run without the app (unix seconds). */
  background_since: number | null;
  /** The "Keep running" setting. */
  keep_running: boolean;
  /** The node the app manages keeps running when the app closes. */
  keeps_running: boolean;
  exited: string | null;
  log_line: string | null;
  /** The node is rebuilding its data with -reindex, once, for a new release. */
  reindexing: boolean;
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
  /** False when freebank.conf says listen=0: no incoming peers. */
  listens: boolean;
  /** freebank.conf binds the peer port to this computer only (v0.2.6). */
  peers_local?: boolean;
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
  kind: "app" | "node" | "earlier" | "aside" | "cache";
  label: string;
  path: string;
  size: number;
  /** Ticked when the list opens. */
  checked: boolean;
  /** Can be ticked at all. */
  allowed: boolean;
  note: string;
  /** The wallets deleting it would delete (none for a link, which goes alone). */
  wallets: string[];
}

/** A wallet backup: the wallet file it copies, and where the copy is. It covers only that wallet. */
export interface WalletBackup {
  wallet: string;
  saved: string;
}

export interface WipeTick {
  id: string;
  path: string;
}

export interface ObliteratePlan {
  items: WipeItem[];
  wallets: string[];
  /** Everything the node's wallet holds, spendable or not yet: only with one wallet and the node
   * caught up. */
  balance: number | null;
  /** How much of balance isn't spendable yet (unconfirmed or newly mined), when any. */
  pending: number | null;
  balance_note: string | null;
  backups: WalletBackup[];
  /** What backing up will do beyond copying (stop the node for a moment), or why it can't. */
  backup_note: string | null;
  /** The app's copy of the recovery words (encrypted), which goes with the app's own folder. */
  seed: string | null;
  /** Why it can't run right now. */
  blocked: string | null;
}

/** How to remove the app itself: "deb" (sudo apt remove freebank), "appimage" (delete the file at
 * path), "mac" (drag it to the Trash) or "other" (delete the program at path). */
export interface RemoveApp {
  kind: "deb" | "appimage" | "mac" | "other";
  path: string | null;
  /** "Remove the app too" can do it (v0.2.9): the .deb through the password prompt, the Mac app to the Bin. */
  can_remove: boolean;
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

/** Sent when the window is closed with "Keep running" on and the app's node running. */
export interface QuitAsk {
  /** The node keeps running after the app closes (false: it started before the setting was on). */
  outlives: boolean;
  /** "Keep your phone connected when FreeBank is closed" is on and a phone is paired. */
  phone?: boolean;
  /** Phone sends are on: keeping the phone connected hands the passphrase to the background part. */
  phone_send?: boolean;
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

// The only links the app opens: the explorer, BitWindow's downloads, FreeBank's release pages (v0.2.0),
// the app's new-issue and private security-report pages on GitHub (v0.2.1, lib/report.ts), and the apt
// repository's page (v0.2.4, lib/appUpdate.ts).
// tauri.conf.json's plugins.shell.open holds the same pattern, and the shell plugin enforces it; this
// copy keeps the browser build's window.open to them too. Keep the two alike (security/tests.rs checks).
export const OPENABLE =
  /^https:\/\/(explorer\.ecxfreebank\.com|apt\.ecxfreebank\.com|releases\.drivechain\.info|github\.com\/mbdrivechains\/(freebank|freebank-app)\/releases|github\.com\/mbdrivechains\/freebank-app\/(issues\/new|security\/advisories\/new))([\/?][A-Za-z0-9._~%\/?=&#+-]*)?$/;

export async function openUrl(url: string): Promise<void> {
  if (!OPENABLE.test(url)) {
    console.warn(`FreeBank opens only its own links, not ${url}`);
    return;
  }
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
  /** "Remove the app too", after Obliterate removed the app's own folder. */
  removeAppItself: () => tauriInvoke("remove_app_itself") as Promise<void>,
  quit: () => tauriInvoke("app_quit") as Promise<void>,
  /** Ctrl+Q (Linux): ask what to stop, as ⌘Q does on a Mac; quits at once when there is nothing to ask. */
  quitAsked: () => tauriInvoke("app_quit_asked") as Promise<void>,
  setKeepRunning: (on: boolean) => tauriInvoke("node_set_keep_running", { on }) as Promise<Settings>,
  restart: () => tauriInvoke("node_restart") as Promise<void>,
  refetchStart: () => tauriInvoke("refetch_start") as Promise<void>,
};

/** Call `cb` when the window is closed with "Keep running" on. Returns a function that stops listening. */
export async function onQuitRequested(cb: (ask: QuitAsk) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<QuitAsk>("quit-requested", (e) => cb(e.payload));
}

/** "29 Sep 2026, 14:02" for a unix time. */
export function when(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** "12.3 of 45.6 MB", or "Connecting…" before the server has answered. */
export function megabytes(bytes: number, total: number | null): string {
  if (!bytes && !total) return "Connecting…";
  const mb = (n: number) => (n / 1e6).toFixed(1);
  return `${mb(bytes)}${total ? ` of ${mb(total)}` : ""} MB`;
}

/** A node log line in words (v0.2.6, the UX walk-through: the sync screen showed raw node lines for minutes). A line
 *  that reads like an error is shown as it is; routine lines get a phrase; anything else isn't shown. */
export function friendlyLog(line: string | null | undefined): string | null {
  if (!line) return null;
  if (/error|fail|corrupt|unable|cannot|refus/i.test(line)) return line;
  const rules: [RegExp, string][] = [
    [/mainchain block cache|bmm|enforcer/i, "Reading the eCash chain…"],
    [/mempool/i, "Loading payments that wait for a block…"],
    [/reindex/i, "Rebuilding the chain index…"],
    [/block index|loading block|block database/i, "Loading the chain…"],
    [/verifying/i, "Checking recent blocks…"],
    [/rewinding|replaying|rolling/i, "Catching up after the last stop…"],
    [/wallet/i, "Opening your wallet…"],
    [/updatetip|height=/i, "Taking in blocks…"],
    [/peer|addrman|bound to|connection/i, "Finding other FreeBank nodes…"],
  ];
  for (const [re, words] of rules) if (re.test(line)) return words;
  return null;
}
