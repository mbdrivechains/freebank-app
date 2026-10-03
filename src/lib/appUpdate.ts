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

// Checked shortly after the app starts, then twice a day while it runs.
let timer: ReturnType<typeof setInterval> | null = null;
export function startAppUpdateChecks(): void {
  if (timer) return;
  setTimeout(() => checkAppUpdate(), 15_000);
  timer = setInterval(() => checkAppUpdate(), 12 * 3600_000);
}
