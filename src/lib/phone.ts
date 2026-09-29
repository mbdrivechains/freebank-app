// The phone relay (desktop app only): typed wrappers over the Rust phone_* commands and its events.

import { tauriInvoke } from "./api";

export interface PhoneDevice {
  id: string;
  name: string;
  /** unix seconds */
  added: number;
  last_seen: number | null;
  /** ECX a day it may send without asking */
  limit: number;
  spent_today: number;
  online: boolean;
}

export interface PairAsk {
  /** this request; answers name it */
  id: string;
  /** the phone's device id */
  device: string;
  name: string;
  /** "042 917": allow only if the phone shows the same */
  code: string;
}

export interface HeldSend {
  confirm: string;
  device: string;
  name: string;
  address: string;
  amount: number;
  /** unix seconds: when it was held, and when it stops waiting */
  at: number;
  expires: number;
  /** "limit": over the phone's daily limit; "locked": the wallet is locked and phone sends are off */
  why: string;
}

export interface PhoneWallet {
  /** null when the node can't be asked right now */
  encrypted: boolean | null;
  locked: boolean;
  /** "Let my phone send while FreeBank is open" is on */
  phone_send: boolean;
}

export interface Confirmed {
  txid: string | null;
  /** Nothing happened: the wallet is locked; ask for its passphrase and confirm again. */
  need_passphrase: boolean;
}

export interface RelayStatus {
  url: string;
  room: string;
  /** "off" | "connecting" | "online" | "retrying" */
  state: string;
  detail: string;
  /** phones waiting for "Allow this phone?", oldest first; allowing one refuses the rest */
  pair_pending: PairAsk[];
  held: HeldSend[];
}

export interface PhoneSend {
  time: number;
  device: string;
  name: string;
  address: string;
  amount: number;
  /** "sent" | "held" | "declined" | "failed" | "expired" | "cancelled" */
  result: string;
  detail: unknown;
}

export const phone = {
  pairStart: () => tauriInvoke("phone_pair_start") as Promise<{ url: string; expires: number }>,
  pairAnswer: (id: string, allow: boolean) => tauriInvoke("phone_pair_answer", { id, allow }) as Promise<void>,
  devices: () => tauriInvoke("phone_devices") as Promise<PhoneDevice[]>,
  revoke: (id: string) => tauriInvoke("phone_revoke", { id }) as Promise<void>,
  setLimit: (id: string, limit: number) => tauriInvoke("phone_set_limit", { id, limit }) as Promise<void>,
  /** `passphrase` unlocks a locked wallet for this one send. */
  confirmSend: (id: string, allow: boolean, passphrase?: string) =>
    tauriInvoke("phone_confirm_send", { id, allow, passphrase: passphrase || null }) as Promise<Confirmed>,
  wallet: () => tauriInvoke("phone_wallet") as Promise<PhoneWallet>,
  /** The passphrase is checked, then kept in the app's memory only (never on disk). */
  sendOn: (passphrase: string) => tauriInvoke("phone_send_on", { passphrase }) as Promise<void>,
  sendOff: () => tauriInvoke("phone_send_off") as Promise<void>,
  status: () => tauriInvoke("phone_relay_status") as Promise<RelayStatus>,
  setRelay: (url: string) => tauriInvoke("phone_set_relay", { url }) as Promise<void>,
  recentSends: () => tauriInvoke("phone_recent_sends") as Promise<PhoneSend[]>,
};

const EVENTS = ["phone-pair-request", "phone-held-send", "phone-send", "phone-changed"];

/** Call `cb` on any phone event. Returns a function that stops listening. */
export async function onPhoneEvent(cb: (name: string, payload: unknown) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const offs = await Promise.all(EVENTS.map((n) => listen(n, (e) => cb(n, e.payload))));
  return () => offs.forEach((off) => off());
}

export function when(unix: number | null): string {
  if (!unix) return "never";
  return new Date(unix * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}
