// "Ways to use FreeBank" (v0.6.12): a goal menu ("What do you want to do?") and, for each goal, a fixed guide to the
// FreeBank tools that do it: who lends, who carries the risk, what it costs, what the app does today, the steps, and
// what FreeBank can't do yet, each with "Send as a suggestion". The same text for everyone: general information, never
// a pick for someone's situation, so nothing here asks for or uses a user's own figures except the bill calculator,
// which only converts what they type. The wording follows FreeBank's legal notes (no "best", "suits you" or "we
// recommend"; every guide carries COMMON). `npm run check:ways` checks both.
import { writable } from "svelte/store";

export type Seg = "notes" | "houses" | "pools" | "bills";
export type View = "home" | "credit" | "ecash";

/** A screen the guide can take you to. */
export interface Target {
  view: View;
  seg?: Seg;
  /** On Home: the panel to open. */
  home?: "deposit" | "withdraw";
}

export interface Step {
  /** Who does it, when the guide has two sides ("You", "Your customer", "The house"). */
  who?: string;
  text: string;
  go?: Target & { label: string };
}

export interface Way {
  id: string;
  /** The line in the goal menu. */
  goal: string;
  title: string;
  summary: string;
  lends: string;
  risk: string;
  cost: string;
  inApp: "yes" | "partly" | "not yet";
  inAppNote: string;
  steps: Step[];
  /** The bill calculator: days to blocks, a price to sECX at a rate typed in, a bond as a share. */
  calc?: boolean;
  /** What FreeBank can't do yet for this goal; each gets "Send as a suggestion". */
  gaps: string[];
}

/** Said in every guide (FreeBank's legal notes, 2026-10-31). */
export const COMMON: string[] = [
  "General information, not advice: it doesn't take your situation into account.",
  "Houses are run by their own operators, not by FreeBank, and need any licence their country requires.",
  "In many countries, lending to consumers needs a licence, buy-now-pay-later-style credit included. Lending to businesses often doesn't.",
  "Withdrawals to eCash are paid if and when eCash's miners approve them.",
  "FreeBank is experimental, pre-audit software: you can lose what you put in. On a test network the coins are test coins.",
  "Costs here are facts about the chain. Any tax is yours to work out.",
];

/** What holding a house's notes risks, in the words every guide uses. */
const HOUSE_RISK =
  "A stressed house may suspend redemption; demanded notes then earn interest until paid. If a house fails, its note-holders rank first in what the chain can seize (partners' escrow and posted reserves), and that may not cover them in full.";

export const BLOCKS_PER_DAY = 144;

