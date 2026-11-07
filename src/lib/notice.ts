// The notice and its short forms, in legal's words (November 2026). The same texts open the release page, the README
// and NOTICE.md, and the ? helper answers from them. Where each shows: the first-run notice (NoticeGate, once, with
// "I understand", kept on this device only), Settings › Help › About and legal (the full notice), and one line where
// a step needs it: L1 wallet creation, L2 a deposit, L4 the changer, R1 house tools, R4 pools, R5 bills.

/** Bumped when the first-run notice changes, so it is shown, and agreed to, again. */
export const NOTICE_VERSION = "2026-11-06";

/** The standard notice (N). The sentence on grams is for gram-notes chains (FreeBank v1.1.0 and later: easynet now);
 *  a chain whose notes count ECX goes without it. */
export function noticeN(grams: boolean): string[] {
  return [
    "FreeBank is experimental, open-source software. It has not been audited, and you can lose what you put into it. " +
      "FreeBank is not a bank. Nothing on it is insured or guaranteed by any government or by its developers.",
    "Houses are run by independent operators, not by FreeBank's developers. A house can suspend payment or fail, and " +
      "its notes may not be paid in full. " +
      (grams
        ? "Notes are denominated in grams of gold and paid in sECX (ECX held on FreeBank) at an on-chain price, or in " +
          "gold tokens. "
        : "") +
      "Moving sECX back to eCash is slow and needs eCash miners' approval, and sECX can trade below ECX.",
    "Running a house, issuing tokens, reporting prices or lending to consumers may need a licence where you do it. " +
      "Nothing here is an offer of a financial product, or financial, legal or tax advice. FreeBank is not offered " +
      "where using it would be unlawful. You are responsible for the laws and taxes that apply to you.",
  ];
}

/** The short line (S). */
export const NOTICE_S =
  "FreeBank is experimental software: you can lose what you put in. It is not a bank; houses are run by independent " +
  "operators and can fail. Not advice.";

/** One-line facts (L). */
export const L1_KEYS =
  "Your wallet's keys are yours alone. Nobody, including FreeBank's developers, can recover your funds or reverse a payment.";
export const L2_DEPOSIT =
  "You're moving ECX onto FreeBank as sECX. Moving it back to eCash is slow, needs eCash miners' approval, and isn't guaranteed.";
export const L3_TESTNET = (name: string) =>
  `${name} is a public test network. Its coins, notes and tokens have no value, and it may be restarted. Never pay ` +
  "for them or accept them as payment.";
export const L4_CHANGER = "Changers are run by independent operators, not by FreeBank's developers.";

/** Role lines (R), where a role that may need a licence is taken on. */
export const R1_HOUSE =
  "Running a house for others (issuing notes, lending them out, taking deposits or discounting bills) may be banking, " +
  "deposit-taking or a financial service where you do it, and need a licence. Gram notes may count as financial " +
  "products, such as derivatives. That is your responsibility, not FreeBank's developers'.";
export const R2_TOKEN =
  "Issuing tokens against gold or dollar tokens you hold for others is custody and token issuance, which needs a " +
  "licence in most countries.";
export const R3_REPORTER =
  "Your reports set what others are paid. Knowingly false or manipulated prices are an offence in many countries.";
export const R4_POOL = "Pool shares can lose value against what you put in. A token is only as good as its issuer.";
export const R5_BILL =
  "Bills are for trade between businesses. Credit to consumers, buy-now-pay-later-style credit included, needs a " +
  "licence in many countries.";

