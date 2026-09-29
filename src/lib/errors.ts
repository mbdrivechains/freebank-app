// Errors from the node, for every screen. The Rust commands (and the PWA's fetch path) pass the node's
// JSON-RPC errors on as "RPC error <code>: <message>" (RpcError::for_ui), so a screen can act on the code.

/** Core's JSON-RPC error codes the app acts on. */
export const RPC = {
  KEYPOOL_RAN_OUT: -12, // a locked wallet can't refill its keys (getnewaddress): unlocking fixes it
  UNLOCK_NEEDED: -13, // "Please enter the wallet passphrase with walletpassphrase first"
  PASSPHRASE_INCORRECT: -14,
  WRONG_ENC_STATE: -15, // the wallet has no passphrase
  INVALID_ADDRESS_OR_KEY: -5, // gettransaction: not a transaction this wallet knows
  IN_WARMUP: -28,
  METHOD_NOT_FOUND: -32601,
} as const;

/** The node's JSON-RPC error code in an error, or null when the failure wasn't the node answering. */
export function rpcCode(e: unknown): number | null {
  const m = /RPC error (-?\d+):/.exec(String(e));
  return m ? Number(m[1]) : null;
}

/** Thrown when the user cancels (the unlock prompt): callers show nothing. */
export class Cancelled extends Error {
  constructor() {
    super("Cancelled");
    this.name = "Cancelled";
  }
}

export function isCancelled(e: unknown): boolean {
  return e instanceof Cancelled;
}

/** An error in plain words; "" for a cancel. A node that is starting or stopped isn't a failure of
 *  the screen, so it says so. */
export function nice(e: unknown): string {
  if (isCancelled(e)) return "";
  const m = String(e).replace(/^Error: /, "");
  if (/Loading|Verifying|Rewinding|warm|still starting|-28/i.test(m)) {
    return "Your node is still starting. This screen fills in once it's ready.";
  }
  if (/Request failed|error sending request|Connection refused|RPC not configured|busy; try again/i.test(m)) {
    return "Your FreeBank node isn't answering. It may be stopped or still starting; the Node tab shows which.";
  }
  return m.replace(/^RPC error(?: -?\d+)?: /, "").replace(/^Error: /, "");
}
