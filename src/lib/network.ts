// The network this run of the app is on (v0.5.0; Rust network.rs), and the eCash node and enforcer the app runs
// itself for it (stack.rs). The screens name the network and its eCash chain from here, never "beta" by hand.

import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

export interface NetworkInfo {
  id: string;
  name: string;
  /** What its eCash chain is called: "eCash beta", "easynet". */
  l1: string;
  /** FreeBank's explorer on this network (tx links), or none. */
  explorer: string | null;
  demo: boolean;
  /** The Telegram bot with play money, if any. */
  bot: string | null;
  /** The network has a faucet (v0.6.1: easynet's). */
  faucet: boolean;
  /** The app can install this network's eCash node and enforcer itself. */
  stack: boolean;
  /** Every network: [id, name, offered]. */
  all: [string, string, boolean][];
}

/** Beta, until the app says otherwise (and in the browser build, which has no Rust side). */
export const BETA: NetworkInfo = {
  id: "beta",
  name: "Beta",
  l1: "eCash beta",
  explorer: "https://explorer.ecxfreebank.com",
  demo: true,
  bot: "freebank_beta_bot",
  faucet: false,
  stack: false,
  all: [["beta", "Beta", true]],
};

export const network = writable<NetworkInfo>(BETA);

export async function loadNetwork(): Promise<NetworkInfo> {
  try {
    const n = (await tauriInvoke("network_info")) as NetworkInfo;
    network.set(n);
    return n;
  } catch {
    return BETA;
  }
}

/** Switch networks: the app restarts on the other one. */
export function switchNetwork(id: string): Promise<void> {
  return tauriInvoke("network_switch", { id }) as Promise<void>;
}

export interface Ports {
  p2p: number;
  rpc: number;
  zmq: number;
  grpc: number;
  erpc: number;
}

/** A program on one of this computer's ports. */
export interface Holder {
  port: number;
  pid: number | null;
  name: string;
}

export interface StackStatus {
  offered: boolean;
  installed: string | null;
  node_up: boolean;
  enforcer_up: boolean;
  blocks: number | null;
  headers: number | null;
  peers: number | null;
  enforcer_tip: number | null;
  /** A newer release FreeBank has for this network's eCash node: Update installs it. */
  update: string | null;
  ports: Ports | null;
  /** Before an install: default ports other programs hold. The app's own node takes free ones instead. */
  clashes: Holder[];
  // v0.5.1
  /** The whole chain on disk, in GB, once the node has every block. */
  full_gb: number;
  /** The network's snapshots, and whether each answers now (before an install, or while one waits to load). */
  offers: Offer[];
  /** Where its blocks go (or would go), and the free bytes there; where its other data is, when the blocks have a disk
   * of their own. */
  blocks_at: string | null;
  blocks_free: number | null;
  data_free: number | null;
  /** What its folders take now. */
  used: number | null;
  /** A snapshot chosen and not loaded yet: Continue carries on. */
  snapshot_pending: string | null;
  /** How it started: a snapshot's id, or "full". */
  start: string | null;
  /** While the node checks the history behind its snapshot: the block reached, of the snapshot's. */
  checked_to: number | null;
  checked_of: number | null;
  /** Stopped for want of disk space: why. */
  stopped_low: string | null;
  /** Stopped on its own while the app meant it to run (v0.6.1), until the next start by hand. */
  stopped_own: StoppedOwn | null;
}

/** What the app's watch found (stack.rs `check_gone`): its eCash node or enforcer stopped on its own. */
export interface StoppedOwn {
  what: "node" | "enforcer" | "both";
  /** When (Unix seconds). */
  at: number;
  /** The last error in its log before it stopped, else how its log ends. */
  line: string | null;
  /** The app started it again (once an hour at most). */
  restarted: boolean;
  restart_error: string | null;
}

/** A snapshot the node can start from. */
export interface Offer {
  id: string;
  source: string;
  height: number;
  size: number;
  available: boolean;
}

/** How to set it up: a snapshot's id or "full" (none: the first snapshot that answers), and a folder for the blocks. */
export interface StackChoice {
  snapshot?: string | null;
  blocks?: string | null;
  /** Setup's install: the app then uses this node. Settings' Continue and Update leave it where it is. */
  take?: boolean;
}

/** What Stop and delete did. */
export interface Removed {
  deleted: string[];
  freed: number;
  kept: string[];
  /** It was the eCash node the app used: the app goes back to setup. */
  in_use: boolean;
}

/** What setup found on another computer. */
export interface Remote {
  rest: string | null;
  enforcer: string | null;
  on_network: boolean;
  l1_blocks: number | null;
  address: string | null;
  tailscale: boolean;
  tried: string[];
  detail: string;
}

export interface StackProgress {
  running: boolean;
  done: boolean;
  stage: string;
  bytes: number;
  total: number | null;
  /** While it catches up (stage "sync"). */
  blocks: number | null;
  headers: number | null;
  error: string | null;
  /** The snapshot this install starts from (v0.5.1): its stages show. */
  snapshot: string | null;
}

