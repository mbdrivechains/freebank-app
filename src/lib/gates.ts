// Which parts of FreeBank the node's network has open, read from the node (Rust gate_info, which asks
// freebankd's getgateinfo from v0.2.17), so opening credit or gold needs no app release.
//
//   $gates        null until the node has answered, then {credit_open, gold_open, source}
//   $creditOpen   true only once the node says credit is open (or is too old to have gates)
//   loadGates()   asks again; App.svelte calls it on every refresh
//
// Screens show notes, houses, pools and bills only while $creditOpen. gold_open is a flag for later:
// no gold screen exists.

import { derived, writable } from "svelte/store";
import { api, type GateInfo } from "./api";

export const gates = writable<GateInfo | null>(null);

export const creditOpen = derived(gates, (g) => g?.credit_open === true);
export const goldOpen = derived(gates, (g) => g?.gold_open === true);

/** Ask the node. A node that is starting or unreachable leaves the last answer in place (null at
 *  first, so credit stays hidden until the node has said). */
export async function loadGates(): Promise<GateInfo | null> {
  try {
    const g = await api.gateInfo();
    gates.set(g);
    return g;
  } catch {
    return null;
  }
}
