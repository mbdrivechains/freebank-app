// Do the gold-ghosted QR codes read as well as plain ones? Draws each code both ways from the
// encoder's own output (qrMatrix, qrPath's squares, qrGhost's shades), clean, blurred and as
// washed-out, noisy copies, and reads them back with the jsQR decoder.
//
//   npm run check:qr        the app's encoder (src/lib/qr.ts)
//   npm run check:qr -- F   another file with the same exports (qrMatrix, qrGhost). The phone page's encoder
//                           has its own read-back test (freebank-distribution phone/src/lib/qr.test.ts).
//
// Fails if a clean or blurred ghost doesn't read where plain does, or if the ghost reads a tenth of
// the washed-out copies fewer than plain (at the 60% gold, 2026-09-30: pairing 38 against 44 of 100).
// QR_SEEDS=n sets how many washed-out copies (100).
import { buildSync } from "esbuild";
import jsQR from "jsqr";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const src = path.resolve(here, "..", process.argv[2] ?? "src/lib/qr.ts");
const out = buildSync({ entryPoints: [src], bundle: true, format: "esm", write: false, logLevel: "error" });
const { qrMatrix, qrGhost } = await import("data:text/javascript," + encodeURIComponent(out.outputFiles[0].text));

const b64u = (s) => Buffer.from(s).toString("base64url");
const pair = JSON.stringify({
  v: 1,
  relay: "wss://app.ecxfreebank.com/ws",
  room: "Qm9vbXRvd25Sb29tSWQxMj",
  d: "BPx" + "k3Jd9aQz".repeat(10) + "Zq2Ab",
  c: "n4Xy8Kq2Lm7Pw3Rt9Va1Bc",
});
const CODES = {
  receive: "XGmPzFGZSivshTisB3JLSC25SFNEgy2iSS",
  deposit: "s130_XGmPzFGZSivshTisB3JLSC25SFNEgy2iSS_a1b2c3",
  pairing: "https://app.ecxfreebank.com/#pair=" + b64u(pair),
};

// A seeded generator, so every run draws the same noise.
function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
}

/** The code as grey levels, `px` pixels a module, with the 4-module quiet zone. */
function draw(m, ghost, px) {
  const n = m.length + 8;
  const w = n * px;
  const g = new Float64Array(w * w).fill(255);
  const fill = (x, y, v) => {
    for (let dy = 0; dy < px; dy++) for (let dx = 0; dx < px; dx++) g[(y * px + dy) * w + x * px + dx] = v;
  };
  if (ghost) {
    for (const { d, fill: hex } of qrGhost(m, 4)) {
      const [r, gg, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
      const luma = 0.299 * r + 0.587 * gg + 0.114 * b;
      for (const [, x, y] of d.matchAll(/M(\d+) (\d+)/g)) fill(+x, +y, luma);
    }
  }
  m.forEach((row, y) => row.forEach((dark, x) => dark && fill(x + 4, y + 4, 0)));
  return { g, w };
}

const blur = ({ g, w }) => {
  const o = new Float64Array(g.length);
  for (let y = 0; y < w; y++)
    for (let x = 0; x < w; x++) {
      let s = 0, c = 0;
      for (let dy = -1; dy <= 1; dy++)
        for (let dx = -1; dx <= 1; dx++) {
          const xx = x + dx, yy = y + dy;
          if (xx >= 0 && yy >= 0 && xx < w && yy < w) (s += g[yy * w + xx]), c++;
        }
      o[y * w + x] = s / c;
    }
  return { g: o, w };
};

/** A washed-out, noisy copy: contrast squeezed toward light grey, then noise. */
const washed = ({ g, w }, seed) => {
  const r = rng(seed);
  const o = g.map((v) => 90 + v * 0.62 + (r() + r() + r() - 1.5) * 70);
  return { g: o, w };
};

const reads = ({ g, w }, text) => {
  const rgba = new Uint8ClampedArray(w * w * 4);
  g.forEach((v, i) => rgba.set([v, v, v, 255], i * 4));
  const r = jsQR(rgba, w, w, { inversionAttempts: "dontInvert" });
  return !!r && r.data === text;
};

let failed = false;
const SEEDS = Number(process.env.QR_SEEDS ?? 100);
console.log(`${path.relative(process.cwd(), src)}\n${"".padEnd(26)}plain   ghost`);
for (const [name, text] of Object.entries(CODES)) {
  const m = qrMatrix(text, "M");
  for (const px of [8, 3, 2]) {
    for (const [how, f] of [["clean", (x) => x], ["blurred", blur]]) {
      const [p, gh] = [false, true].map((ghost) => reads(f(draw(m, ghost, px)), text));
      console.log(`${name} ${how} ${px}px`.padEnd(26) + `${p ? "reads" : "FAILS"}   ${gh ? "reads" : "FAILS"}`);
      if (p && !gh) failed = true;
    }
  }
  const count = (ghost) => {
    let ok = 0;
    for (let s = 1; s <= SEEDS; s++) ok += reads(washed(draw(m, ghost, 3), s * 7919), text);
    return ok;
  };
  const [p, gh] = [count(false), count(true)];
  console.log(`${name} washed-out 3px`.padEnd(26) + `${p}/${SEEDS}`.padEnd(8) + `${gh}/${SEEDS}`);
  if (gh < p - SEEDS / 10) failed = true;
}
console.log(failed ? "\nThe ghost reads worse than plain." : "\nThe ghost reads as well as plain.");
process.exit(failed ? 1 : 0);