export const WAYS: Way[] = [
  {
    id: "sell-on-credit",
    goal: "Give a customer time to pay",
    title: "Give a customer time to pay",
    summary:
      "Your customer writes a bill: a promise to pay you a set amount of sECX at a set block, with a bond they lock until then. You hold the bill. At the due date they pay it, and their bond goes back to them. If it isn't paid by the end of the grace period, you can claim the bond.",
    lends: "You do: the goods go now, and you are paid later.",
    risk: "You do. If the customer doesn't pay, you can claim the bond, which may be less than the bill. Until the due date your price is in sECX, so its worth in other currencies moves with sECX.",
    cost: "Network fees: the customer pays them to write and to pay the bill. The customer's bond is locked until the bill is paid. FreeBank charges nothing.",
    inApp: "yes",
    inAppNote:
      "The customer writes the whole bill from their own wallet and hands it to you: the app can't yet let you draw a bill for them to sign.",
    calc: true,
    steps: [
      { who: "You", text: "Make a bill key and send it to your customer.", go: { view: "credit", seg: "bills", label: "Bills › Receive a bill" } },
      {
        who: "Your customer",
        text: "Under House tools: issue a bill, enter the price as the face amount, the bond you agreed, the days to the due date as blocks (144 a day), and the blocks to pay it in (1,008 is about a week).",
        go: { view: "credit", seg: "bills", label: "Bills › House tools" },
      },
      { who: "Your customer", text: "On the new bill, choose Endorse and paste your bill key. The bill then shows in your Bills, with you as holder." },
      { who: "Your customer", text: "At the due date, choose Retire on the bill: it pays you the face amount, and their bond comes back." },
      { who: "You", text: "If it isn't paid by the end of the grace period, choose Claim the bond on the bill." },
    ],
    gaps: [
      "Draw a bill for a customer to sign: the seller proposes the bill and the customer accepts it and posts the bond.",
      "Price a bill in dollars or in grams of gold, so a long bill doesn't move with sECX.",
      "Instalments: one bill paid in parts, tracked as one.",
      "Due dates as calendar dates, not block numbers.",
    ],
  },
  {
    id: "sell-a-bill",
    goal: "Get paid now for a bill you hold",
    title: "Get paid now for a bill you hold",
    summary:
      "Sell a bill you hold to a discount house. The house pays you now in its own notes, less a discount you agree, and collects from the payer at the due date. Its notes are redeemable for sECX at par while the house is open.",
    lends: "The house, from the moment it holds the bill.",
    risk: `Once the house holds the bill, the payer's risk is the house's: on-chain it can collect only from the payer and the bond, never from you (any other agreement you make with the house is outside the chain). While you hold the house's notes, you carry the house's risk. ${HOUSE_RISK}`,
    cost: "The discount (the house's price below the face amount) and network fees.",
    inApp: "not yet",
    inAppNote:
      "Selling a bill to a house is three messages between you and the house, from the command line today (FreeBank guide, section 4.3: proposediscount, signdiscount, completediscount). The house needs room under its cap and a recent attestation.",
    steps: [
      { who: "You", text: "Look at the houses: their status and attested reserves. Agree a price with a house's operator.", go: { view: "credit", seg: "houses", label: "Credit › Houses" } },
      { who: "You", text: "From the command line, propose the sale: proposediscount with the bill, the house and the price. Send the proposal to the house." },
      { who: "The house", text: "Signs it (signdiscount) and sends it back." },
      { who: "You", text: "Complete it (completediscount). The notes arrive and show under Notes.", go: { view: "credit", seg: "notes", label: "Credit › Notes" } },
      { who: "You", text: "Redeem the notes for sECX when you want to, or pass them on." },
    ],
    gaps: ["Sell a bill to a house from the app."],
  },
  {
    id: "borrow",
    goal: "Borrow from a house",
    title: "Borrow from a house",
    summary:
      "Write a bill payable by you, with a bond, and sell it to a discount house: the house pays you now in its notes, less a discount, and you pay the bill at the due date. You can repay in the house's own notes.",
    lends: "The house.",
    risk: `You owe the face amount at the due date. If it isn't paid by the end of the grace period, the house takes your bond. While you hold the house's notes, you carry the house's risk. ${HOUSE_RISK}`,
    cost: "The discount and network fees. Your bond is locked until you pay.",
    inApp: "partly",
    inAppNote:
      "Writing the bill and repaying it (in sECX or in the house's notes) are in the app. Selling the bill to the house is from the command line today (FreeBank guide, section 4.3).",
    calc: true,
    steps: [
      { who: "You", text: "Look at the houses and agree terms with one: the price it pays, the face amount and the due date.", go: { view: "credit", seg: "houses", label: "Credit › Houses" } },
      { who: "You", text: "Write the bill: the face amount you will repay, the bond, the days to the due date as blocks.", go: { view: "credit", seg: "bills", label: "Bills › House tools" } },
      { who: "You and the house", text: "From the command line: proposediscount, the house signs (signdiscount), then completediscount. The house's notes arrive." },
      { who: "You", text: "Spend the notes, or redeem them for sECX.", go: { view: "credit", seg: "notes", label: "Credit › Notes" } },
      { who: "You", text: "At the due date, choose Repay on the bill, in the house's notes or in sECX. Your bond comes back.", go: { view: "credit", seg: "bills", label: "Credit › Bills" } },
    ],
    gaps: ["Sell a bill to a house from the app.", "A standing credit line: a limit agreed with a house that you draw on and repay."],
  },
  {
    id: "notes",
    goal: "Hold or pass on a house's notes",
    title: "Hold or pass on a house's notes",
    summary:
      "A house's notes are its promise to pay sECX at par on demand while it is open. You can hold them, send them to another FreeBank address (one starting with X), or redeem them at the house.",
    lends: "You lend to the house for as long as you hold its notes.",
    risk: `You carry the house's risk. ${HOUSE_RISK} Look at a house's status and attested reserves before taking its notes.`,
    cost: "Network fees. A redemption may carry the house's spread.",
    inApp: "yes",
    inAppNote: "Holding, sending, redeeming and demanding notes are all in the app.",
    steps: [
      { text: "Look at a house's status (open, stressed, suspended) and its attested reserves.", go: { view: "credit", seg: "houses", label: "Credit › Houses" } },
      { text: "Notes reach you when someone sends them to your address (Receive gives one starting with X), or when you buy them in a pool.", go: { view: "credit", seg: "pools", label: "Credit › Pools" } },
      { text: "Under Notes: Send, Redeem for sECX, or Demand when a house has suspended.", go: { view: "credit", seg: "notes", label: "Credit › Notes" } },
    ],
    gaps: [],
  },
  {
    id: "pools",
    goal: "Swap a house's notes and sECX in a pool",
    title: "Swap a house's notes and sECX in a pool",
    summary:
      "A house's pool holds its notes and sECX. You can swap one for the other at the pool's price, which moves with each trade, or add both to the pool and earn a share of its fees.",
    lends: "Nobody lends in a swap. Adding to a pool puts your notes and sECX in it until you take them out.",
    risk: `The pool's price moves with each trade, and a share of a pool changes with them. Notes you hold carry the house's risk. ${HOUSE_RISK} A pool keeps trading while its house is stressed: selling at a discount is how holders can leave.`,
    cost: "The pool's fee on each swap (the house sets it, 0.3% unless set otherwise) and network fees.",
    inApp: "yes",
    inAppNote: "Swapping, adding to and taking out of a pool are in the app. A house creates its pool under House tools.",
    steps: [
      { text: "Pick a house's pool and look at its price.", go: { view: "credit", seg: "pools", label: "Credit › Pools" } },
      { text: "Swap notes for sECX or sECX for notes, or add both to the pool." },
      { text: "Your pool shares show in the pool. Take them out whenever you want." },
    ],
    gaps: [],
  },
  {
    id: "term-deposit",
    goal: "Lend to a house for a term",
    title: "Lend to a house for a term",
    summary:
      "Lend sECX to a house for a set term at the rate it sets. At maturity it pays back the principal and the interest. The receipt can be passed to someone else before then.",
    lends: "You lend to the house until maturity.",
    risk: "You carry the house's risk until maturity, and a deposit can't be taken out early (selling the receipt is the way out). A house can suspend or fail. If it fails, note-holders rank first; depositors come after them, capped at the principal, and what is left may not cover them in full.",
    cost: "Network fees. The interest is what the house offers.",
    inApp: "not yet",
    inAppNote:
      "Term deposits are from the command line today (FreeBank guide, section 4.6), run with the house: originatedeposit, transferdeposit, and at maturity a payout through the house.",
    steps: [
      { text: "Look at the houses and ask one for its rate and terms.", go: { view: "credit", seg: "houses", label: "Credit › Houses" } },
      { text: "The house opens the deposit for the principal, rate and maturity agreed, and the receipt is passed to you." },
      { text: "At maturity, the deposit is paid out through the house: principal plus interest." },
    ],
    gaps: ["Term deposits in the app: see a house's rate, open a deposit, and be paid at maturity."],
  },
  {
    id: "run-a-house",
    goal: "Run a house",
    title: "Run a house",
    summary:
      "Charter a house: pledge escrow, attest your reserves on schedule, mint notes against them, run a pool, take bills and deposits, and settle with other houses at par.",
    lends: "Your house does, to the holders of the bills it buys, with the notes it issues.",
    risk: "You carry the risk of what you lend. Your escrow and posted reserves stand behind your notes: miss an attestation and your house turns stressed, and if it fails, note-holders rank first in what the chain can seize. Issuing wrapped tokens needs custody and issuer licences where you serve people. A house that redeems its notes for sECX may need to register or be licensed where it operates, as a crypto-asset service.",
    cost: "Network fees, your escrow locked while the house runs, and what you lose on bills that aren't paid.",
    inApp: "partly",
    inAppNote:
      "Chartering, attesting, minting notes, a pool, your mint's tokens and settling with other houses are in the app. Buying bills and term deposits are from the command line today.",
    steps: [
      { text: "Charter a house and attest its reserves.", go: { view: "credit", seg: "houses", label: "Houses › House tools" } },
      { text: "Mint notes against your reserves.", go: { view: "credit", seg: "notes", label: "Notes › House tools" } },
      { text: "Create your house's pool, so holders can swap your notes.", go: { view: "credit", seg: "pools", label: "Pools › House tools" } },
      { text: "Attest again within every 144 blocks (about a day) to stay open. The app reminds you when one is due." },
    ],
    gaps: ["Buy bills (discounting) from the app.", "Term deposits from the app."],
  },
  {
    id: "move-coins",
    goal: "Move coins between eCash and FreeBank",
    title: "Move coins between eCash and FreeBank",
    summary:
      "A deposit turns ECX in your eCash wallet into the same amount of sECX on FreeBank. A withdrawal turns sECX back into ECX, paid on eCash if and when eCash's miners approve it: on easynet that takes hours, on eCash itself weeks.",
    lends: "Nobody: sECX and ECX move one-for-one through the drivechain peg.",
    risk: "A withdrawal waits for eCash's miners to approve it, and they can refuse. Until it is paid it shows as pending.",
    cost: "Network fees on both chains.",
    inApp: "yes",
    inAppNote: "Deposit and Withdraw are on Home.",
    steps: [
      { text: "Deposit: from your eCash wallet into FreeBank.", go: { view: "home", home: "deposit", label: "Home › Deposit" } },
      { text: "Withdraw: back to your eCash wallet or another eCash address.", go: { view: "home", home: "withdraw", label: "Home › Withdraw" } },
    ],
    gaps: [],
  },
];