/** The full notice (F): Settings › Help › About and legal, and NOTICE.md. */
export const NOTICE_F: { title: string; text: string[]; list?: string[]; after?: string }[] = [
  {
    title: "What FreeBank is",
    text: [
      "FreeBank is open-source software (MIT licence), provided as is, with no warranty. Its developers write and " +
        "publish the code. They don't run houses, money changers or custody, don't issue tokens or report prices, and " +
        "don't check anyone who does.",
    ],
  },
  {
    title: "You can lose what you put in",
    text: [
      "The software is experimental and has not been audited; it may have bugs. Your keys are yours alone: nobody, " +
        "including the developers, can recover your funds or reverse a payment.",
    ],
  },
  {
    title: "Blocks can stop",
    text: [
      "FreeBank has new blocks only while someone bids for them on eCash. Until 31 Oct 2027 its developer bids a fixed " +
        "amount so that blocks keep coming (doc/PRODUCER_POLICY.md); anyone may bid, and the highest bid wins. After " +
        "that date, if nobody bids, FreeBank stops: coins on it can't move or be withdrawn until someone does.",
    ],
  },
  {
    title: "Not a bank, no guarantee",
    text: ["FreeBank is not a bank. Nothing on it is insured or guaranteed by any government or by its developers."],
  },
  {
    title: "Houses and notes",
    text: [
      "A note is a claim on the house that issued it, not on FreeBank, its developers or any gold. Houses are run by " +
        "independent operators. A house can suspend payment (it then owes interest on queued demands for up to 12 " +
        "months) or fail. If it fails, holders are paid from what the chain can seize, and that may not cover them in " +
        "full. Every note shows what its house holds and how it can fail.",
    ],
  },
  {
    title: "Grams and sECX",
    text: [
      "Notes are denominated in grams of gold. A house pays them in sECX at an on-chain gold price, or in gold " +
        "tokens, gram for gram. When you redeem, you sign for what you are paid. If you make a formal demand, the " +
        "house can pay it without you, and it chooses: sECX at a recent on-chain price, or gold tokens, the only " +
        "choice when no price is available. Gold tokens can trade below gold and can be frozen by their issuer. If no " +
        "price is available, a redemption in sECX needs an amount you both sign. sECX is ECX held on FreeBank. " +
        "Moving it back to eCash is slow, needs eCash miners' approval and isn't guaranteed, and sECX can trade " +
        "below ECX. ECX's price can fall.",
    ],
  },
  {
    title: "Gold tokens and pools",
    text: [
      "A gold token is only as good as its issuer, who holds the backing elsewhere, and some issuers can freeze their " +
        "tokens. Passing the chain's checks is not a guarantee. Pool shares can lose value against what you put in.",
    ],
  },
  {
    title: "Roles that may need a licence",
    text: ["In many countries:"],
    list: [
      "issuing notes payable on demand and lending them out may be banking or deposit-taking;",
      "gram notes may be financial products, such as derivatives;",
      "lending to consumers needs a licence, buy-now-pay-later-style credit included;",
      "holding tokens for others, or issuing your own against them, is custody and token issuance;",
      "paying out sECX for notes may count as a crypto-asset service;",
      "customer-check, sanctions and reporting laws apply to anyone serving the public;",
      "knowingly false prices are an offence.",
    ],
    after: "If you take on any of these roles, the law where you do it is your responsibility, not the developers'.",
  },
  {
    title: "Not an offer, not advice",
    text: [
      "Nothing in FreeBank, its documents, its app or its assistant is an offer of a financial product, or financial, " +
        "legal or tax advice. FreeBank is not offered where using it would be unlawful. You are responsible for the " +
        "laws and taxes that apply to you.",
    ],
  },
  {
    title: "Test networks",
    text: ["easynet is a public test network. Its coins, notes and tokens have no value, and it may be restarted."],
  },
];

const KEY = "freebank.notice";

/** Whether this device has agreed to the current first-run notice. Storage that can't be read counts as not yet. */
export function noticeAgreed(): boolean {
  try {
    return localStorage.getItem(KEY) === NOTICE_VERSION;
  } catch {
    return false;
  }
}

/** Records "I understand" on this device only; nothing is sent anywhere. */
export function agreeNotice(): void {
  try {
    localStorage.setItem(KEY, NOTICE_VERSION);
  } catch {
    /* shown again next start */
  }
}
