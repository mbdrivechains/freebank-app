// Central brand + network config for the FreeBank app.
// One place to change the product name, base-coin ticker, and connection defaults.

export const APP_NAME = "FreeBank";
export const APP_TAGLINE = "Free-banking on a Bitcoin drivechain";

// Base reserve coin. FreeBank v1 is base-native 1:1 — canonical model: unit = reserve = ECX
// ("the hardest money possible"). Notes issued by discount houses are denominated in
// abstract "units"; the base chain coin is ECX.
// NOTE: freebankd still reports CURRENCY_UNIT="SC1" (unrebranded sidechain-template debt, fee
// display only). The display ticker is intentionally ECX; change BASE_TICKER here if that moves.
export const BASE_TICKER = "ECX";
export const NOTE_UNIT = "units";

// The public explorer (beta). Receipts link a transaction as `${EXPLORER_URL}/tx/<txid>`.
// The same address is node::EXPLORER on the Rust side.
export const EXPLORER_URL = "https://explorer.ecxfreebank.com";

// localStorage key for the saved PWA connection.
export const CONFIG_KEY = "freebank_connection";

// freebankd RPC ports (no testnet — main + regtest only). FreeBank is drivechain slot 130.
export const DEFAULT_PORT = 8454; // main (regtest is 18457)
export const PORTS: Record<string, number> = { main: 8454, regtest: 18457 };

export type ConnMode = "local" | "tailscale" | "tor" | "custom";

export interface ConnPreset {
  mode: ConnMode;
  label: string;
  hostPlaceholder: string;
  defaultHost: string;
  help: string;
  works: boolean; // false => informational only in this build (transport not wired)
}

// Model A mobility: the node is custodial (keys live on freebankd); this app is a remote control.
// "This computer" and "Tailscale" both connect over a plain IP and work today. "Tor" is a
// first-class option in the vision but needs the desktop app + a local SOCKS proxy, so it is
// presented honestly as not-yet-wired in this build.
export const CONN_PRESETS: ConnPreset[] = [
  {
    mode: "local",
    label: "This computer",
    hostPlaceholder: "127.0.0.1",
    defaultHost: "127.0.0.1",
    help: "freebankd is running on this same machine.",
    works: true,
  },
  {
    mode: "tailscale",
    label: "My node · Tailscale",
    hostPlaceholder: "100.x.y.z",
    defaultHost: "100.",
    help: "Remote-control your own node over your private Tailscale network — e.g. from a laptop while travelling. Enter the node's Tailscale IP (100.x.y.z). Your keys never leave the node.",
    works: true,
  },
  {
    mode: "tor",
    label: "My node · Tor",
    hostPlaceholder: "xxxxxxxx.onion",
    defaultHost: "",
    help: "Reach your node over Tor. Needs the desktop app plus a local Tor SOCKS proxy — the browser PWA cannot dial .onion directly. Transport not wired in this build yet.",
    works: false,
  },
  {
    mode: "custom",
    label: "Custom",
    hostPlaceholder: "host or IP",
    defaultHost: "",
    help: "Any reachable host running freebankd RPC.",
    works: true,
  },
];
