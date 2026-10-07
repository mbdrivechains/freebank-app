// "About 12 min left" while the node catches up (v0.2.9; Michael, 2026-10-07: "add an estimat to sync if that is easy
// and does not mess up your screen"). From the blocks connected over the last few minutes, so a run of empty blocks
// early on doesn't set it for good; shown only once it has a minute of data, and always as "about".

export interface SyncSample {
  /** ms */
  at: number;
  blocks: number;
}

/** The window the rate is taken over. */
const WINDOW_MS = 3 * 60_000;
/** No estimate before this much data. */
const MIN_SPAN_MS = 60_000;

/** Keep the samples a rate needs: those inside the window and the one before it. A drop in blocks (a new node, a
 *  reindex) starts again. */
export function addSample(samples: SyncSample[], s: SyncSample): SyncSample[] {
  const last = samples[samples.length - 1];
  if (last && s.blocks < last.blocks) return [s];
  if (last && s.blocks === last.blocks && s.at - last.at < 1000) return samples;
  const all = [...samples, s];
  // The window, plus the newest sample just before it, so a gap in polling still leaves a rate.
  const start = all.findIndex((x) => s.at - x.at <= WINDOW_MS);
  return all.slice(Math.max(0, start - 1));
}

/** Seconds left at the recent rate, or null when there's too little data or no progress. */
export function secondsLeft(samples: SyncSample[], tip: number | null | undefined): number | null {
  if (!tip || samples.length < 2) return null;
  const first = samples[0];
  const last = samples[samples.length - 1];
  const span = last.at - first.at;
  const done = last.blocks - first.blocks;
  if (span < MIN_SPAN_MS || done <= 0) return null;
  const left = tip - last.blocks;
  if (left <= 0) return null;
  return (left / done) * (span / 1000);
}

/** "about 12 min left", "about 2 h left", "under a minute left". */
export function sayLeft(seconds: number | null): string {
  if (seconds == null) return "";
  if (seconds < 60) return "under a minute left";
  const min = Math.round(seconds / 60);
  if (min < 90) return `about ${min} min left`;
  const h = Math.round(seconds / 360) / 10;
  return `about ${h < 10 ? h : Math.round(h)} h left`;
}
