// "Report a problem or suggest something" (operator, 2026-09-30). One dialog (ReportDialog.svelte),
// opened from Settings > Help, "Report this" on an error, and the Mac's Help menu. Two ways out:
//   - GitHub: a prefilled issue form in the browser (a public issue; needs an account), or, for a
//     security problem, GitHub's private vulnerability report;
//   - "Send to FreeBank": the app posts it to FreeBank's server (src-tauri/src/feedback.rs), no account.
import { writable } from "svelte/store";
import { tauriInvoke } from "./api";

export type ReportKind = "problem" | "idea" | "security";

export interface ReportDraft {
  kind: ReportKind;
  text: string;
}

/** What a report may carry about this computer; the user sees it and can leave it out. */
export interface ReportDetails {
  app: string;
  os: string;
  node: string | null;
}

/** The open dialog's starting point; null when it is closed. */
export const reportDraft = writable<ReportDraft | null>(null);

export function openReport(kind: ReportKind = "problem", text = ""): void {
  reportDraft.set({ kind, text });
}

export const report = {
  details: () => tauriInvoke("feedback_details") as Promise<ReportDetails>,
  /** Returns the report's reference on FreeBank's server. */
  send: (kind: ReportKind, text: string, contact: string, withDetails: boolean) =>
    tauriInvoke("feedback_send", { kind, text, contact, withDetails }) as Promise<string>,
};

const REPO = "https://github.com/mbdrivechains/freebank-app";
export const SECURITY_URL = `${REPO}/security/advisories/new`;

/** Percent-encode all but letters, digits and -._~ (the app's link allowlist takes nothing else). */
function enc(s: string): string {
  return encodeURIComponent(s).replace(/[!'()*]/g, (c) => "%" + c.charCodeAt(0).toString(16).toUpperCase());
}

/**
 * GitHub's new-issue page on the Problem or Idea form (.github/ISSUE_TEMPLATE), filled in through the
 * link: the text, and the details if the user keeps them. The user sees it all on GitHub before
 * anything is posted. A security problem goes to the private report instead.
 */
export function githubUrl(kind: ReportKind, text: string, d: ReportDetails | null): string {
  if (kind === "security") return SECURITY_URL;
  const fields: [string, string][] = [
    ["template", kind === "idea" ? "idea.yml" : "problem.yml"],
    ["what", text.slice(0, 1500)],
  ];
  if (d) fields.push(["version", d.app], ["system", d.os], ["node", d.node ?? ""]);
  return `${REPO}/issues/new?` + fields.filter(([, v]) => v).map(([k, v]) => `${k}=${enc(v)}`).join("&");
}
