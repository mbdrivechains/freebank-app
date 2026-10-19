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
}

export const stack = {
  status: () => tauriInvoke("stack_status") as Promise<StackStatus>,
  install: () => tauriInvoke("stack_install") as Promise<void>,
  progress: () => tauriInvoke("stack_install_progress") as Promise<StackProgress>,
  start: () => tauriInvoke("stack_start") as Promise<void>,
  stop: () => tauriInvoke("stack_stop") as Promise<void>,
};

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
