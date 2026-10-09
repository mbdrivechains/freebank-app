// Settlement between houses (v0.4.1): netting, the Edinburgh exchange (freebankd v0.2.22, settle operation 4). A round
// is a hex blob passed from house to house: one starts it (createnetting), every other house joins, then every house
// funds (its net debt at par; a house owing nothing still marks its part funded), then every house signs; the last
// signature sends it. decodenetting shows a round: this file reads that and says what this house does next.

export interface NettingBundle {
  issuer: number;
  units: number;
  coins: number;
}

export interface NettingPart {
  house: number;
  presentkey: string;
  bundles: NettingBundle[];
  funded: boolean;
  fundingcoins: number;
  fundingvalue: number;
  changeaddress: string;
}

export interface NettingNet {
  house: number;
  /** In note units (1 ECX = 100000000): above zero, the house is owed; below, it owes. */
  net: number;
  receives: number;
  pays: number;
}

export interface NettingRound {
  version: number;
  houses: number[];
  starter: number;
  expiryheight: number;
  fee: number;
  parts: NettingPart[];
  nets?: NettingNet[];
  signed: number[];
  stage: "joining" | "funding" | "signing" | "complete" | string;
  /** Set when the nets can't be worked out from the round (it then can't be funded or signed). */
  error?: string;
}

/** What this house does next with the round, if anything. */
export type NettingStep =
  | { kind: "join" }
  | { kind: "fund" }
  | { kind: "sign" }
  | { kind: "wait"; why: string }
  | { kind: "done" }
  | { kind: "none"; why: string };

export function nettingStep(r: NettingRound, house: number): NettingStep {
  if (!r.houses.includes(house)) return { kind: "none", why: `House #${house} isn't in this round.` };
  const mine = r.parts.find((p) => p.house === house);
  if (r.stage === "complete") return { kind: "done" };
  if (r.stage === "joining") {
    if (!mine) return { kind: "join" };
    return { kind: "wait", why: "Waiting for the other houses to join." };
  }
  // Once everyone has joined, the nets come from the round; a round whose nets can't be worked out goes no further.
  if (r.error) return { kind: "none", why: `This round can't go on: ${r.error}` };
  if (r.stage === "funding") {
    if (mine && !mine.funded) return { kind: "fund" };
    return { kind: "wait", why: "Waiting for the other houses to fund." };
  }
  if (r.stage === "signing") {
    if (!r.signed.includes(house)) return { kind: "sign" };
    return { kind: "wait", why: "Waiting for the other houses to sign." };
  }
  return { kind: "none", why: `This round is at a stage this app doesn't know: ${r.stage}.` };
}

/** A pasted round: hex only, spaces and line breaks dropped (it may come wrapped from a chat or an email). */
export function roundHexFrom(text: string): string {
  const h = text.replace(/\s+/g, "");
  if (!/^[0-9a-fA-F]+$/.test(h) || h.length % 2) throw new Error("That isn't a netting round: paste the hex you were given.");
  return h.toLowerCase();
}