export function wayById(id: string): Way | undefined {
  return WAYS.find((w) => w.id === id);
}

/** Days to FreeBank blocks (about 144 a day), rounded up; 0 for anything not a positive number. */
export function blocksFor(days: number): number {
  return Number.isFinite(days) && days > 0 ? Math.ceil(days * BLOCKS_PER_DAY) : 0;
}

/** A price in another currency, at a rate (that currency per sECX), in sECX to 8 places; 0 when either is missing. */
export function inBase(price: number, ratePerCoin: number): number {
  if (!(Number.isFinite(price) && price > 0 && Number.isFinite(ratePerCoin) && ratePerCoin > 0)) return 0;
  return Math.round((price / ratePerCoin) * 1e8) / 1e8;
}

/** A bond as a share (percent) of the face amount, in sECX to 8 places. */
export function bondFor(face: number, percent: number): number {
  if (!(Number.isFinite(face) && face > 0 && Number.isFinite(percent) && percent > 0)) return 0;
  return Math.round(face * Math.min(percent, 100) * 1e6) / 1e8;
}

/** The text a gap's "Send as a suggestion" starts with. */
export function suggestion(way: Way, gap: string): string {
  return `Ways to use FreeBank › ${way.title}: FreeBank can't do this yet.\n\n${gap}\n\nWhat I wanted to do: `;
}

/** The guide: null when closed, "" for the goal menu, a way's id for its guide. */
export const waysOpen = writable<string | null>(null);

/** A screen the guide asked for; App takes the view, CreditPanel the segment, and either clears it. */
export const goTo = writable<Target | null>(null);
