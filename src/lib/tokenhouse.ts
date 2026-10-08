// Run a house (v0.4.0): the house's mint. The mint writes keyset.json (its folder) for the partners to record on chain
// before it issues any token: the house, the keyset id, its keys and the key that signs its posts. Here the app checks
// the file before recording it: the id must be the one its keys give (Cashu's v1 id, NUT-02), so what is recorded is
// what the mint signs with.
import { sha256 } from "./sha256";

export interface MintKeyset {
  house: number;
  keysetid: string;
  keys: { amount: number; pubkey: string }[];
  postingpubkey: string;
  /** The mint's float address: a batch lock must take its notes from there (fb-mint writes it since v0.4.0). */
  float?: string;
}

const POINT = /^0[23][0-9a-f]{64}$/;

function bytes(hex: string): Uint8Array {
  const b = new Uint8Array(hex.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return b;
}

/** Cashu's v1 keyset id: "00" and the first 7 bytes of SHA-256 over the keys' points, in order of amount. */
export function keysetId(keys: { amount: number; pubkey: string }[]): string {
  const sorted = [...keys].sort((a, b) => a.amount - b.amount);
  const all = new Uint8Array(sorted.length * 33);
  sorted.forEach((k, i) => all.set(bytes(k.pubkey), i * 33));
  return "00" + Array.from(sha256(all).slice(0, 7), (x) => x.toString(16).padStart(2, "0")).join("");
}

/** keyset.json as pasted, checked; throws with what's wrong. */
export function parseKeysetFile(text: string): MintKeyset {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new Error("That isn't the mint's keyset.json: paste the whole file.");
  }
  const o = v as Partial<MintKeyset>;
  if (!Number.isInteger(o.house) || (o.house as number) < 0) throw new Error("keyset.json has no house.");
  if (typeof o.keysetid !== "string" || !/^00[0-9a-f]{14}$/.test(o.keysetid)) throw new Error("keyset.json's keyset id isn't a v1 id.");
  if (typeof o.postingpubkey !== "string" || !POINT.test(o.postingpubkey)) throw new Error("keyset.json's posting key isn't a public key.");
  if (!Array.isArray(o.keys) || o.keys.length === 0 || o.keys.length > 51) throw new Error("keyset.json's keys are missing (at most 51).");
  const seen = new Set<number>();
  for (const k of o.keys) {
    const a = k?.amount;
    if (!Number.isSafeInteger(a) || a <= 0 || a > 2 ** 50 || 2 ** Math.round(Math.log2(a)) !== a) {
      throw new Error("A key's amount isn't a power of two up to 2^50.");
    }
    if (seen.has(a)) throw new Error("Two keys for one amount.");
    seen.add(a);
    if (typeof k.pubkey !== "string" || !POINT.test(k.pubkey)) throw new Error("A key isn't a public key.");
  }
  const id = keysetId(o.keys);
  if (id !== o.keysetid) throw new Error(`keyset.json's id (${o.keysetid}) isn't the one its keys give (${id}).`);
  const float = typeof o.float === "string" && /^X[1-9A-HJ-NP-Za-km-z]{25,34}$/.test(o.float) ? o.float : undefined;
  return { house: o.house as number, keysetid: o.keysetid, keys: o.keys, postingpubkey: o.postingpubkey, float };
}

/** The lock's hex from what was pasted: the bare hex, or createnotelock's whole answer ({"hex": …}). */
export function lockHexFrom(text: string): string {
  const t = text.trim();
  if (t.startsWith("{")) {
    try {
      const h = (JSON.parse(t) as { hex?: unknown }).hex;
      if (typeof h === "string") return h.trim();
    } catch {
      // not JSON: the node says what's wrong with it
    }
  }
  return t;
}

/** The mint's float address for a house, as this computer last saw it in keyset.json (or typed). */
export function savedFloat(house: number): string {
  try {
    return localStorage.getItem(`fb.mintFloat.${house}`) ?? "";
  } catch {
    return "";
  }
}

export function saveFloat(house: number, address: string) {
  try {
    localStorage.setItem(`fb.mintFloat.${house}`, address);
  } catch {
    // no storage: typed again next time
  }
}
