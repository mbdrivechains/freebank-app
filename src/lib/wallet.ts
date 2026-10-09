// The wallet's lock, for every screen that signs. v0.2.0 makes the wallet passphrase required, so each
// signing action meets a locked wallet. Wrap it:
//
//   const txid = await withUnlock(() => api.sendTransaction(address, amount), { what: "send 1.5 sECX" });
//
// withUnlock runs the action. If the node answers -13 ("Please enter the wallet passphrase with
// walletpassphrase first"), or -12 (a locked wallet can't refill its keys), it shows the unlock prompt
// (UnlockPrompt.svelte, mounted once in App.svelte), unlocks for UNLOCK_SECONDS, retries once, and
// locks again, unless the wallet was already unlocked before. A wrong passphrase (-14) says so in the
// prompt and lets the user retry or cancel. Cancel rejects with Cancelled (lib/errors.ts), which nice()
// turns into "", so a screen's usual `catch (e) { error = nice(e) }` shows nothing.
//
// `upfront: true` asks before running, for actions that can't simply be retried (the phone's held
// send). Actions that would unlock at once wait their turn: one prompt at a time.

import { get, writable } from "svelte/store";
import { api, type WalletStatus } from "./api";
import { Cancelled, RPC, nice, rpcCode } from "./errors";

export type { WalletStatus };

/** How long withUnlock unlocks for. */
export const UNLOCK_SECONDS = 30;

export const wallet = {
  status: (): Promise<WalletStatus> => api.walletStatus(),
  /** Unlock for `seconds` (1..300). A wrong passphrase rejects with "RPC error -14: …". */
  unlock: (passphrase: string, seconds: number): Promise<WalletStatus> => api.walletUnlock(passphrase, seconds),
  lock: (): Promise<WalletStatus> => api.walletLock(),
};

/** The prompt withUnlock is showing, or null. App.svelte renders UnlockPrompt from it. */
export interface UnlockRequest {
  /** What the passphrase is for, completing "Enter your wallet passphrase to …". */
  what: string;
  /** Set after a wrong passphrase or a failed unlock. */
  error: string;
  /** An unlock is in flight. */
  busy: boolean;
}

export const unlockRequest = writable<UnlockRequest | null>(null);

let pending: { resolve: () => void; reject: (e: unknown) => void } | null = null;

/** The prompt's Unlock. The passphrase goes straight to the node and isn't kept. */
export async function submitUnlock(passphrase: string): Promise<void> {
  const r = get(unlockRequest);
  if (!r || !pending || r.busy) return;
  unlockRequest.set({ ...r, busy: true, error: "" });
  try {
    await wallet.unlock(passphrase, UNLOCK_SECONDS);
    const p = pending;
    pending = null;
    unlockRequest.set(null);
    p?.resolve();
  } catch (e) {
    const error =
      rpcCode(e) === RPC.PASSPHRASE_INCORRECT
        ? "That passphrase isn't right. Try again, or cancel."
        : nice(e) || "The wallet didn't unlock.";
    unlockRequest.set({ ...r, busy: false, error });
  }
}

/** The prompt's Cancel: the waiting action rejects with Cancelled. */
export function cancelUnlock(): void {
  const p = pending;
  pending = null;
  unlockRequest.set(null);
  p?.reject(new Cancelled());
}

function askToUnlock(what: string): Promise<void> {
  return new Promise((resolve, reject) => {
    pending = { resolve, reject };
    unlockRequest.set({ what, error: "", busy: false });
  });
}

// One unlock-and-retry at a time, so two actions never race for the prompt or the lock.
let queue: Promise<unknown> = Promise.resolve();
function oneAtATime<T>(job: () => Promise<T>): Promise<T> {
  const run = queue.then(job, job);
  queue = run.catch(() => undefined);
  return run;
}

/** The node refused because the wallet is locked (a read that needs it, such as listmynotes, or a signing action). */
export function walletLocked(e: unknown): boolean {
  return needsUnlock(e);
}

function needsUnlock(e: unknown): boolean {
  const code = rpcCode(e);
  return code === RPC.UNLOCK_NEEDED || code === RPC.KEYPOOL_RAN_OUT;
}

export interface UnlockOptions {
  /** Completes "Enter your wallet passphrase to …", e.g. "send 1.5 sECX". */
  what?: string;
  /** Ask before running the action instead of after the node refuses it. */
  upfront?: boolean;
}

/** Run a signing action, unlocking the wallet for it if the node asks. See the top of this file. */
export async function withUnlock<T>(action: () => Promise<T>, opts: UnlockOptions = {}): Promise<T> {
  const what = opts.what ?? "sign this transaction";
  let refused: unknown = null;
  if (!opts.upfront) {
    try {
      return await action();
    } catch (e) {
      if (!needsUnlock(e)) throw e;
      refused = e;
    }
  }
  return oneAtATime(async () => {
    let st: WalletStatus | null = null;
    try {
      st = await wallet.status();
    } catch (e) {
      if (refused) throw refused;
    }
    // No passphrase: unlocking can't help (the refusal stands); upfront, just run it.
    if (st && !st.encrypted) {
      if (refused) throw refused;
      return action();
    }
    // Unlocked already (someone unlocked it for longer): run it, and leave the lock as it is.
    if (!st || st.unlocked_until > 0) return action();
    await askToUnlock(what);
    try {
      return await action();
    } finally {
      await wallet.lock().catch(() => undefined);
    }
  });
}
