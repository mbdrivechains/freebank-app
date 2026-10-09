// The app updates itself (v0.2.4; src-tauri/src/app_update.rs). A release is offered only once its SHA256SUMS is
// signed with FreeBank's release key. An AppImage or a Mac app replaces itself and restarts; the Debian package is
// updated by Software Updater (FreeBank's apt repository) or with the new .deb from the release page.

import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

export interface AppUpdateCheck {
  current: string;
  /** The latest release whose SHA256SUMS is signed. */
  latest: string | null;
  available: boolean;
  /** "self" (replaces itself and restarts), "apt" (Debian package, apt repository set up), "deb" (Debian package
   *  without it), "download" (anything else; `why` says why). */
  how: "self" | "apt" | "deb" | "download";
  why: string | null;
  /** The latest release's page. */
  page: string | null;
  error: string | null;
}

export interface AppUpdateProgress {
  running: boolean;
  /** signature, download, verify, replace, restart */
  stage: string;
  version: string | null;
  bytes: number;
  total: number | null;
  /** While the restart waits for the node ("Stopping FreeBank…"). */
  note: string | null;
  error: string | null;
}

export const APT_PAGE = "https://apt.ecxfreebank.com";

/** The .deb without FreeBank's apt repository set up: offer "Get FreeBank updates with Software Updater" (v0.2.9,
 *  opt in). Asked once on its own; always in Settings > App updates. */
export const aptOffer = writable(false);

export async function loadAptOffer(): Promise<boolean> {
  try {
    const offer = (await tauriInvoke("apt_updates_offer")) as boolean;
    aptOffer.set(offer);
    return offer;
  } catch {
    aptOffer.set(false);
    return false;
  }
}

/** Writes FreeBank's apt source through the computer's password prompt. */
export async function enableAptUpdates(): Promise<void> {
  await tauriInvoke("apt_updates_enable");
  aptOffer.set(false);
  await checkAppUpdate(true);
}

const APT_ASKED = "freebank.aptAsked";

/** Whether the one-time question was answered (or put off for good) on this computer. */
export function aptAsked(): boolean {
  try {
    return localStorage.getItem(APT_ASKED) === "1";
  } catch {
    return false;
  }
}

export function setAptAsked(): void {
  try {
    localStorage.setItem(APT_ASKED, "1");
  } catch {
    // Without storage it asks again next time; harmless.
  }
}

export const appUpdate = writable<AppUpdateCheck | null>(null);
export const appUpdateProgress = writable<AppUpdateProgress | null>(null);
/** "Later" on the notice: hidden until the app starts again. */
export const appUpdateLater = writable(false);

export async function checkAppUpdate(force = false): Promise<AppUpdateCheck | null> {
  try {
    const c = (await tauriInvoke("app_update_check", { force })) as AppUpdateCheck;
    appUpdate.set(c);
    return c;
  } catch {
    return null;
  }
}

let watching = false;
async function watch(): Promise<void> {
  if (watching) return;
  watching = true;
  try {
    for (;;) {
      const p = (await tauriInvoke("app_update_progress")) as AppUpdateProgress;
      appUpdateProgress.set(p);
      // Once restarting, the app closes on its own; until then, keep reading.
      if (!p.running) return;
      await new Promise((r) => setTimeout(r, 500));
    }
  } catch {
    // The app is closing for the restart.
  } finally {
    watching = false;
  }
}

/** "Update and restart". Throws what keeps it from starting ("Please wait: …"). */
export async function startAppUpdate(): Promise<void> {
  await tauriInvoke("app_update_start");
  await watch();
}

const STAGES: Record<string, string> = {
  signature: "Checking the release's signature…",
  download: "Downloading",
  verify: "Checking the download against the signed checksums…",
  replace: "Putting the new version in place…",
  restart: "Restarting FreeBank…",
};

export function stageText(p: AppUpdateProgress): string {
  return STAGES[p.stage] ?? "Updating…";
}

// Automatic updates (v0.4.2, opt in): app_update.rs `auto_round`. With the switch on, a signed release is fetched,
// checked and put in place without asking; it runs from the next start (`installed` until then).
export interface AutoUpdate {
  on: boolean;
  /** Put in place by itself, not running yet. */
  installed: string | null;
  /** This copy can update itself (an AppImage, or the Mac app in Applications). */
  can: boolean;
  /** The last automatic round that failed: that version waits for "Update and restart". */
  failed: { version: string; reason: string; at: number } | null;
}
export const autoUpdate = writable<AutoUpdate | null>(null);

export async function loadAutoUpdate(): Promise<AutoUpdate | null> {
  try {
    const a = (await tauriInvoke("app_auto_update_get")) as AutoUpdate;
    autoUpdate.set(a);
    return a;
  } catch {
    autoUpdate.set(null);
    return null;
  }
}

export async function setAutoUpdate(on: boolean): Promise<void> {
  await tauriInvoke("app_auto_update_set", { on });
  await loadAutoUpdate();
}

/** "Restart now": open the version an automatic update put in place. */
export async function restartForUpdate(): Promise<void> {
  await tauriInvoke("app_update_restart");
  await watch();
}

// Checked shortly after the app starts, then twice a day while it runs; what automatic updates did, every hour.
let timer: ReturnType<typeof setInterval> | null = null;
export function startAppUpdateChecks(): void {
  if (timer) return;
  setTimeout(() => checkAppUpdate(), 15_000);
  loadAutoUpdate();
  timer = setInterval(() => checkAppUpdate(), 12 * 3600_000);
  setInterval(() => loadAutoUpdate(), 3600_000);
}
