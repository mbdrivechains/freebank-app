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
  if (/Pledge below the consensus minimum/i.test(m)) {
    return "A house's sECX escrow must be at least 0.1 sECX for each owner. Nothing was sent.";
  }
  // One change per house per block (node v1.2.1: a rate post sent while another op on the house waits is refused).
  if (/op for this house is pending/i.test(m)) {
    return "Another action on this house is waiting for a block. Try again after the next block (about ten minutes). Nothing was sent.";
  }
  // The node's own refusal of a credit op the mempool won't take (FreeBank's release after v1.2.1 returns
  // "Refused: <reason>" and frees the coins at once): usually another op on the same house, note or bill is waiting.
  {
    const r = /Refused:\s*(house-op-in-mempool|house-slot-op-in-mempool|bill-op-in-mempool|note-op-in-mempool|swap-op-pending)/i.exec(m);
    if (r) {
      const what = /bill/.test(r[1]) ? "this bill" : /note/.test(r[1]) ? "these notes" : /swap/.test(r[1]) ? "this swap" : "this house";
      return `Another action on ${what} is waiting for a block. Try again after the next block (about ten minutes). Nothing was sent.`;
    }
  }
  // v0.7.5: protests and claims, the node's refusals in plain words.
  if (/No lapsed unprotested pre-auth demand to protest/i.test(m)) {
    return "There's no unpaid demand on this house to protest yet: a demand can be protested only once its window (about a week) has passed without payment. Nothing was sent.";
  }
  if (/option clause already governs this grievance/i.test(m)) {
    return "This house has suspended payment: your demand waits in its queue and earns interest until paid, so there's nothing to protest.";
  }
  if (/use the claim path/i.test(m)) {
    return "This house has failed: use Claim on its notes instead.";
  }
  if (/House is not insolvent/i.test(m)) {
    return "This house hasn't failed, so there's nothing to claim: redeem or demand its notes instead.";
  }
  if (/claim window has ended/i.test(m)) {
    return "Claims on this house closed 2 years after it failed.";
  }
  if (/still collecting its loan book/i.test(m)) {
    return "This failed house is still collecting its loans; claims open once that's done. Nothing was sent.";
  }
  if (/escrow pot is empty|Escrow pot no longer covers/i.test(m)) {
    return "There's nothing left in this house's escrow to pay claims from.";
  }
  // Gold B (node v1.2.0): each house posts its own rate; a payout or an issue needs one in effect.
  if (/has no rate in effect/i.test(m)) {
    return "This house has no rate posted right now, so it can't pay out in sECX or issue notes until it posts one. Nothing was done.";
  }
  if (/no rate key yet/i.test(m)) {
    return "This house has no rate key yet. Under House tools, Your house's rate registers one the first time you post.";
  }
  if (/doesn't hold the house's rate key/i.test(m)) {
    return "This wallet doesn't hold the key that signs this house's rates, so it can't post them.";
  }
  if (/a house with no rate posted can't issue/i.test(m)) {
    return "The house can't issue that much: it needs a rate posted in the last 24 hours, and enough sECX escrow for the notes (Houses shows both).";
  }
  // Gram nodes, the token wallet's refusals (the user's own pool and token actions).
  if (/Insufficient plain funds/i.test(m)) {
    return "Not enough spendable sECX for this and its fee. sECX still on its way can be used once it's in a block: wait a block and try again.";
  }
  if (/Insufficient unfrozen token units/i.test(m)) {
    return "You don't have that much of this token to use: some may be on its way (wait a block), or frozen by its issuer.";
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