export const stack = {
  /** offers: also ask the snapshots' servers which answer (setup only). */
  status: (offers = false) => tauriInvoke("stack_status", { offers }) as Promise<StackStatus>,
  install: (choice?: StackChoice) => tauriInvoke("stack_install", { choice: choice ?? null }) as Promise<void>,
  progress: () => tauriInvoke("stack_install_progress") as Promise<StackProgress>,
  start: () => tauriInvoke("stack_start") as Promise<void>,
  stop: () => tauriInvoke("stack_stop") as Promise<void>,
  /** Stop and delete: the install or the node stops at once, and its chain is deleted. */
  remove: () => tauriInvoke("stack_remove") as Promise<Removed>,
  /** Free bytes where a typed folder is (or would be made). */
  free: (path: string) => tauriInvoke("stack_free", { path }) as Promise<number | null>,
  findRemote: (host: string) => tauriInvoke("stack_find_remote", { host }) as Promise<Remote>,
  useRemote: (rest: string, enforcer: string) => tauriInvoke("stack_use_remote", { rest, enforcer }) as Promise<unknown>,
};

/** What a plain start says while a chosen snapshot waits to load (stack.rs SNAPSHOT_PENDING). */
export const SNAPSHOT_PENDING_START = "FreeBank's eCash node hasn't loaded its snapshot yet.";

/** "830 GB", "9.4 GB", "327 MB". */
export function gigabytes(n: number | null | undefined): string {
  if (n == null) return "…";
  if (n < 1e9) return `${Math.round(n / 1e6)} MB`;
  const g = n / 1e9;
  return `${g >= 100 ? Math.round(g) : g >= 10 ? g.toFixed(0) : g.toFixed(1)} GB`;
}

/** Who made a snapshot, and how far it takes the node, in words. */
export function snapshotWords(o: Offer): string {
  return o.id === "ours"
    ? `Our snapshot at block ${o.height.toLocaleString()} (${gigabytes(o.size)}), published by mbdrivechains with its eCash beta node build: not an L2L or eCash release. The node checks it against the hash compiled into it.`
    : o.id === "l2l"
      ? `LayerTwo Labs' snapshot at block ${o.height.toLocaleString()} (${gigabytes(o.size)}), the one BitWindow uses. The node checks it against Bitcoin Core's own hash for that block.`
      : `${o.source}'s snapshot at block ${o.height.toLocaleString()} (${gigabytes(o.size)}).`;
}

/** How the node started, for Settings. */
export function startWords(start: string | null | undefined): string {
  return start === "ours"
    ? "from mbdrivechains' snapshot (967,000)"
    : start === "l2l"
      ? "from LayerTwo Labs' snapshot (935,000)"
      : start === "full"
        ? "a full sync from the start"
        : "";
}

/** The stop (its `at`) whose Home notice × hid: for the rest of the session, as Home is left and come back to. */
export const stoppedOwnHidden = writable(0);

/** What the watch found, in sentences for Home and Settings. */
export function stoppedOwnWords(s: StoppedOwn): string {
  const name = s.what === "both" ? "The eCash node and the enforcer" : s.what === "node" ? "The eCash node" : "The enforcer";
  const it = s.what === "both" ? "them" : "it";
  const time = new Date(s.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  // stack.rs `last_words` says some things in its own sentence; others are the log's own line.
  const own = !!s.line && /^It(s log ends with no error| stopped normally)/.test(s.line);
  const log = !s.line ? "" : own ? ` ${s.line}` : ` Its log says: “${s.line}”`;
  const then = s.restarted
    ? ` FreeBank started ${it} again.`
    : s.restart_error
      ? ` Starting ${it} again failed: ${s.restart_error}`
      : ` It happened again within the hour, so FreeBank left ${it} stopped: press Start them.`;
  return `${name} stopped by ${s.what === "both" ? "themselves" : "itself"} at ${time}.${log}${then}`;
}

/** What each install stage is called on screen. */
export const STACK_STAGES: [string, string][] = [
  ["download-node", "Download the eCash node"],
  ["download-enforcer", "Download the enforcer"],
  ["check", "Check them against the hashes FreeBank knows"],
  ["unpack", "Unpack"],
  ["config", "Choose its ports"],
  ["start", "Start them"],
  ["sync", "Catch up with the chain"],
];

/** With a snapshot (v0.5.1): its stages, between starting the node and catching up. */
export const SNAPSHOT_STAGES: [string, string][] = [
  ["download-snapshot", "Download the snapshot, while the node gets the chain's headers"],
  ["check-snapshot", "Check the snapshot against its hash"],
  ["load-snapshot", "Load it into the node (many minutes; the node checks it too)"],
];

/** The stages an install goes through: with a snapshot's in the middle when it starts from one. */
export function stackStages(snapshot: string | null | undefined): [string, string][] {
  if (!snapshot) return STACK_STAGES;
  const i = STACK_STAGES.findIndex(([id]) => id === "sync");
  return [...STACK_STAGES.slice(0, i), ...SNAPSHOT_STAGES, ...STACK_STAGES.slice(i)];
}
