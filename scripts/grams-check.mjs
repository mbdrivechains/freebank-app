// Gram notes (src/lib/grams.ts), checked without a browser.
//
//   npm run check:grams
//
// Fails if grams are formatted or parsed wrong, if the unit check misreads getblockchaininfo, if the per-note card (Gold B,
// node v1.2.0: the house's own rate, its sECX escrow and till, the protest, cure and failure) is wrong, if a failed house
// says "insolvent" instead of legal's "Failed: demand unpaid", if the overdue bills read as complete, if the disclosure
// drops one of legal's sentences (USER_WARNINGS §2.6) or uses a word legal rules out ("gold-backed", "gold reserves",
// "bank", "currency", "safe", "stable"; under B "equity", "solvent", "proven", "backed"); or if the notice
// (src/lib/notice.ts) reads differently in the app, the README, the release notes and NOTICE.md.
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
const g = await load("src/lib/grams.ts");

let n = 0;
const check = (what, f) => {
  f();
  n++;
  if (process.env.VERBOSE) console.log("ok", what);
};

check("fmtGrams", () => {
  assert.equal(g.fmtGrams(300000), "300");
  assert.equal(g.fmtGrams(1234567), "1,234.567");
  assert.equal(g.fmtGrams(500), "0.5");
  assert.equal(g.fmtGrams(1), "0.001");
  assert.equal(g.fmtGrams(-2500), "-2.5");
  assert.equal(g.fmtGrams(0), "0");
});

check("parseGrams", () => {
  assert.equal(g.parseGrams("1.5"), 1500);
  assert.equal(g.parseGrams("300"), 300000);
  assert.equal(g.parseGrams(".001"), 1);
  assert.equal(g.parseGrams(0.25), 250);
  assert.equal(g.parseGrams("1.2345"), null);
  assert.equal(g.parseGrams("0"), null);
  assert.equal(g.parseGrams("-1"), null);
  assert.equal(g.parseGrams("abc"), null);
  assert.equal(g.parseGrams(""), null);
  assert.equal(g.gramsInput(1234500), "1234.5");
});

check("unit from getblockchaininfo", () => {
  assert.equal(g.unitOf({ chain: "regtest", note_unit: "mg" }), "mg");
  assert.equal(g.unitOf({ chain: "easynet" }), "sat");
  assert.equal(g.unitOf(null), null);
});

check("noteAmounts", () => {
  const mg = g.noteAmounts("mg");
  assert.equal(mg.fmt(300000), "300 g");
  assert.equal(mg.parse("0.5"), 500);
  const sat = g.noteAmounts("sat");
  assert.equal(sat.fmt(150000000), "1.50000000 sECX");
  assert.equal(sat.parse("1.5"), 150000000);
});

const gold = {
  capital_counted_mg: 2000000,
  till_counted_mg: 100000,
  capital_raw_mg: 2000000,
  till_raw_mg: 100000,
  notes_mg: 2000000,
  capital_cap_mg: 20000000,
  till_cap_mg: 5000000,
  vaults: [
    { kind: "capital", partner: 0, token_id: "aa", units: 1000000 },
    { kind: "capital", partner: 0, token_id: "bb", units: 1000000 },
    { kind: "till", partner: 0, token_id: "aa", units: 50000 },
  ],
};
// Gold B (node v1.2.0): gethouse and gethousecapital as the node gives them.
const open = { effective_status: "open", lastattestheight: 812 };
const cap = {
  bid: 100000, ask: 101000, ask_30d_high: 125000, bid_90d_high: 100000, rate_fresh: true, rate_fresh_until: 1794100000,
  escrow_sats: 50000000, escrow_mg: 400000, till_attested_sats: 12500000, till_mg: 100000,
  notes_mg: 3000000, deposits_mg: 1000000, demand_interest_mg: 0, overdue_bills_seen: 2, overdue_bill_ids: [4, 9],
};

check("houseCard", () => {
  const c = g.houseCard(open, cap);
  assert.equal(c.bid, 100000);
  assert.equal(c.ask, 101000);
  assert.equal(c.ask30, 125000);
  assert.equal(c.rateFresh, true);
  assert.equal(c.escrowRatio, 0.1, "escrow over notes and deposits");
  assert.equal(c.tillRatio, 100000 / 3000000, "till over notes");
  assert.equal(c.tillAt, 812);
  assert.equal(c.overdueSeen, 2);
  assert.equal(c.failedAt, null);
  const none = g.houseCard(open, { ...cap, bid: null, ask: null, ask_30d_high: null, rate_fresh: false, escrow_mg: null, till_mg: null, notes_mg: 0, deposits_mg: 0 });
  assert.deepEqual([none.bid, none.ask, none.ask30, none.escrowRatio, none.tillRatio], [null, null, null, null, null]);
  assert.equal(g.rateWords(none), "no rate posted");
  assert.equal(g.rateWords(c), "pays 0.00100000 sECX a gram, asks 0.00101000 sECX");
});

