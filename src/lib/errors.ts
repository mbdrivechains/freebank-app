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
  // Gram notes: every sECX payout is priced at the tip, and the node refuses one when the feed is stale and no pool is
  // deep ("No payout price...").
  if (/no payout price/i.test(m)) {
    return "There is no gold price on FreeBank right now, so payouts in sECX wait. Nothing is lost: try again once the price is reported. Until then, bills don't fall overdue.";
  }
  // Gram nodes, the gold token wallet's refusals (the user's own pool and token actions).
  if (/Insufficient plain funds/i.test(m)) {
    return "Not enough spendable sECX for this and its fee. sECX still on its way can be used once it's in a block: wait a block and try again.";
  }
  if (/Insufficient unfrozen token units/i.test(m)) {
    return "You don't have that much of this gold token to use: some may be on its way (wait a block), or frozen by its issuer.";
  }
  if (/Insufficient LP shares/i.test(m)) {
    return "You don't have that many shares of this pool.";
  }
  if (/above max_sats|below min_out/i.test(m)) {
    return "The pool's price moved more than 1% since the estimate, so nothing was done. Look at the new estimate and try again.";
  }
  if (/Swap out of range for the pool/i.test(m)) {
    return "That swap is too small for the pool to pay anything, or too large for what it holds. Try another amount.";
  }
  if (/Liquidity too small to mint a share/i.test(m)) {
    return "That's too little to earn a share of the pool. Add more.";
  }
  if (/pool keeps its floor/i.test(m)) {
    return "That many shares can't come out: a pool keeps a small floor that is never taken out. Take out fewer.";
  }
  // Node v0.2.19's refusals for members-only and redeem-only houses.
  if (/bad-house-not-member/.test(m)) {
    return "This house's notes can only go to its members, and that address isn't one.";
  }
  if (/bad-house-redeem-only/.test(m)) {
    return "This house's notes pass only between the house and their holder: demand them instead (or redeem them, if your node holds the house's keys).";
  }
  if (/member op for this house is already in the mempool/i.test(m)) {
    return "A house's members can change once a block. Try again after the next block.";
  }
  if (/house-state-changing op .*already in the mempool/i.test(m)) {
    return "This house already has a change waiting for the next block. Try again after it.";
  }
  return m.replace(/^RPC error(?: -?\d+)?: /, "").replace(/^Error: /, "");
}
