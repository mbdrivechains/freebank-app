// Houses this wallet runs (v0.4.6): Home's attest reminder and the opt-in automatic attests (src-tauri/src/house_watch.rs).
import { tauriInvoke } from "./api";

export type HouseDue = { id: number; name: string; status: string; cause: "" | "deadline" | "other"; deadline: number; left: number; last: number };
export type HousesDue = { height: number; houses: HouseDue[]; auto_within: number; auto_here: boolean };

export const housesDue = () => tauriInvoke("houses_due") as Promise<HousesDue>;
export const autoAttestOn = (passphrase: string) => tauriInvoke("auto_attest_on", { passphrase }) as Promise<boolean>;
export const autoAttestOff = () => tauriInvoke("auto_attest_off") as Promise<boolean>;
