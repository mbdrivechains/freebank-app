// Bills (v0.6.12): when this wallet may claim a bill's bond. The node takes a claim from the holder of an active bill
// that no house holds, once the next block is past maturity + grace ("bad-bill-claim-early" before). The node keeps
// such a bill "a" (active) until the claim is mined, which then marks it "d", so the status alone can't say when.
export interface BillState {
  status: string;
  roles?: string[];
  maturity_height: number;
  grace_blocks: number;
  owner_house_id?: number;
}

/** The first block in which the bond can be claimed. */
export function claimableFrom(b: BillState): number {
  return b.maturity_height + b.grace_blocks + 1;
}

/** Whether this wallet can claim the bond now, at chain height `height` (the next block is height + 1). */
export function canClaimBond(b: BillState, height: number): boolean {
  return (
    b.status === "a" &&
    (b.roles ?? []).includes("holder") &&
    !b.owner_house_id &&
    height + 1 >= claimableFrom(b)
  );
}
