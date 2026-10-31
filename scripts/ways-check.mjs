// "Ways to use FreeBank" (src/lib/ways.ts) and the bond claim (src/lib/bills.ts), checked without a browser.
//
//   npm run check:ways
//
// Fails if a guide drops a line FreeBank's legal notes require (2026-10-31: every guide is general information, says
// who carries the risk, that houses run themselves and need licences, that consumer lending needs one in many countries, "if and when" for
// withdrawals, that it is experimental, and that tax is the user's), if any text uses a word the notes rule out
// ("best", "we recommend", "safe", "banker", backing claims), if a step points at a screen the app hasn't got, or if
// a calculator or the claim rule gives a wrong answer.
import { buildSync } from "esbuild";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
async function load(file) {
  const out = buildSync({
    entryPoints: [path.resolve(here, "..", file)],
    bundle: true,
    format: "esm",
    write: false,
    logLevel: "error",
    nodePaths: [path.resolve(here, "..", "node_modules")],
  });
  return import("data:text/javascript," + encodeURIComponent(out.outputFiles[0].text));
}
const { WAYS, COMMON, blocksFor, inBase, bondFor, suggestion, wayById } = await load("src/lib/ways.ts");
const { canClaimBond, claimableFrom } = await load("src/lib/bills.ts");

let n = 0;
function check(name, fn) {
  fn();
  n++;
  console.log(`ok ${n} - ${name}`);
}

check("every guide is complete and its id unique", () => {
  const ids = new Set();
  for (const w of WAYS) {
    for (const k of ["id", "goal", "title", "summary", "lends", "risk", "cost", "inAppNote"])
      assert.ok(typeof w[k] === "string" && w[k].trim().length > 0, `${w.id}: ${k}`);
    assert.ok(["yes", "partly", "not yet"].includes(w.inApp), `${w.id}: inApp`);
    assert.ok(w.steps.length > 0, `${w.id}: steps`);
    assert.ok(Array.isArray(w.gaps), `${w.id}: gaps`);
    assert.ok(!ids.has(w.id), `duplicate ${w.id}`);
    ids.add(w.id);
    assert.equal(wayById(w.id), w);
  }
  assert.equal(wayById("nope"), undefined);
});

check("the lines every guide carries", () => {
  const all = COMMON.join(" ");
  for (const must of [
    "General information, not advice",
    "doesn't take your situation into account",
    "their own operators, not by FreeBank",
    "need any licence their country requires",
    "lending to consumers needs a licence",
    "buy-now-pay-later-style credit included",
    "Lending to businesses often doesn't",
    "if and when eCash's miners approve",
    "experimental",
    "you can lose what you put in",
    "tax is yours",
  ])
    assert.ok(all.includes(must), `COMMON lacks "${must}"`);
});

check("guides with a house's paper say it can suspend or fail, and what a failure pays", () => {
  for (const id of ["sell-a-bill", "borrow", "notes", "pools", "term-deposit", "run-a-house"]) {
    const r = wayById(id).risk;
    assert.match(r, /fail/, `${id}: failure`);
    assert.match(r, /rank first|paid first/, `${id}: who is paid first`);
    if (id !== "run-a-house") assert.match(r, /suspend/, `${id}: suspension`);
    if (id !== "run-a-house") assert.match(r, /may not cover/, `${id}: the limit`);
  }
  assert.match(wayById("move-coins").summary, /if and when/);
  assert.match(wayById("run-a-house").risk, /may need to register or be licensed where it operates, as a crypto-asset service/);
  assert.match(wayById("run-a-house").risk, /custody and issuer licences/);
  assert.match(wayById("sell-a-bill").risk, /never from you/); // consensus: no recourse on house-held paper
});

check("no word the legal notes rule out", () => {
  const text = JSON.stringify({ WAYS, COMMON });
  const banned =
    /\b(best|suits? you|we recommend|recommended|safe|safely|guarantee[ds]?|stable|banker|risk-free|money|fully backed|gold-backed|backed by gold)\b/i;
  const hit = text.match(banned);
  assert.equal(hit, null, hit ? `found "${hit[0]}"` : "");
});

check("every step's screen exists", () => {
  for (const w of WAYS)
    for (const s of w.steps) {
      if (!s.go) continue;
      assert.ok(["home", "credit", "ecash"].includes(s.go.view), `${w.id}: view ${s.go.view}`);
      if (s.go.view === "credit") assert.ok(["notes", "houses", "pools", "bills"].includes(s.go.seg), `${w.id}: seg ${s.go.seg}`);
      else assert.equal(s.go.seg, undefined, `${w.id}: a segment off Credit`);
      if (s.go.home !== undefined) {
        assert.equal(s.go.view, "home", `${w.id}: a Home panel off Home`);
        assert.ok(["deposit", "withdraw"].includes(s.go.home), `${w.id}: home ${s.go.home}`);
      }
      assert.ok(s.go.label.length > 0, `${w.id}: label`);
    }
});

check("days to blocks", () => {
  assert.equal(blocksFor(90), 12960);
  assert.equal(blocksFor(1.5), 216);
  assert.equal(blocksFor(0.001), 1);
  for (const bad of [0, -3, NaN, Infinity]) assert.equal(blocksFor(bad), 0);
});

check("a price to sECX, and a bond as a share", () => {
  assert.equal(inBase(10000, 0.5), 20000);
  assert.equal(inBase(10, 3), 3.33333333);
  for (const [p, r] of [[0, 1], [10, 0], [NaN, 1], [10, -1]]) assert.equal(inBase(p, r), 0);
  assert.equal(bondFor(20000, 10), 2000);
  assert.equal(bondFor(3.33333333, 25), 0.83333333);
  assert.equal(bondFor(1, 150), 1);
  for (const [f, p] of [[0, 10], [10, 0], [NaN, 10]]) assert.equal(bondFor(f, p), 0);
});

check("a suggestion names the guide and the gap", () => {
  const w = wayById("sell-on-credit");
  const t = suggestion(w, w.gaps[0]);
  assert.ok(t.includes(w.title) && t.includes(w.gaps[0]) && t.endsWith("What I wanted to do: "));
});

check("the bond can be claimed by the holder only once the next block is past maturity + grace", () => {
  const b = { status: "a", roles: ["holder"], maturity_height: 1000, grace_blocks: 1008 };
  assert.equal(claimableFrom(b), 2009);
  assert.equal(canClaimBond(b, 2007), false); // the next block, 2008, is maturity + grace: "bad-bill-claim-early"
  assert.equal(canClaimBond(b, 2008), true);
  assert.equal(canClaimBond({ ...b, status: "d" }, 3000), false); // claimed already
  assert.equal(canClaimBond({ ...b, status: "r" }, 3000), false); // paid
  assert.equal(canClaimBond({ ...b, roles: ["acceptor"] }, 3000), false);
  assert.equal(canClaimBond({ ...b, roles: undefined }, 3000), false);
  assert.equal(canClaimBond({ ...b, owner_house_id: 2 }, 3000), false); // the house's to claim
  assert.equal(canClaimBond({ ...b, owner_house_id: 0, roles: ["drawer", "holder"] }, 3000), true);
});

console.log(`all ${n} checks passed`);
