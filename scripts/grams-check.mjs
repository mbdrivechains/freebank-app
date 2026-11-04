// Gram notes (src/lib/grams.ts), checked without a browser.
//
//   npm run check:grams
//
// Fails if grams are formatted or parsed wrong, if the unit check misreads getblockchaininfo, if the per-note card's ratios,
// issuers or trust warnings are wrong, if the disclosure drops a clause legal's condition 4 requires or uses a word legal
// rules out for grams ("gold-backed", "gold reserves", "bank", "currency", "safe", "stable"), or if the token pool
// estimates part from the node's own maths; or if the notice (src/lib/notice.ts) reads differently in the app, the README,
// the release notes and NOTICE.md.
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
const tokens = new Map([
  ["aa", { token_id: "aa", ticker: "GOLDA", upstream: "acme-gold" }],
  ["bb", { token_id: "bb", ticker: "GOLDB", upstream: "" }],
]);

check("houseCard", () => {
  const c = g.houseCard("open", 0, gold, tokens);
  assert.equal(c.capitalRatio, 1);
  assert.equal(c.tillRatio, 0.05);
  assert.deepEqual(c.issuers, ["GOLDA (acme-gold)", "GOLDB"]);
  assert.equal(c.goldPerGram, null);
  // Deposits count with notes for capital, not for the till.
  const d = g.houseCard("open", 2000000, gold, tokens);
  assert.equal(d.capitalRatio, 0.5);
  assert.equal(d.tillRatio, 0.05);
  // A failed house: gold left per gram of notes.
  const f = g.houseCard("insolvent", 0, { ...gold, capital_raw_mg: 900000, till_raw_mg: 100000 }, tokens);
  assert.equal(f.goldPerGram, 0.5);
  // Nothing issued: no ratios rather than a division by zero.
  const e = g.houseCard("open", 0, { ...gold, notes_mg: 0 }, tokens);
  assert.equal(e.capitalRatio, null);
  assert.equal(e.tillRatio, null);
});

