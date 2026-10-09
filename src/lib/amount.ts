// Amounts in sECX, the app's only unit (operator's decisions D-2026-09-29-5 and -7: no grams until gold is
// switched on). Note units are base-native, 1 unit = 1 sat of sECX, so the same helpers serve notes too.

export const SATS_PER_ECX = 100_000_000;

/** An amount typed in sECX ("1.5", or the number an <input type="number"> binds) as whole sats.
 *  null unless it is a plain decimal above zero with at most 8 decimal places. */
export function parseEcx(v: string | number | null | undefined): number | null {
  if (v === null || v === undefined) return null;
  if (typeof v === "number") {
    if (!Number.isFinite(v) || v <= 0) return null;
    const sats = Math.round(v * SATS_PER_ECX);
    // More than 8 decimal places shows up as a remainder well above float noise.
    if (Math.abs(v * SATS_PER_ECX - sats) > 1e-3) return null;
    return Number.isSafeInteger(sats) && sats > 0 ? sats : null;
  }
  const m = /^\s*(\d*)(?:\.(\d{0,8}))?\s*$/.exec(v);
  if (!m || (m[1] === "" && (m[2] ?? "") === "")) return null;
  const sats = Number(m[1] || "0") * SATS_PER_ECX + Number((m[2] ?? "").padEnd(8, "0"));
  return Number.isSafeInteger(sats) && sats > 0 ? sats : null;
}

/** Whole sats as sECX with 8 decimals: 150000000 → "1.50000000". */
export function fmtEcx(sats: number): string {
  const neg = sats < 0;
  const s = Math.round(Math.abs(sats));
  const whole = Math.floor(s / SATS_PER_ECX);
  const frac = String(s % SATS_PER_ECX).padStart(8, "0");
  return `${neg ? "-" : ""}${whole.toLocaleString("en-US")}.${frac}`;
}

/** Whole sats as an amount for an input field, without grouping: 150000000 → "1.50000000". */
export function ecxInput(sats: number): string {
  const s = Math.max(0, Math.round(sats));
  return `${Math.floor(s / SATS_PER_ECX)}.${String(s % SATS_PER_ECX).padStart(8, "0")}`;
}

/** The message for an amount parseEcx refused. */
export const ECX_PROBLEM = "Enter an amount in sECX above zero, with at most 8 decimal places.";