check("status words: protested, failed with its dates, never insolvent", () => {
  assert.equal(g.statusWords("open"), "Open");
  const s = g.houseCard({ effective_status: "stressed", protest_height: 900, cure_insolvent_height: 1908 }, cap);
  assert.equal(g.statusWords(s.status, s), "Protested: a demand went unpaid; it has until about block 1,908 to pay or suspend payment");
  const f = g.houseCard({ effective_status: "insolvent", protest_height: 900, insolventheight: 1908 }, cap);
  assert.equal(g.statusWords(f.status, f), "Failed: demand unpaid (protested at block 900, failed at block 1,908)");
  assert.equal(g.statusWords("insolvent"), "Failed: demand unpaid");
  assert.equal(g.statusWords("deferred"), "Suspended: demands are queued and earn interest until paid");
  for (const st of ["open", "stressed", "deferred", "insolvent", "wounddown"]) {
    assert.ok(!/insolvent/i.test(g.statusWords(st, f)), `no "insolvent" for ${st}`);
  }
});

check("overdue bills: never complete", () => {
  assert.equal(g.overdueWords(1), "1 bill the chain sees as overdue. A house can repay a bill for its borrower, so not every bad loan shows.");
  assert.ok(g.overdueWords(2).startsWith("2 bills"));
});

check("disclosure", () => {
  const d = g.disclosure(g.houseCard(open, cap));
  for (const clause of [
    "A note is a claim on the house that issued it, not on FreeBank, its developers or any gold.",
    "paid in sECX at a rate the house sets itself (now 0.00100000 sECX a gram), or in gold tokens on Ethereum if the house delivers them.",
    "FreeBank checks only a house's sECX, never its gold tokens, and shows whether the house pays.",
    "Its sECX escrow is 10.0% of its notes and deposits.",
    "holders are paid from what the chain can seize, mainly its sECX escrow, and that may not cover them in full.",
    "can trade below ECX.",
  ]) assert.ok(d.includes(clause), `has: ${clause}`);
  assert.ok(g.disclosure(g.houseCard(open, { ...cap, bid: null })).includes("(it has none posted now)"));
  // Words legal rules out for grams, and under B.
  for (const w of [/gold-backed/i, /gold reserves/i, /\bbank\b/i, /currency/i, /\bsafe\b/i, /\bstable\b/i, /equity/i, /solvent/i, /proven/i, /\bbacked\b/i]) {
    assert.ok(!w.test(d), `no ${w}`);
  }
});

check("payout words at the house's bid", () => {
  assert.equal(g.payoutWords(300000, 100000), "about 0.30000000 sECX at the house's rate now");
  assert.equal(g.payoutWords(300000, null), null, "no rate posted");
  assert.ok(g.payoutWords(300000, 100000, 200).includes("after the house's 2% spread"));
  assert.ok(g.payoutWords(300000, 100000, 200).startsWith("about 0.29400000 sECX"));
});

check("bill faces: face_mg (whole mg) on a gram node, amount (8-decimal sECX) before", () => {
  assert.equal(g.billUnits({ face_mg: 50000, escrow: 0.001 }), 50000);
  assert.equal(g.billUnits({ face_mg: "50000" }), 50000);
  assert.equal(g.billUnits({ face_mg: 0.5 }), 0, "not whole: nothing");
  assert.equal(g.billUnits({ amount: 0.5 }), 50000000, "v1.0.2: 0.5 sECX");
  assert.equal(g.billUnits({}), 0);
  assert.equal(g.billArg(20000, "mg"), 20000);
  assert.equal(g.billArg(20000, "sat"), 0.0002);
});

// The notice (src/lib/notice.ts): the same words in the app, the README, the release notes and NOTICE.md.
const nt = await load("src/lib/notice.ts");
const { readFileSync } = await import("node:fs");
const quoted = (file) => {
  const t = readFileSync(path.resolve(here, "..", file), "utf8");
  const block = t.slice(t.indexOf("> **Notice.**"), t.indexOf("> The full notice"));
  return block.split("\n").map((l) => l.replace(/^> ?/, "")).join("\n").replace("**Notice.** ", "")
    .split(/\n\s*\n/).map((x) => x.replace(/\s+/g, " ").trim()).filter(Boolean);
};
check("the notice: one text in the app, the README and the release notes", () => {
  const n1 = nt.noticeN(true);
  assert.equal(n1.length, 3);
  assert.deepEqual(quoted("README.md"), n1, "README.md");
  const v = JSON.parse(readFileSync(path.resolve(here, "..", "package.json"), "utf8")).version;
  assert.deepEqual(quoted(`release-notes/v${v}.md`), n1, `release-notes/v${v}.md`);
  assert.ok(n1[1].includes("grams of gold"));
  assert.ok(!nt.noticeN(false).join(" ").includes("grams"), "ECX-notes chains: no sentence on grams");
  // Links read as their text: "[doc/PRODUCER_POLICY.md](https://…)" is "doc/PRODUCER_POLICY.md".
  const notice = readFileSync(path.resolve(here, "..", "NOTICE.md"), "utf8")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\s+/g, " ");
  for (const sec of nt.NOTICE_F) {
    assert.ok(notice.includes(`**${sec.title}.**`), `NOTICE.md has "${sec.title}"`);
    for (const t of [...sec.text, ...(sec.list ?? []), ...(sec.after ? [sec.after] : [])])
      assert.ok(notice.includes(t.replace("(MIT licence)", "(MIT licence, see LICENSE)")), `NOTICE.md says: ${t.slice(0, 50)}…`);
  }
  assert.equal(nt.L3_TESTNET("easynet"), "easynet is a public test network. Its coins, notes and tokens have no value, and it may be restarted. Never pay for them or accept them as payment.");
});

console.log(`grams: ${n} checks passed`);