check("disclosure", () => {
  const t = g.disclosure(g.houseCard("open", 0, { ...gold, capital_counted_mg: 250000 }, tokens));
  assert.match(t, /^This house holds gold tokens issued by GOLDA \(acme-gold\) and GOLDB and lends the rest\./);
  assert.match(t, /Its owners' capital, 12\.5% of notes, takes losses first\./);
  for (const clause of [
    "paid in sECX at the gold price",
    "slow to move back to eCash",
    "needs eCash miners' approval",
    "can trade below ECX",
    "or a gold issuer fails, it may not pay in full",
    "If no gold price is available, it may pay in gold tokens.",
  ]) {
    assert.ok(t.includes(clause), clause);
  }
  // Never the 10% floor as what the house holds.
  assert.ok(!/\b10(\.0)?%/.test(t));
  for (const banned of [/gold[- ]backed/i, /backed by gold/i, /gold reserves/i, /\bbank\b/i, /currency/i, /\bsafe\b/i, /\bstable\b/i]) {
    assert.ok(!banned.test(t), String(banned));
  }
});

check("payout price and words", () => {
  const fix = { payout_view_sats_per_gram: 110000, stale: false };
  // The chain's own price (listtokenpools) wins, null included; the oracle's median only when that call failed.
  assert.equal(g.payoutPrice(fix, null), 110000);
  assert.equal(g.payoutPrice({ ...fix, stale: true, payout_view_sats_per_gram: null }, null), null);
  assert.equal(g.payoutPrice({ ...fix, stale: true }, { payout_price_sats_per_gram: 115000, pools: [] }), 115000);
  assert.equal(g.payoutPrice(fix, { payout_price_sats_per_gram: null, pools: [] }), null);
  assert.equal(g.payoutWords(300000, 110000), "about 0.33000000 sECX at today's price");
  assert.equal(g.payoutWords(300000, null), null);
  assert.equal(g.payoutWords(300000, 110000, 0), "about 0.33000000 sECX at today's price");
  assert.equal(g.payoutWords(300000, 110000, 400), "about 0.31680000 sECX at today's price, after the house's 4% spread (a house that isn't open owes one while its till is under 2% of its notes)");
  assert.equal(g.payoutWords(300000, 110000, 25), "about 0.32917500 sECX at today's price, after the house's 0.25% spread (a house that isn't open owes one while its till is under 2% of its notes)");
});

check("trust, frozen gold and equity on the card", () => {
  const t2 = new Map(tokens);
  t2.set("bb", { token_id: "bb", ticker: "GOLDB", upstream: "beta-gold", trusted: false, untrusted_since: 812 });
  const c = g.houseCard("open", 0, { ...gold, frozen: [{ token_id: "aa", frozen: 4000, grace_ends: 900, counts_zero: false }], equity_mg: -5, demand_interest_mg: 120 }, t2);
  assert.deepEqual(c.untrusted, ["GOLDB (beta-gold) has failed the chain's checks on gold tokens since block 812 (an issuer bond, backing reports that cover its supply, and redemptions paid on time): its gold stops counting about 30 days after"]);
  assert.equal(c.frozenMg, 4000);
  assert.equal(c.equityMg, -5);
  assert.equal(c.demandInterestMg, 120);
  // An older gram build without the fields: nothing to warn about.
  const o = g.houseCard("open", 0, gold, tokens);
  assert.deepEqual(o.untrusted, []);
  assert.equal(o.frozenMg, 0);
  assert.equal(o.equityMg, null);
  assert.equal(o.demandInterestMg, 0);
});

check("pool bounds: 1% each way", () => {
  assert.equal(g.leastOut(109488), 108393);
  assert.equal(g.mostIn(1096348), 1107312);
  assert.equal(g.leastOut(0), 0);
  assert.equal(g.mostIn(1), 2, "rounded up");
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

check("gold price words", () => {
  const fix = { payout_view_sats_per_gram: 110000, stale: false, gold_ecx_last_fix: { value: 110000, reporters: 3, fixheight: 73, ageblocks: 2 }, gold_usd_last_fix: null };
  assert.equal(g.goldFixWords(fix, 110000), "0.00110000 sECX a gram, from 3 reporters, 2 blocks ago");
  assert.match(g.goldFixWords({ ...fix, stale: true, payout_view_sats_per_gram: null }, null), /^None right now: the reporters' last price is 2 blocks ago, too old to pay at\. Payouts in sECX wait, and bills don't fall overdue\.$/);
  assert.match(g.goldFixWords(null, null), /^None yet: no reporter/);
});

check("token pool maths match the node", () => {
  // pool.cpp PoolSwapOut: inFee = in * (10000 - fee); out = inFee * Rout / (Rin * 10000 + inFee), floored.
  assert.equal(g.swapOut(1000, 600000, 66000000, 30), 109488); // sell 1 g: worked out separately in Python
  assert.equal(g.swapOut(110000, 66000000, 600000, 30), 995); // buy with 0.0011 sECX
  assert.equal(g.swapOut(0, 600000, 66000000, 30), null);
  assert.equal(g.swapOut(1000, 0, 66000000, 30), null);
  assert.equal(g.swapOut(1, 1, 1, 30), null); // would empty the out side, or pay nothing
  const pool = { token_id: "aa", lp_token_id: "lp", units: 600000, sats: 66000000, lp_supply: 6292853, fee_bps: 30 };
  assert.equal(g.addSats(1000, pool), 110000);
  assert.equal(g.addSats(1, { ...pool, sats: 66000001 }), 111); // rounded up, as the wallet does
  assert.deepEqual(g.shareOf(6292853, pool), { units: 600000, sats: 66000000 });
  assert.deepEqual(g.shareOf(0, pool), { units: 0, sats: 0 });
});

check("wallet holdings", () => {
  const h = g.holdings([
    { token_id: "aa", units: 300, frozen: 0 },
    { token_id: "aa", units: 200, frozen: 50 },
    { token_id: "bb", units: 7, frozen: 0 },
  ]);
  assert.deepEqual(h.get("aa"), { units: 500, frozen: 50 });
  assert.deepEqual(h.get("bb"), { units: 7, frozen: 0 });
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
  const notice = readFileSync(path.resolve(here, "..", "NOTICE.md"), "utf8").replace(/\s+/g, " ");
  for (const sec of nt.NOTICE_F) {
    assert.ok(notice.includes(`**${sec.title}.**`), `NOTICE.md has "${sec.title}"`);
    for (const t of [...sec.text, ...(sec.list ?? []), ...(sec.after ? [sec.after] : [])])
      assert.ok(notice.includes(t.replace("(MIT licence)", "(MIT licence, see LICENSE)")), `NOTICE.md says: ${t.slice(0, 50)}…`);
  }
  assert.equal(nt.L3_TESTNET("easynet"), "easynet is a public test network. Its coins, notes and tokens have no value, and it may be restarted. Never pay for them or accept them as payment.");
});

console.log(`grams: ${n} checks passed`);
