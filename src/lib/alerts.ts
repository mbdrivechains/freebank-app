import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

// Update alerts (v0.7.6, alerts.rs): a node the app uses says it may need an update (its own `warnings`), has stopped
// adding blocks it knows of, or has heard of no new block for two hours. Shown on every screen while it holds; no
// dismiss.
export interface NodeAlert {
  /** Whose node: FreeBank's, or the eCash node the app uses. */
  node: "freebank" | "ecash";
  /** "warning": the node's own text. "stalled": 30 minutes with headers ahead of blocks, out of initial sync.
   *  "quiet": no new block for two hours (`hours`), with nothing left to download. */
  kind: "warning" | "stalled" | "quiet";
  text: string;
  blocks: number | null;
  headers: number | null;
  /** For "quiet": the newest block's time (unix seconds), whole hours since, and peers that finished the handshake. */
  tip_time: number | null;
  hours: number | null;
  peers: number | null;
}

export const nodeAlerts = writable<NodeAlert[]>([]);

export async function checkNodeAlerts(): Promise<void> {
  try {
    nodeAlerts.set((await tauriInvoke("node_alerts")) as NodeAlert[]);
  } catch {
    // Not now: the screens already say when a node is down.
  }
}

let timer: ReturnType<typeof setInterval> | null = null;
export function startNodeAlertChecks(): void {
  if (timer) return;
  setTimeout(() => checkNodeAlerts(), 20_000);
  timer = setInterval(() => checkNodeAlerts(), 60_000);
}
