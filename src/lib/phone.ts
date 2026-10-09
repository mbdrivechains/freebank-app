// The phone relay (desktop app only): typed wrappers over the Rust phone_* commands and its events.

import { tauriInvoke } from "./api";
import { BASE_TICKER } from "./brand";

/** A phone payment (v0.2.5): sECX, or a house's notes sent, redeemed or demanded. */
export type PaymentKind = "send" | "note-send" | "note-redeem" | "note-demand";

interface Payment {
  kind?: PaymentKind;
  house?: number | null;
  amount: number;
}

/** What a phone's payment does, in a few words: "send 0.5 sECX", "redeem 0.5 sECX of house #3's notes". */
export function paymentWhat(p: Payment): string {
  const amt = `${p.amount} ${BASE_TICKER}`;
  const notes = `${amt} of house #${p.house}'s notes`;
  switch (p.kind ?? "send") {
    case "note-send":
      return `send ${notes}`;
    case "note-redeem":
      return `redeem ${notes}`;
    case "note-demand":
      return `demand ${notes}`;
    default:
      return `send ${amt}`;
  }
}

/** The same, done: "Sent 0.5 sECX", "Redeemed 0.5 sECX of house #3's notes". */
export function paymentDone(p: Payment): string {
  const w = paymentWhat(p);
  const done: Record<string, string> = { send: "Sent", redeem: "Redeemed", demand: "Demanded" };
  const [verb, ...rest] = w.split(" ");
  return [done[verb] ?? verb, ...rest].join(" ");
}

/** The button that does it: Send, Redeem or Demand. */
export function paymentButton(p: Payment): string {
  return p.kind === "note-redeem" ? "Redeem" : p.kind === "note-demand" ? "Demand" : "Send";
}

export interface PhoneDevice {
  id: string;
  name: string;
  /** unix seconds */
  added: number;
  last_seen: number | null;
  /** sECX a day it may send without asking */
  limit: number;
  spent_today: number;
  online: boolean;
  /** Face ID: the phone added a passkey; `face_id_sends`, each send asks for it too. */
  face_id: boolean;
  face_id_sends: boolean;
}

/** An approval the desktop is waiting for on a phone (v0.2.5, "Approve sends on my phone"). */
export interface Approval {
  id: string;
  /** "Send 2.5 sECX to X…", or the change it would make */
  text: string;
  /** unix seconds */
  expires: number;
}

export interface ApproveInfo {
  /** sECX: once this computer's payments in a day would come to more than this, a phone's Face ID first; null: off. */
  over: number | null;
  /** sECX this computer can still pay today without asking */
  left: number | null;
  /** paired phones with Face ID, which can approve */
  approvers: number;
  waiting: Approval[];
  /** A change the recovery words made, waiting its day: the new amount (null: off), due at unix seconds. */
  scheduled: { over: number | null; due: number } | null;
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
  /** Empty for a redeem or a demand. */
  address: string;
  amount: number;
  kind?: PaymentKind;
  house?: number | null;
  /** unix seconds: when it was held, and when it stops waiting */
  at: number;
  expires: number;
  /** "limit": over the phone's daily limit; "locked": the wallet is locked and phone sends are off */
  why: string;
  /** The phone's own Face ID signed it (v0.2.5). */
  face_id?: boolean;
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
  /** v0.2.5: a note action and its house (absent for a send). */
  kind?: PaymentKind;
  house?: number | null;
  /** "sent" | "held" | "declined" | "failed" | "expired" | "cancelled" | "refused" (FreeBank was closed) */
  result: string;
  detail: unknown;
  /** A held send's id, on each of its lines (since v0.2.2): it is listed once, with its latest state. */
  held?: string;
}

/** "Keep your phone connected when FreeBank is closed" (src-tauri/src/phone/background.rs). */
export interface KeepInfo {
  keep: boolean;
  /** Asked already (once, after the first phone pairs). */
  asked: boolean;
  /** At this start the app took the link back from a background part running since then (unix s). */
  took_back: number | null;
  take_back_error: string | null;
  /** "Start when I log in" (daemon mode, src-tauri/src/phone/login_item.rs), and whether this system has it. */
  at_login: boolean;
  at_login_here: boolean;
}

/** Hosted wallets (v0.2.8, src-tauri/src/phone/hosted.rs): a wallet this desktop keeps for someone else's phone. */
export type HostedStep = "setup" | "words" | "joining" | "ready" | "moving" | "moved" | "failed";

export interface HostedPhone {
  id: string;
  name: string;
  house: number;
  house_name: string;
  /** The inviting phone's name. */
  by: string;
  added: number;
  last_seen: number | null;
  step: HostedStep;
  why: string | null;
  member: string | null;
  moved_at: number | null;
  /** The copy here was deleted: at the node's next start its file goes (emptied by the move home) or moves aside. */
  remove: boolean;
  /** The move home found it holding nothing. */
  empty: boolean;
  /** Moving home: their own computer's address, which the house adds as a member. */
  move_to: string | null;
  face_id: boolean;
  online: boolean;
}

/** An invited phone asking to join (the inviting phone usually answers it). */
export interface HostedAsk {
  id: string;
  device: string;
  name: string;
  code: string;
  house: number;
  house_name: string;
  by: string;
  expires: number;
}

export interface HostedInfo {
  phones: HostedPhone[];
  asks: HostedAsk[];
}

export const HOSTED_STEP: Record<HostedStep, string> = {
  setup: "Making the wallet",
  words: "Showing its recovery words",
  joining: "Joining the house",
  ready: "Ready",
  moving: "Moving to their own computer",
  moved: "Moved to their own computer",
  failed: "Couldn't be made",
};

export const phone = {
  keepInfo: () => tauriInvoke("phone_keep_info") as Promise<KeepInfo>,
  /** On also keeps the node running. */
  keepSet: (on: boolean) => tauriInvoke("phone_keep_set", { on }) as Promise<void>,
  /** "Start when I log in"; on also turns on keeping the phone connected and the node running. */
  loginSet: (on: boolean) => tauriInvoke("phone_login_set", { on }) as Promise<void>,
  /** The close notice's "Keep the phone connected": start the background part, then close. */
  keepConnectedQuit: () => tauriInvoke("phone_keep_connected_quit") as Promise<void>,
  pairStart: () => tauriInvoke("phone_pair_start") as Promise<{ url: string; expires: number }>,
  pairAnswer: (id: string, allow: boolean) => tauriInvoke("phone_pair_answer", { id, allow }) as Promise<void>,
  approveInfo: () => tauriInvoke("phone_approve_info") as Promise<ApproveInfo>,
  /** `over` in sECX (null: off). Off or a higher amount waits for a phone's Face ID; with `words`, it happens a day
   * later instead (the answer: when, unix seconds), unless a phone or this computer cancels it. */
  approveSet: (over: number | null, words?: string) =>
    tauriInvoke("phone_approve_set", { over, words }) as Promise<number | null>,
  /** Cancel the change the recovery words made, while it waits. */
  approveCancelScheduled: () => tauriInvoke("phone_approve_cancel_scheduled") as Promise<void>,
  approvalCancel: (id: string) => tauriInvoke("phone_approval_cancel", { id }) as Promise<void>,
  devices: () => tauriInvoke("phone_devices") as Promise<PhoneDevice[]>,
  revoke: (id: string) => tauriInvoke("phone_revoke", { id }) as Promise<void>,
  /** "Remove Face ID", for a phone that lost its passkey. */
  removePasskey: (id: string) => tauriInvoke("phone_remove_passkey", { id }) as Promise<void>,
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
  hosted: () => tauriInvoke("phone_hosted") as Promise<HostedInfo>,
  hostedAnswer: (id: string, allow: boolean) => tauriInvoke("phone_hosted_answer", { id, allow }) as Promise<void>,
  /** Delete the copy here: its phone is cut off, and its wallet file moves aside at the node's next start. */
  hostedRemove: (id: string) => tauriInvoke("phone_hosted_remove", { id }) as Promise<void>,
  hostedRetry: (id: string) => tauriInvoke("phone_hosted_retry", { id }) as Promise<void>,
  hostedRemovePasskey: (id: string) => tauriInvoke("phone_hosted_remove_passkey", { id }) as Promise<void>,
};

const EVENTS = ["phone-pair-request", "phone-held-send", "phone-send", "phone-changed", "phone-approval", "phone-hosted-moved", "phone-hosted-moving", "phone-move-notice"];

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
