<script lang="ts">
  // Credit (v0.2.6, the UX walk-through's structure): Notes | Houses | Pools | Bills, one tab with four segments, what a
  // holder needs first and the issuer's forms folded under "House tools". Moved here from App.svelte as it was, then
  // reorganised.
  import { onMount } from "svelte";
  import {
    api,
    houseType,
    type Bill,
    type House,
    type HouseMember,
    type HouseType,
    type LpHolding,
    type NoteHolding,
    type NettingAnswer,
    type Pool,
  } from "../lib/api";
  import { nettingStep, roundHexFrom, type NettingRound } from "../lib/netting";
  import { BASE_TICKER } from "../lib/brand";
  import { ECX_PROBLEM, ecxInput, fmtEcx, parseEcx } from "../lib/amount";
  import { nice } from "../lib/errors";
  import { walletLocked, withUnlock } from "../lib/wallet";
  import { showReceipt } from "../lib/receipts";
  import Notice from "./Notice.svelte";
  import { lockHexFrom, parseKeysetFile, saveFloat, savedFloat, type MintKeyset } from "../lib/tokenhouse";

  /** The wallet's spendable ECX, sats (for "you need coins first"). */
  export let balance = 0;
  /** The node's block height (bills' maturity in days). */
  export let height = 0;
  /** The node is still catching up: "no houses yet" would mislead (v0.2.6, the walk-through). */
  export let syncing = false;

  // FreeBank blocks follow eCash's, about 144 a day.
  function days(blocks: number): string {
    const d = blocks / 144;
    return d < 1 ? `${Math.max(1, Math.round(d * 24))} hours` : `${d < 10 ? d.toFixed(1).replace(/\.0$/, "") : Math.round(d)} days`;
  }
  // A house by name, as the Houses segment lists it.
  function nameOf(list: House[], id: number): string {
    const h = list.find((x) => x.id === id);
    return h ? `${h.classid} · #${id}` : `House #${id}`;
  }

  // Older nodes (before v0.2.19) list notes, pool shares and bills only with the wallet unlocked: one unlock reads
  // all three (the walk-through found a card and an unlock per segment).
  async function unlockAll() {
    const r = await unlockToRead(
      async () => [await api.listMyNotes(), await api.listMyLp(), await api.listMyBills()] as const,
      "see your notes, pool shares and bills",
    );
    if (r) {
      [notes, myLp, bills] = r;
      locked = { notes: false, lp: false, bills: false };
    }
  }

  type Seg = "notes" | "houses" | "pools" | "bills";
  const SEGS: [Seg, string][] = [["notes", "Notes"], ["houses", "Houses"], ["pools", "Pools"], ["bills", "Bills"]];
  let seg: Seg = "notes";
  let error = "";
  const NEED_COINS = "You need FreeBank coins first: Home › Deposit brings them in from eCash.";

  function open(s: Seg) {
    seg = s;
    error = "";
    if (s === "notes") { loadNotes(); loadHouses(); }
    else if (s === "houses") loadHouses();
    else if (s === "pools") { loadPools(); loadHouses(); }
    else loadBills();
  }
  onMount(() => open("notes"));

  // Notes (M1)
  let notes: NoteHolding[] = [];
  let notesLoading = false;
  let mintHouseId = "";
  let mintUnits = "";
  let mintTo = "";
  let action: { type: "send" | "redeem" | "demand"; houseId: number } | null = null;
  let actionUnits = "";
  let actionAddress = "";
  let actionBusy = false;
  const STATUS_LABEL: Record<string, string> = {
    o: "Open", s: "Stressed", d: "Suspended", i: "Insolvent", w: "Wound down",
  };

  // A house's type (node v0.2.19): open, members only or redeem only. It takes the list, so it re-renders when the
  // houses arrive after the notes.
  function typeOf(list: House[], id: number | string): HouseType {
    return houseType(list.find((h) => h.id === Number(id)));
  }
  const TYPE_LABEL: Record<HouseType, string> = { open: "", members: "Members only", redeem: "Redeem only" };

  // An address in a receipt's headline; the full one goes in its "To" row.
  function short(a: string): string {
    return a.length > 20 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a;
  }

  // Everything is shown and entered in ECX (D-2026-09-29-5 and -7: no grams until gold is switched
  // on). Note units are base-native, 1 unit = 1 sat of ECX, so a note amount is an ECX amount, and
  // this shows "= 50,000,000 units" under an ECX field once it holds a valid amount.
  function unitsEcho(v: string | number): string {
    const u = parseEcx(v);
    return u === null ? "" : `= ${u.toLocaleString()} units`;
  }

  // Lists that need the wallet unlocked (listmynotes, listmylp, listmybills): with it locked, the screen says so and
  // offers Unlock (the usual passphrase prompt) instead of the node's "Please enter the wallet passphrase" (found on
  // Xvfb, 2026-10-01).
  let locked = { notes: false, lp: false, bills: false };
  async function unlockToRead<T>(read: () => Promise<T>, what: string): Promise<T | null> {
    try {
      return await withUnlock(read, { what, upfront: true });
    } catch (e) {
      error = nice(e);
      return null;
    }
  }

  async function loadNotes() {
    notesLoading = true;
    error = "";
    try {
      notes = await api.listMyNotes();
      locked.notes = false;
    } catch (e) {
      if (walletLocked(e)) locked.notes = true;
      else error = nice(e);
    }
    notesLoading = false;
  }

  async function unlockNotes() {
    const n = await unlockToRead(() => api.listMyNotes(), "see your notes");
    if (n) {
      notes = n;
      locked.notes = false;
    }
  }

  async function doMint() {
    if (!mintHouseId || !mintUnits) return;
    const units = parseEcx(mintUnits);
    if (units === null) { error = ECX_PROBLEM; return; }
    const houseId = parseInt(mintHouseId);
    const amount = `${fmtEcx(units)} ${BASE_TICKER}`;
    const to = mintTo.trim();
    // A redeem-only house reaches a member only by minting to them (node v0.2.19).
    if (!to && typeOf(houses, houseId) === "redeem") { error = "A redeem-only house mints only to a member: enter their address."; return; }
    actionBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.mintNote(houseId, units, to), { what: `mint ${amount} of notes` });
      showReceipt({
        txid,
        what: to ? `Minted ${amount} of House #${houseId} notes to ${short(to)}` : `Minted ${amount} of notes from House #${houseId}`,
        rows: to ? [{ label: "To", value: to, mono: true }] : undefined,
      });
      mintUnits = "";
      mintTo = "";
      await loadNotes();
    } catch (e) {
      error = nice(e);
    }
    actionBusy = false;
  }

  function startAction(type: "send" | "redeem" | "demand", houseId: number) {
    action = { type, houseId };
    actionAddress = "";
    // A redeem or a demand takes one holder's coins summing exactly to the amount, so start from what can go:
    // everything redeemable now (while suspended, only the demanded notes), or everything not yet demanded.
    const n = notes.find((x) => x.house_id === houseId);
    const units = !n ? 0 : type === "redeem" ? n.redeemable_units ?? n.units : type === "demand" ? n.units - n.demanded_units : 0;
    actionUnits = units > 0 ? ecxInput(units) : "";
  }

  // The yearly rate on demands queued at a suspended house (node v0.2.18, defer_interest_bps: 1000 = 10%). It takes
  // the list, so the line re-renders when the houses arrive after the notes.
  function queueRate(list: House[], houseId: number): string {
    const bps = list.find((h) => h.id === houseId)?.defer_interest_bps;
    return typeof bps === "number" && bps > 0 ? `${bps / 100}% a year` : "interest";
  }

  async function submitAction() {
    if (!action || !actionUnits) return;
    const units = parseEcx(actionUnits);
    if (units === null) { error = ECX_PROBLEM; return; }
    const { type, houseId } = action;
    const amount = `${fmtEcx(units)} ${BASE_TICKER}`;
    actionBusy = true;
    error = "";
    try {
      if (type === "send") {
        // No address: to one of this wallet's own keys (gathers notes spread over several addresses into one; for a
        // members-only house the node keeps them on a key that is a member).
        const to = typeOf(houses, houseId) === "redeem" ? "" : actionAddress.trim();
        const txid = await withUnlock(() => api.transferNote(houseId, units, to), { what: `send ${amount} of notes` });
        showReceipt(
          to
            ? { txid, what: `Sent ${amount} of House #${houseId} notes to ${short(to)}`, rows: [{ label: "To", value: to, mono: true }] }
            : { txid, what: `Moved ${amount} of House #${houseId} notes to one of your own addresses` },
        );
      } else if (type === "redeem") {
        const txid = await withUnlock(() => api.redeemNote(houseId, units), { what: `redeem ${amount} of notes` });
        showReceipt({ txid, what: `Redeemed ${amount} of House #${houseId} notes` });
      } else {
        const txid = await withUnlock(() => api.demandNote(houseId, units), { what: `lodge a demand on ${amount} of notes` });
        showReceipt({ txid, what: `Lodged a demand on ${amount} of House #${houseId} notes` });
      }
      action = null;
      await loadNotes();
    } catch (e) {
      error = nice(e);
      // A redeem or a demand takes one holder's coins summing exactly to the amount (the node: "... sum exactly ...").
      if (type !== "send" && /sum exactly/.test(error))
        error += " Your notes of this house may sit at more than one of your addresses: gather them first (Send, with the address left empty), then try again.";
    }
    actionBusy = false;
  }

  // Houses (M2)
  let houses: House[] = [];
  let housesLoading = false;
  let regName = "";
  let regTier = "0";
  let regEscrow = "";
  let regType: HouseType = "open";
  let regBusy = false;
  let attestId = "";
  // A members-only or redeem-only house's member list, open under its row (node v0.2.19).
  let membersOf: number | null = null;
  /** Add and Remove show only once asked for: only the house's own node can change members (the walk-through). */
  let editMembers = false;
  let members: HouseMember[] = [];
  let membersLoading = false;
  let memberAdd = "";

  function pct(bps: number): string {
    return (bps / 100).toFixed(1) + "%";
  }
  function util(h: House): number {
    return h.mintcapunits > 0 ? Math.min(100, (h.mintedunits / h.mintcapunits) * 100) : 0;
  }

  async function loadHouses() {
    housesLoading = true;
    error = "";
    try {
      houses = await api.listHouses();
    } catch (e) {
      error = nice(e);
    }
    housesLoading = false;
  }

  async function doRegister() {
    if (!regName || !regEscrow) return;
    const name = regName.trim();
    const escrow = parseFloat(regEscrow);
    regBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.registerHouse(name, parseInt(regTier), escrow, 1000, 1, 0.001, regType), {
        what: "charter the house",
      });
      const kind = regType === "open" ? "house" : regType === "members" ? "members-only house" : "redeem-only house";
      showReceipt({ txid, what: `Chartered the ${kind} "${name}" with ${escrow} ${BASE_TICKER} pledged` });
      regName = "";
      regEscrow = "";
      regType = "open";
      await loadHouses();
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  async function showMembers(houseId: number) {
    if (membersOf === houseId) { membersOf = null; return; }
    membersOf = houseId;
    members = [];
    memberAdd = "";
    editMembers = false;
    membersLoading = true;
    try {
      members = await api.listHouseMembers(houseId);
    } catch (e) {
      error = nice(e);
    }
    membersLoading = false;
  }

  // Add the addresses typed (one per line, or separated by spaces or commas), or remove one. The node allows one
  // change per house per block, and only with the house's keys.
  async function changeMembers(houseId: number, add: boolean, addresses: string[]) {
    if (addresses.length === 0) return;
    regBusy = true;
    error = "";
    try {
      const n = addresses.length === 1 ? "1 member" : `${addresses.length} members`;
      const txid = await withUnlock(() => api.changeHouseMembers(houseId, add, addresses), {
        what: `${add ? "add" : "remove"} ${n}`,
      });
      showReceipt({
        txid,
        what: add ? `Added ${n} to House #${houseId}` : `Removed ${n} from House #${houseId} (in effect 3 blocks after it confirms)`,
      });
      memberAdd = "";
      members = await api.listHouseMembers(houseId);
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  async function doAttest(houseId: number) {
    regBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.attestHouse(houseId), { what: `attest House #${houseId}'s reserves` });
      showReceipt({ txid, what: `Attested House #${houseId}'s reserves` });
      await loadHouses();
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  // Run a house (v0.4.0): the house's mint. Its keyset recorded on chain before it issues, and its batch locks
  // approved with the partners' keys here (they never go to the mint's server). Both come as text from the server:
  // keyset.json, and the hex `createnotelock` prints there.
  let ksText = "";
  let ks: MintKeyset | null = null;
  let ksError = "";
  let ksOnChain: { id: string; postingpubkey: string }[] | null = null;
  // Recorded here and not yet in a block: re-pasting the file shouldn't offer (and pay for) a second record.
  let ksPending: Record<string, string> = {};
  let lockHex = "";
  let lockShown: { house: number; units: number; holder: string } | null = null;
  let lockFloat = "";
  $: floatOk = /^X[1-9A-HJ-NP-Za-km-z]{25,34}$/.test(lockFloat.trim());

  async function checkKeyset() {
    ks = null;
    ksOnChain = null;
    ksError = "";
    try {
      ks = parseKeysetFile(ksText.trim());
      if (ks.float) saveFloat(ks.house, ks.float);
      ksOnChain = (await api.tokenKeysets(ks.house)).keysets;
    } catch (e) {
      ksError = nice(e);
    }
  }

  async function recordKeyset() {
    if (!ks) return;
    const k = ks;
    regBusy = true;
    error = "";
    try {
      const r = await withUnlock(() => api.registerTokenKeyset(k.house, k.keys, k.postingpubkey), {
        what: `record House #${k.house}'s token keyset`,
      });
      if (r.keysetid !== k.keysetid) {
        error = `The chain names this keyset ${r.keysetid}, not ${k.keysetid}: the mint won't see it as its own.`;
      }
      ksPending = { ...ksPending, [k.keysetid]: r.txid };
      showReceipt({ txid: r.txid, what: `Recorded House #${k.house}'s token keyset ${k.keysetid}` });
      ksText = "";
      ks = null;
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  async function checkLock() {
    lockShown = null;
    error = "";
    try {
      lockShown = await withUnlock(() => api.checkNoteLock(lockHexFrom(lockHex)), { what: "check the lock with the house's keys", upfront: true });
      if (lockShown && !lockFloat) lockFloat = savedFloat(lockShown.house);
    } catch (e) {
      error = nice(e);
    }
  }

  async function sendLock() {
    if (!lockShown) return;
    const l = lockShown;
    regBusy = true;
    error = "";
    try {
      const float = lockFloat.trim();
      saveFloat(l.house, float);
      const txid = await withUnlock(() => api.sendNoteLock(lockHexFrom(lockHex), l.house, l.units, float), {
        what: `lock ${fmtEcx(l.units)} ${BASE_TICKER} of House #${l.house}'s notes`,
        upfront: true,
      });
      showReceipt({
        txid,
        what: `Locked ${fmtEcx(l.units)} ${BASE_TICKER} of House #${l.house}'s notes as token backing`,
        rows: [{ label: "Held at", value: l.holder, mono: true }],
      });
      lockHex = "";
      lockShown = null;
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  // Settlement between houses (v0.4.1): netting. A round arrives as text, is shown decoded, and this house does its
  // next step; the round that step returns is what goes on to the next house.
  let netOwn: number | null = null;
  let netOthers = "";
  let netText = "";
  let netRound: NettingRound | null = null;
  let netOut = "";
  let netWaiting: string[] = [];
  $: netStep = netRound && netOwn ? nettingStep(netRound, netOwn) : null;

  async function decodeNetting(text: string) {
    error = "";
    netRound = null;
    netWaiting = [];
    try {
      const hex = roundHexFrom(text);
      netRound = await api.decodeNetting(hex);
      netText = hex;
    } catch (e) {
      error = nice(e);
    }
  }

  // Notes being moved onto one key first: the step waits until those transfers are in a block. Done again before
  // that, the node would make a part from the notes already on the key and hand in less.
  async function consolidated() {
    error = "";
    try {
      const confs = await Promise.all(netWaiting.map(async (t) => (await api.getTransaction(t)).confirmations ?? 0));
      if (confs.every((c) => c >= 1)) netWaiting = [];
      else error = "Not all of them are in a block yet: try again after the next block.";
    } catch (e) {
      error = nice(e);
    }
  }

  // An answer: the round to pass on (shown decoded), or notes first moved onto one key ("consolidating").
  async function took(r: NettingAnswer) {
    if (r.status === "consolidating") {
      netWaiting = r.txids ?? [];
      return;
    }
    netWaiting = [];
    if (r.round) {
      netOut = r.round;
      await decodeNetting(r.round);
    }
    if (r.txid) {
      const net = r.net ?? 0;
      showReceipt({ txid: r.txid, what: `Netting round sent: House #${netOwn} ${net < 0 ? `pays ${fmtEcx(-net)} ${BASE_TICKER}` : net > 0 ? `receives ${fmtEcx(net)} ${BASE_TICKER}` : "settles even"}` });
      netOut = "";
    }
  }

  async function startNetting() {
    const own = netOwn;
    const others = netOthers.split(/[\s,]+/).filter(Boolean).map((x) => parseInt(x.replace(/^#/, ""), 10));
    if (!own || others.some((x) => !Number.isInteger(x) || x <= 0) || others.includes(own)) {
      error = "List the other houses' numbers, without your own: e.g. 2, 3.";
      return;
    }
    regBusy = true;
    error = "";
    try {
      await took(await withUnlock(() => api.createNetting(own, others), { what: `start a netting round for House #${own}` }));
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  async function nettingAct(step: "join" | "fund" | "sign") {
    const own = netOwn;
    if (!own || !netRound) return;
    const round = netText;
    regBusy = true;
    error = "";
    try {
      const r = await withUnlock(
        () => (step === "join" ? api.joinNetting(own, round) : step === "fund" ? api.fundNetting(own, round) : api.signNetting(own, round)),
        { what: `${step} the netting round for House #${own}` },
      );
      await took(r);
    } catch (e) {
      error = nice(e);
    }
    regBusy = false;
  }

  // Pools (M3): note ⇄ ECX AMM
  let pools: Pool[] = [];
  let myLp: LpHolding[] = [];
  let poolsLoading = false;
  let poolAction: { type: "swap" | "add" | "remove"; poolId: number } | null = null;
  let swapDir: "noteforbtx" | "btxfornote" = "noteforbtx";
  let poolAmountIn = "";
  let poolMinOut = "";
  let poolAddNoteEcx = "";
  let poolAddEcx = "";
  let poolRemoveLp = "";
  let poolBusy = false;
  // Create pool
  let createPoolId = "";
  let createNoteEcx = "";
  let createEcx = "";
  let createFeeBps = "30";

  function price(p: Pool): string {
    // The node leaves the spot price out while a pool side is empty. Both sides count in sats
    // (1 note unit = 1 sat), so sats per unit is ECX paid per ECX of notes.
    if (p.spot_price_sats_x1e8 == null) return "no price yet";
    return `${(p.spot_price_sats_x1e8 / 1e8).toFixed(6)} ${BASE_TICKER} per ${BASE_TICKER} of notes`;
  }

  // An empty or zero "least out" is no limit; anything else must be a valid amount (null if not).
  function minOut(v: string | number | null): number | null {
    return v === "" || v === null || Number(v) === 0 ? 0 : parseEcx(v);
  }

  async function loadPools() {
    poolsLoading = true;
    error = "";
    try {
      pools = await api.listPools();
      try {
        myLp = await api.listMyLp();
        locked.lp = false;
      } catch (e) {
        if (!walletLocked(e)) throw e;
        locked.lp = true;
      }
    } catch (e) {
      error = nice(e);
    }
    poolsLoading = false;
  }

  async function unlockLp() {
    const lp = await unlockToRead(() => api.listMyLp(), "see your liquidity");
    if (lp) {
      myLp = lp;
      locked.lp = false;
    }
  }

  function startPoolAction(type: "swap" | "add" | "remove", poolId: number) {
    poolAction = { type, poolId };
    swapDir = "noteforbtx";
    poolAmountIn = "";
    poolMinOut = "";
    poolAddNoteEcx = "";
    poolAddEcx = "";
    poolRemoveLp = "";
  }

  async function submitPoolAction() {
    if (!poolAction) return;
    const { type, poolId } = poolAction;
    poolBusy = true;
    error = "";
    try {
      if (type === "swap") {
        const dir = swapDir;
        const amountIn = parseEcx(poolAmountIn);
        const least = minOut(poolMinOut);
        if (amountIn === null || least === null) throw new Error(ECX_PROBLEM);
        const txid = await withUnlock(() => api.swapNote(poolId, dir, amountIn, least), { what: "swap in the pool" });
        const given = `${fmtEcx(amountIn)} ${BASE_TICKER}`;
        showReceipt({
          txid,
          what: dir === "noteforbtx"
            ? `Swapped ${given} of notes for ${BASE_TICKER} in Pool #${poolId}`
            : `Swapped ${given} for notes in Pool #${poolId}`,
        });
      } else if (type === "add") {
        const notes = parseEcx(poolAddNoteEcx);
        const ecx = parseEcx(poolAddEcx);
        if (notes === null || ecx === null) throw new Error(ECX_PROBLEM);
        const txid = await withUnlock(() => api.addLiquidity(poolId, notes, ecx), { what: "add liquidity" });
        showReceipt({
          txid,
          what: `Added ${fmtEcx(notes)} ${BASE_TICKER} of notes and ${fmtEcx(ecx)} ${BASE_TICKER} to Pool #${poolId}`,
        });
      } else {
        const lp = Number(poolRemoveLp);
        if (!Number.isInteger(lp) || lp < 1) throw new Error("Enter how many LP units to burn.");
        const txid = await withUnlock(() => api.removeLiquidity(poolId, lp), { what: "remove liquidity" });
        showReceipt({ txid, what: `Removed ${lp.toLocaleString()} LP units from Pool #${poolId}` });
      }
      poolAction = null;
      await loadPools();
    } catch (e) {
      error = nice(e);
    }
    poolBusy = false;
  }

  async function doCreatePool() {
    if (!createPoolId || !createNoteEcx || !createEcx) return;
    const notes = parseEcx(createNoteEcx);
    const ecx = parseEcx(createEcx);
    if (notes === null || ecx === null) { error = ECX_PROBLEM; return; }
    const poolId = parseInt(createPoolId);
    poolBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.createPool(poolId, notes, ecx, parseInt(createFeeBps)), {
        what: "create the pool",
      });
      showReceipt({
        txid,
        what: `Created Pool #${poolId} with ${fmtEcx(notes)} ${BASE_TICKER} of notes and ${fmtEcx(ecx)} ${BASE_TICKER}`,
      });
      createNoteEcx = "";
      createEcx = "";
      await loadPools();
    } catch (e) {
      error = nice(e);
    }
    poolBusy = false;
  }

  // Bills (M4): bills of exchange — the discount-house asset side
  let bills: Bill[] = [];
  let billsLoading = false;
  let billBusy = false;
  let billAction: { type: "endorse"; id: number } | null = null;
  let endorsePubkey = "";
  let newBillPubkey = "";
  let billBody = "";
  let billAmount = "";
  let billEscrow = "";
  let billMatureIn = "1000";
  let billGrace = "1008";
  const BILL_STATUS: Record<string, string> = {
    a: "Active", r: "Retired", d: "Defaulted", x: "Disputed",
  };
  function toHex(s: string): string {
    let h = "";
    for (let i = 0; i < s.length; i++) h += s.charCodeAt(i).toString(16).padStart(2, "0");
    return h;
  }

  async function loadBills() {
    billsLoading = true;
    error = "";
    try {
      bills = await api.listMyBills();
      locked.bills = false;
    } catch (e) {
      if (walletLocked(e)) locked.bills = true;
      else error = nice(e);
    }
    billsLoading = false;
  }

  async function unlockBills() {
    const b = await unlockToRead(() => api.listMyBills(), "see your bills");
    if (b) {
      bills = b;
      locked.bills = false;
    }
  }

  async function doIssueBill() {
    if (!billAmount || !billEscrow) return;
    billBusy = true;
    error = "";
    try {
      // The tip now, not a figure from the last refresh: maturity counts from it.
      const now = (await api.getBlockchainInfo()).blocks;
      const maturity = now + parseInt(billMatureIn || "1000");
      const bodyHex = toHex(billBody || "bill");
      const amount = parseFloat(billAmount);
      const escrow = parseFloat(billEscrow);
      const txid = await withUnlock(
        () => api.issueBill(bodyHex, amount, escrow, maturity, parseInt(billGrace || "1008")),
        { what: "issue the bill" },
      );
      showReceipt({
        txid,
        what: `Issued a bill for ${amount} ${BASE_TICKER}, bonded with ${escrow} ${BASE_TICKER}`,
        rows: [{ label: "Matures at", value: `block ${maturity.toLocaleString()}` }],
      });
      billBody = "";
      billAmount = "";
      billEscrow = "";
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function getBillPubkey() {
    error = "";
    try {
      newBillPubkey = await withUnlock(() => api.getNewBillPubkey(), { what: "make a new bill pubkey" });
    } catch (e) {
      error = nice(e);
    }
  }

  async function doEndorseBill() {
    if (!billAction || !endorsePubkey) return;
    billBusy = true;
    error = "";
    try {
      const id = billAction.id;
      const to = endorsePubkey.trim();
      const txid = await withUnlock(() => api.endorseBill(id, to), { what: `endorse Bill #${id}` });
      showReceipt({ txid, what: `Endorsed Bill #${id}` });
      billAction = null;
      endorsePubkey = "";
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function doRetireBill(id: number) {
    billBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.retireBill(id), { what: `retire Bill #${id}` });
      showReceipt({ txid, what: `Retired Bill #${id}` });
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

  async function doClaimBillEscrow(id: number) {
    billBusy = true;
    error = "";
    try {
      const txid = await withUnlock(() => api.claimBillEscrow(id), { what: `claim Bill #${id}'s escrow` });
      showReceipt({ txid, what: `Claimed Bill #${id}'s escrow` });
      await loadBills();
    } catch (e) {
      error = nice(e);
    }
    billBusy = false;
  }

</script>

<nav class="segments" aria-label="Credit">
  {#each SEGS as [s, label]}
    <button class:active={seg === s} on:click={() => open(s)}>{label}</button>
  {/each}
</nav>

{#if error}
  <Notice kind="error" message={error} on:dismiss={() => (error = "")} />
{/if}

{#if seg === "notes"}
      <!-- Notes (M1): per-house credit notes — hold / send / redeem / demand -->
      {#if locked.notes}
        <div class="card locked-read">
          <p>Your wallet is locked: this node lists notes, pool shares and bills only while it's unlocked.</p>
          <button on:click={unlockAll}>Unlock</button>
        </div>
      {/if}
      <div class="card">
        <div class="notes-head">
          <h2>My notes</h2>
          <button class="link-btn" on:click={loadNotes} disabled={notesLoading}>
            {notesLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if locked.notes}
          <p class="muted small">—</p>
        {:else if notes.length === 0}
          <div class="empty">
            <p>You don't hold any notes yet.</p>
            {#if !housesLoading && houses.length === 0}
              <p class="muted small">{syncing ? "Your node is still catching up; houses and their notes show once it has." : "Notes are issued by houses, and there are no houses on this network yet."}</p>
            {:else}
              <p class="muted small">Anyone can send you a note at one of your addresses (see Receive), or you can buy one in a pool.</p>
            {/if}
            {#if balance === 0}<p class="muted small">{NEED_COINS}</p>{/if}
          </div>
        {:else}
          {#each notes as n}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">{nameOf(houses, n.house_id)}</span>
                  <span class="badge badge-{n.house_status}">{STATUS_LABEL[n.house_status] ?? n.house_status}</span>
                  {#if typeOf(houses, n.house_id) !== "open"}<span class="badge badge-members">{TYPE_LABEL[typeOf(houses, n.house_id)]}</span>{/if}
                </div>
                <div class="note-units">{fmtEcx(n.units)} {BASE_TICKER}</div>
              </div>
              <div class="hint">{n.units.toLocaleString()} units</div>
              {#if n.demanded_units > 0}
                <div class="note-demanded">
                  {#if n.house_status === "d"}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} in the house's payout queue, earning {queueRate(houses, n.house_id)}
                  {:else if n.house_status === "o" || n.house_status === "s"}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} demanded: the house must pay it within the demand window
                  {:else}
                    {fmtEcx(n.demanded_units)} {BASE_TICKER} demanded; the house has failed, so holders are paid from what it has left
                  {/if}
                </div>
              {/if}
              <div class="note-actions">
                <!-- A redeem-only house's notes pass only back to the house or to the holder's own key: Gather moves them
                     onto one of this wallet's addresses (a redeem takes coins summing exactly to the amount). -->
                <button on:click={() => startAction("send", n.house_id)}>{typeOf(houses, n.house_id) === "redeem" ? "Gather" : "Send"}</button>
                <button on:click={() => startAction("redeem", n.house_id)} disabled={!n.redeemable}>Redeem</button>
                <button on:click={() => startAction("demand", n.house_id)} disabled={!n.demandable}>Demand</button>
              </div>
              {#if action && action.houseId === n.house_id}
                <div class="note-form">
                  <label>
                    Amount ({BASE_TICKER})
                    <input type="number" bind:value={actionUnits} placeholder="0.00000000" step="0.00000001" min="0" />
                  </label>
                  {#if unitsEcho(actionUnits)}
                    <p class="hint">{unitsEcho(actionUnits)}</p>
                  {/if}
                  {#if action.type === "send" && typeOf(houses, n.house_id) === "redeem"}
                    <p class="hint">This house's notes go only back to the house. Gather moves them onto one of your own addresses, so a redeem can take them together.</p>
                  {:else if action.type === "send"}
                    <label>
                      To address
                      <input type="text" bind:value={actionAddress} placeholder="X… (empty: to one of your own addresses)" />
                    </label>
                    {#if typeOf(houses, n.house_id) === "members"}
                      <p class="hint">Members only: this house's notes can go only to its members.</p>
                    {/if}
                  {/if}
                  {#if action.type === "redeem"}
                    {#if n.house_status === "d"}
                      <p class="hint">The house is suspended: only notes under demand can be redeemed now, with their interest.</p>
                    {/if}
                    <p class="hint">Redemption is paid from the house's reserves — this succeeds when your node controls the house.</p>
                  {/if}
                  {#if action.type === "demand"}
                    {#if n.house_status === "d"}
                      <p class="hint">Your notes join the house's payout queue and earn {queueRate(houses, n.house_id)} from today. The house can pay you at any time; until it does, these notes can't be sent.</p>
                    {:else}
                      <p class="hint">A formal demand: the house must pay you in full within the demand window. Until it pays, these notes can't be sent.</p>
                    {/if}
                  {/if}
                  <div class="note-form-actions">
                    <button on:click={submitAction} disabled={actionBusy || !actionUnits}>
                      {actionBusy ? "…" : action.type === "send" ? (typeOf(houses, n.house_id) === "redeem" ? "Gather" : "Send note") : action.type === "redeem" ? "Redeem" : "Lodge demand"}
                    </button>
                    <button class="secondary" on:click={() => (action = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      {#if houses.length > 0}
      <details class="advanced tools">
        <summary>House tools: mint notes</summary>
      <div class="card">
        <h3>Mint notes</h3>
        <p class="muted">Issue new notes from a house your node controls. Enter the amount in {BASE_TICKER}: notes are base-native, 1 unit = 1 sat.</p>
        <div class="form">
          <label>
            House ID
            <input type="number" bind:value={mintHouseId} placeholder="e.g. 1" />
          </label>
          <label>
            Amount ({BASE_TICKER})
            <input type="number" bind:value={mintUnits} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          {#if unitsEcho(mintUnits)}
            <p class="hint">{unitsEcho(mintUnits)}</p>
          {/if}
          {#if mintHouseId && typeOf(houses, mintHouseId) !== "open"}
            <label>
              To a member (FreeBank address)
              <input type="text" bind:value={mintTo} placeholder={typeOf(houses, mintHouseId) === "redeem" ? "X… (required)" : "X… (empty: the house's own key)"} />
            </label>
          {/if}
          <button on:click={doMint} disabled={actionBusy || !mintHouseId || !mintUnits}>
            {actionBusy ? "…" : "Mint"}
          </button>
        </div>
      </div>
      </details>
      {/if}
    {:else if seg === "houses"}
      <!-- Houses (M2): the directory of competing note-issuers -->
      <div class="card">
        <div class="notes-head">
          <h2>Houses</h2>
          <button class="link-btn" on:click={loadHouses} disabled={housesLoading}>
            {housesLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">Every note-issuing house on the chain. A note is only as sound as the house behind it — check its status and reserves before you trust its notes.</p>
        {#if houses.length === 0}
          <div class="empty">
            <p>{syncing ? "Your node is still catching up; houses show once it has." : "No houses on this network yet."}</p>
            <p class="muted small">
              {balance > 0 ? "You can charter the first one below." : `Chartering one takes a reserve in ECX. ${NEED_COINS}`}
            </p>
          </div>
        {:else}
          {#each houses as h}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">#{h.id} · {h.classid}</span>
                  <span class="badge badge-{h.effective_status.charAt(0)}">{h.effective_status}</span>
                  {#if houseType(h) !== "open"}<span class="badge badge-members">{TYPE_LABEL[houseType(h)]}</span>{/if}
                </div>
                <div class="note-units" title="Tier {h.tier}">notes up to {(h.lambdax10 / 10).toFixed(1).replace(/\.0$/, "")}× its reserve</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Reserve pledged</span> {h.activeescrow} {BASE_TICKER}</div>
                <div><span class="stat-label">Notes outstanding</span> {fmtEcx(h.mintedunits)} of {fmtEcx(h.mintcapunits)} {BASE_TICKER} cap</div>
                {#if h.mintedunits > 0}
                  <div><span class="stat-label">Reserves against notes</span> {pct(h.attestedratiobps)}</div>
                {/if}
                <div><span class="stat-label">Last attested</span> {h.lastattestheight > 0 ? `block ${h.lastattestheight} · ${h.lastattestreserves} ECX` : "never"}</div>
              </div>
              {#if h.mintcapunits > 0}
                <div class="util-bar"><div class="util-fill" style="width:{util(h)}%"></div></div>
              {/if}
              {#if houseType(h) !== "open"}
              <div class="note-actions">
                  <button on:click={() => showMembers(h.id)}>
                    {membersOf === h.id ? "Hide members" : `Members${typeof h.member_records === "number" ? ` (${h.member_records})` : ""}`}
                  </button>
              </div>
              {/if}
              {#if membersOf === h.id}
                <div class="note-form">
                  <p class="hint">
                    {houseType(h) === "redeem"
                      ? "Only members can hold this house's notes, and notes pass only between the house and the holder."
                      : "Only members can hold this house's notes."}
                    The list is public. A holder removed keeps their notes and can still redeem or demand them.
                  </p>
                  {#if membersLoading}
                    <p class="muted small">Loading…</p>
                  {:else if members.length === 0}
                    <p class="muted small">No members yet.</p>
                  {:else}
                    {#each members as m (m.address)}
                      <div class="member-row">
                        <span class="mono small">{m.address}</span>
                        <span class="muted small">
                          {m.removal_height > 0 ? (m.active ? `leaving at block ${m.removal_height}` : `removed at block ${m.removal_height}`) : `since block ${m.added_height}`}
                        </span>
                        {#if editMembers && m.removal_height === 0}
                          <button class="link-btn" on:click={() => changeMembers(h.id, false, [m.address])} disabled={regBusy}>Remove</button>
                        {/if}
                      </div>
                    {/each}
                  {/if}
                  {#if editMembers}
                  <label>
                    Add members (FreeBank addresses, one per line)
                    <textarea rows="2" bind:value={memberAdd} placeholder="X…"></textarea>
                  </label>
                  <div class="note-form-actions">
                    <button
                      on:click={() => changeMembers(h.id, true, memberAdd.split(/[\s,]+/).filter(Boolean))}
                      disabled={regBusy || !memberAdd.trim()}
                    >Add</button>
                  </div>
                  <p class="hint">Changes need this house's keys on your node, one change a block.</p>
                  {:else}
                  <button class="link-btn" on:click={() => (editMembers = true)}>Change members (your own house)…</button>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      <details class="advanced tools">
        <summary>House tools: charter a house, attest reserves, your mint's tokens, settle with other houses</summary>
      <div class="card">
        <h3>Charter a house</h3>
        <p class="muted">Open your own note-issuing house — the Scottish move: anyone can start a bank, kept honest by convertibility. Your node holds the keys.</p>
        <div class="form">
          <label>
            Name (note-class id)
            <input type="text" bind:value={regName} placeholder="e.g. clyde — a–z 0–9, ≤16 chars" />
          </label>
          <label>
            Tier (0–3): a higher tier may issue more notes for each ECX of reserve
            <input type="number" bind:value={regTier} min="0" max="3" />
          </label>
          <label>
            Pledged reserve (ECX)
            <input type="number" bind:value={regEscrow} placeholder="e.g. 1.0" step="0.00000001" />
          </label>
          <label>
            Who may hold its notes (fixed for the life of the house)
            <select bind:value={regType}>
              <option value="open">Anyone</option>
              <option value="members">Members only</option>
              <option value="redeem">Members only, and notes pass only between the house and the holder</option>
            </select>
          </label>
          <button on:click={doRegister} disabled={regBusy || !regName || !regEscrow || balance === 0}>
            {regBusy ? "…" : "Charter house"}
          </button>
          {#if balance === 0 && houses.length > 0}<p class="blocked-why">{NEED_COINS}</p>{/if}
        </div>
      </div>
      <div class="card">
        <h3>Attest reserves</h3>
        <p class="muted">A house your node holds the keys of shows its reserves on chain, so holders can see what backs its notes.</p>
        <div class="form">
          <label>
            House #
            <input type="number" bind:value={attestId} placeholder="e.g. 1" />
          </label>
          <button on:click={() => doAttest(parseInt(attestId))} disabled={regBusy || !attestId}>Attest reserves</button>
        </div>
      </div>
      <div class="card" data-testid="house-mint">
        <h3>Tokens: your house's mint</h3>
        <p class="muted">The mint issues tokens backed by your house's notes, from its own server. The house's keys stay
          here: the mint issues nothing until you record its keyset, and its batch locks need your approval.</p>
        <h4>Record the mint's keyset</h4>
        <div class="form">
          <label>
            The mint's keyset.json (in its folder on the server), pasted whole
            <textarea rows="3" bind:value={ksText} on:input={() => { ks = null; ksError = ""; }} placeholder={'{"house":…,"keysetid":"00…","keys":[…],"postingpubkey":"02…"}'}></textarea>
          </label>
          <button on:click={checkKeyset} disabled={!ksText.trim()}>Check it</button>
          {#if ksError}<p class="blocked-why">{ksError}</p>{/if}
          {#if ks}
            <div class="house-stats">
              <div><span class="stat-label">House</span> #{ks.house}</div>
              <div><span class="stat-label">Keyset</span> <span><span class="mono">{ks.keysetid}</span> · {ks.keys.length} keys, its id checked</span></div>
              <div><span class="stat-label">Posts signed by</span> <span class="mono small">{ks.postingpubkey}</span></div>
            </div>
            {#if ksPending[ks.keysetid] && !ksOnChain?.some((k) => k.id === ks?.keysetid)}
              <p class="muted small">Recorded, waiting for a block (transaction <span class="mono">{ksPending[ks.keysetid].slice(0, 16)}…</span>). Check it again after the next block.</p>
            {:else if ksOnChain?.some((k) => k.id === ks?.keysetid)}
              <p class="muted small">Already recorded{ksOnChain.find((k) => k.id === ks?.keysetid)?.postingpubkey === ks.postingpubkey ? "." : ", with another posting key: the mint needs a new keyset."}</p>
            {:else}
              <button on:click={recordKeyset} disabled={regBusy}>{regBusy ? "…" : `Record it for House #${ks.house}`}</button>
              <p class="hint">Needs this house's keys on your node, and a small fee.</p>
            {/if}
          {/if}
        </div>
        <h4>Approve a batch lock</h4>
        <p class="muted small">On the mint's server, in its folder:
          <span class="mono">F=$(cat float-address) &amp;&amp; [ -n "$F" ] &amp;&amp; freebank-cli createnotelock &lt;house&gt; &lt;units&gt; 0.001 "$F"</span>.
          Paste what it prints. The mint's wallet pays the fee.</p>
        <div class="form">
          <label>
            What createnotelock printed (or just its hex)
            <textarea rows="2" bind:value={lockHex} on:input={() => (lockShown = null)} placeholder={'{"hex": "0d000000…", …}'}></textarea>
          </label>
          <button on:click={checkLock} disabled={regBusy || !lockHex.trim()}>Check it</button>
          {#if lockShown}
            <div class="house-stats">
              <div><span class="stat-label">Locks</span> {fmtEcx(lockShown.units)} {BASE_TICKER} of House #{lockShown.house}'s notes as token backing</div>
              <div><span class="stat-label">Takes notes held at</span> <span class="mono small">{lockShown.holder}</span></div>
            </div>
            <label>
              The mint's float address (float in its keyset.json; remembered here)
              <input type="text" class="mono" bind:value={lockFloat} placeholder="X…" />
            </label>
            {#if floatOk && lockFloat.trim() !== lockShown.holder}
              <p class="blocked-why">This lock takes notes from {lockShown.holder}, not the mint's float: don't send it. A customer's
                notes may sit there, waiting for room. Make the lock again with the float's address.</p>
            {/if}
            <button on:click={sendLock} disabled={regBusy || !floatOk || lockFloat.trim() !== lockShown.holder}>{regBusy ? "…" : "Approve and send"}</button>
          {/if}
        </div>
      </div>
      <div class="card" data-testid="house-netting">
        <h3>Settle with other houses: netting</h3>
        <p class="muted">The Edinburgh exchange: each house hands in the other houses' notes it holds, they're all
          burned, and only each house's net is paid, in {BASE_TICKER} at par. A round passes from house to house as a
          block of text: everyone joins, then funds, then signs; the last signature sends it. Every house signs, so
          nothing is settled without you.</p>
        <div class="form">
          <label>
            Your house #
            <input type="number" bind:value={netOwn} placeholder="e.g. 1" />
          </label>
          <h4>Start a round</h4>
          <label>
            The other houses, by number
            <input type="text" bind:value={netOthers} placeholder="e.g. 2, 3, 4" />
          </label>
          <button on:click={startNetting} disabled={regBusy || !netOwn || !netOthers.trim()}>{regBusy ? "…" : "Start a round"}</button>
          <h4>A round you were given</h4>
          <label>
            The round, pasted whole
            <textarea rows="3" bind:value={netText} on:input={() => { netRound = null; netOut = ""; netWaiting = []; }} placeholder="the hex another house passed you"></textarea>
          </label>
          <button on:click={() => decodeNetting(netText)} disabled={regBusy || !netText.trim()}>Check it</button>
          {#if netWaiting.length}
            <p class="muted small">Your notes are first being moved onto one key ({netWaiting.length} transfer{netWaiting.length > 1 ? "s" : ""}).
              Once they're in a block, do the same step again.</p>
            <button on:click={consolidated} disabled={regBusy}>They're in a block?</button>
          {/if}
          {#if netRound}
            <div class="house-stats">
              <div><span class="stat-label">Stage</span> {netRound.stage}{netRound.stage !== "complete" ? `, expires at block ${netRound.expiryheight}` : ""}</div>
              <div><span class="stat-label">Started by</span> {nameOf(houses, netRound.starter)}{netRound.starter === netOwn ? " (yours)" : ""}, who pays its fee of {netRound.fee} {BASE_TICKER}</div>
              {#each netRound.houses as h}
                {@const part = netRound.parts.find((p) => p.house === h)}
                {@const n = netRound.nets?.find((x) => x.house === h)}
                <div>
                  <span class="stat-label">{nameOf(houses, h)}{h === netOwn ? " (yours)" : ""}</span>
                  <span>
                    {#if !part}not joined yet{:else}
                      hands in {part.bundles.length ? part.bundles.map((b) => `${fmtEcx(b.units)} of ${nameOf(houses, b.issuer)}`).join(", ") : "nothing"}
                    {/if}
                    {#if n}{` · ${n.net > 0 ? `receives ${fmtEcx(n.receives)} ${BASE_TICKER}` : n.net < 0 ? `pays ${fmtEcx(n.pays)} ${BASE_TICKER}` : "settles even"}`}{/if}{#if part?.funded}{" · funded"}{/if}{#if netRound.signed.includes(h)}{" · signed"}{/if}
                  </span>
                </div>
              {/each}
            </div>
            {#if netStep}
              {#if netRound.error}<p class="blocked-why">{netRound.error}</p>{/if}
              {#if netStep.kind === "join"}
                <button on:click={() => nettingAct("join")} disabled={regBusy || netWaiting.length > 0}>{regBusy ? "…" : `Join for House #${netOwn}`}</button>
                <p class="hint">Adds every confirmed note of the other houses your house holds.</p>
              {:else if netStep.kind === "fund"}
                <button on:click={() => nettingAct("fund")} disabled={regBusy}>{regBusy ? "…" : `Fund House #${netOwn}'s part`}</button>
                <p class="hint">Sets aside the coins paying your net at par{netRound.starter === netOwn ? `, and the round's fee (${netRound.fee} ${BASE_TICKER})` : ""};
                  a house owing nothing still marks its part funded. Your node holds those coins for the round until it's
                  sent, or until the node restarts.</p>
              {:else if netStep.kind === "sign"}
                <button on:click={() => nettingAct("sign")} disabled={regBusy}>{regBusy ? "…" : `Sign for House #${netOwn}`}</button>
                <p class="hint">Signing agrees to every bundle, net and payment above{netRound.starter === netOwn ? `, and to paying the round's fee` : ""}. The last signature sends the round.</p>
              {:else if netStep.kind === "done"}
                <p class="muted small">Every house has signed: the last signature sent the round.</p>
              {:else}
                <p class="muted small">{netStep.why}</p>
              {/if}
            {/if}
          {/if}
          {#if netOut}
            <label>
              Pass this round on to the next house
              <textarea rows="3" readonly class="mono small" value={netOut}></textarea>
            </label>
            <button on:click={() => navigator.clipboard?.writeText(netOut)}>Copy</button>
          {/if}
        </div>
      </div>
      </details>
    {:else if seg === "pools"}
      <!-- Pools (M3): notes ⇄ ECX, a constant-product pool -->
      {#if locked.lp}
        <div class="card locked-read">
          <p>Your wallet is locked: this node lists notes, pool shares and bills only while it's unlocked.</p>
          <button on:click={unlockAll}>Unlock</button>
        </div>
      {/if}
      {#if pools.length > 0}
      <div class="card">
        <div class="notes-head">
          <h2>My pool shares</h2>
          <button class="link-btn" on:click={loadPools} disabled={poolsLoading}>
            {poolsLoading ? "…" : "Refresh"}
          </button>
        </div>
        {#if locked.lp}
          <p class="muted small">—</p>
        {:else if myLp.length === 0}
          <p class="muted">You haven't added to any pool. Adding to one below earns a share of its swap fees.</p>
        {:else}
          {#each myLp as lp}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Pool #{lp.pool_id}</span>
                  <span class="badge badge-o">{pct(lp.share_bps)}</span>
                </div>
                <div class="note-units">{lp.lp_units.toLocaleString()} shares · fee {pct(lp.fee_bps)}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">My share</span> {pct(lp.share_bps)} of {lp.lp_supply.toLocaleString()} shares</div>
                <div><span class="stat-label">Underlying notes</span> {fmtEcx(lp.my_note_units)} {BASE_TICKER}</div>
                <div><span class="stat-label">Underlying {BASE_TICKER}</span> {fmtEcx(lp.my_btx_sats)} {BASE_TICKER}</div>
              </div>
            </div>
          {/each}
        {/if}
      </div>

      {/if}

      <div class="card">
        <div class="notes-head">
          <h2>Pools</h2>
          <button class="link-btn" on:click={loadPools} disabled={poolsLoading}>
            {poolsLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">A pool swaps a house's notes for {BASE_TICKER} and back, at a price set by what it holds. Swap here, or put notes and {BASE_TICKER} in a pool to earn a share of its fees.</p>
        {#if pools.length === 0}
          <div class="empty">
            <p>No pools on this network yet.</p>
            <p class="muted small">
              {houses.length === 0
                ? "Each pool trades one house's notes, so pools come after houses, and there are no houses yet."
                : "A house's note holders can seed the first one below."}
            </p>
          </div>
        {:else}
          {#each pools as p}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Pool #{p.pool_id}</span>
                  <span class="badge badge-o">fee {pct(p.fee_bps)}</span>
                </div>
                <div class="note-units">{price(p)}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Note reserve</span> {fmtEcx(p.note_reserve)} {BASE_TICKER}</div>
                <div><span class="stat-label">{BASE_TICKER} reserve</span> {fmtEcx(p.btx_reserve)} {BASE_TICKER}</div>
                <div><span class="stat-label">Pool shares</span> {p.lp_supply.toLocaleString()}</div>
              </div>
              <div class="note-actions">
                <button on:click={() => startPoolAction("swap", p.pool_id)}>Swap</button>
                <button on:click={() => startPoolAction("add", p.pool_id)}>Add</button>
                <button on:click={() => startPoolAction("remove", p.pool_id)}>Remove</button>
              </div>
              {#if poolAction && poolAction.poolId === p.pool_id}
                <div class="note-form">
                  {#if poolAction.type === "swap"}
                    <div class="conn-modes">
                      <button
                        class:active={swapDir === "noteforbtx"}
                        on:click={() => (swapDir = "noteforbtx")}
                      >Note → {BASE_TICKER}</button>
                      <button
                        class:active={swapDir === "btxfornote"}
                        on:click={() => (swapDir = "btxfornote")}
                      >{BASE_TICKER} → Note</button>
                    </div>
                    <label>
                      {swapDir === "noteforbtx" ? `Notes in (${BASE_TICKER})` : `${BASE_TICKER} in`}
                      <input type="number" bind:value={poolAmountIn} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <label>
                      {swapDir === "noteforbtx" ? `Least ${BASE_TICKER} out` : `Least notes out (${BASE_TICKER})`}
                      <input type="number" bind:value={poolMinOut} placeholder="empty or 0 = no slippage limit" step="0.00000001" min="0" />
                    </label>
                  {:else if poolAction.type === "add"}
                    <label>
                      Notes ({BASE_TICKER})
                      <input type="number" bind:value={poolAddNoteEcx} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <label>
                      {BASE_TICKER}
                      <input type="number" bind:value={poolAddEcx} placeholder="0.00000000" step="0.00000001" min="0" />
                    </label>
                    <p class="hint">Liquidity is deposited pro-rata to the pool's current ratio; excess is refunded.</p>
                  {:else}
                    <label>
                      Shares to take out
                      <input type="number" bind:value={poolRemoveLp} placeholder="shares" />
                    </label>
                    <p class="hint">Gives back your shares and pays you their notes and {BASE_TICKER} at the pool's current ratio.</p>
                  {/if}
                  <div class="note-form-actions">
                    <button on:click={submitPoolAction} disabled={poolBusy}>
                      {poolBusy ? "…" : poolAction.type === "swap" ? "Swap" : poolAction.type === "add" ? "Add liquidity" : "Remove liquidity"}
                    </button>
                    <button class="secondary" on:click={() => (poolAction = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      {#if houses.length > 0}
      <details class="advanced tools">
        <summary>House tools: create a pool</summary>
      <div class="card">
        <h3>Create a pool</h3>
        <p class="muted">Seed a new note/{BASE_TICKER} pool. The pool id is the house id whose notes it trades. You supply both sides of the initial reserves.</p>
        <div class="form">
          <label>
            Pool id (house id)
            <input type="number" bind:value={createPoolId} placeholder="e.g. 1" />
          </label>
          <label>
            Seed notes ({BASE_TICKER})
            <input type="number" bind:value={createNoteEcx} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          <label>
            Seed {BASE_TICKER}
            <input type="number" bind:value={createEcx} placeholder="0.00000000" step="0.00000001" min="0" />
          </label>
          <label>
            Fee (hundredths of a percent: 30 = 0.3%)
            <input type="number" bind:value={createFeeBps} placeholder="e.g. 30" />
          </label>
          {#if createPoolId && typeOf(houses, createPoolId) !== "open"}
            <p class="blocked-why">House #{createPoolId} is members only: its notes can't go into a pool.</p>
          {/if}
          <button on:click={doCreatePool} disabled={poolBusy || !createPoolId || !createNoteEcx || !createEcx || typeOf(houses, createPoolId) !== "open"}>
            {poolBusy ? "…" : "Create pool"}
          </button>
        </div>
      </div>
      </details>
      {/if}
    {:else if seg === "bills"}
      <!-- Bills (M4): bills of exchange — the discount-house asset side -->
      {#if locked.bills}
        <div class="card locked-read">
          <p>Your wallet is locked: this node lists notes, pool shares and bills only while it's unlocked.</p>
          <button on:click={unlockAll}>Unlock</button>
        </div>
      {/if}
      <div class="card">
        <div class="notes-head">
          <h2>My bills</h2>
          <button class="link-btn" on:click={loadBills} disabled={billsLoading}>
            {billsLoading ? "…" : "Refresh"}
          </button>
        </div>
        <p class="muted">A bill is a promise to pay a set amount of {BASE_TICKER} on a set date, backed by a bond held until then. Here are the bills you hold, wrote, or agreed to pay.</p>
        {#if locked.bills}
          <p class="muted small">—</p>
        {:else if bills.length === 0}
          <div class="empty">
            <p>You have no bills yet.</p>
            <p class="muted small">
              {balance > 0
                ? "Issue one under House tools, or share a bill key so someone can hand a bill to you."
                : "Someone can hand a bill to you: share a bill key below."}
            </p>
          </div>
        {:else}
          {#each bills as b}
            <div class="note-row">
              <div class="note-top">
                <div>
                  <span class="note-house">Bill #{b.id}</span>
                  <span class="badge badge-bill-{b.status}">{BILL_STATUS[b.status] ?? b.status}</span>
                </div>
                <div class="note-units">{b.amount} {BASE_TICKER}</div>
              </div>
              <div class="house-stats">
                <div><span class="stat-label">Bond</span> {b.escrow} {BASE_TICKER}</div>
                <div><span class="stat-label">Matures</span> {b.maturity_height > height ? `in about ${days(b.maturity_height - height)}` : "now"} (block {b.maturity_height}), then {days(b.grace_blocks)} to pay</div>
                {#if b.roles && b.roles.length}
                  <div><span class="stat-label">Your role</span> {b.roles.join(", ")}</div>
                {/if}
              </div>
              <div class="note-actions">
                <button on:click={() => { billAction = { type: "endorse", id: b.id }; endorsePubkey = ""; }} disabled={b.status !== "a"}>Endorse</button>
                <button on:click={() => doRetireBill(b.id)} disabled={billBusy || b.status !== "a"}>Retire</button>
                <button on:click={() => doClaimBillEscrow(b.id)} disabled={billBusy || b.status !== "d"}>Claim the bond</button>
              </div>
              {#if billAction && billAction.id === b.id}
                <div class="note-form">
                  <label>
                    Hand it to (their bill key)
                    <input type="text" bind:value={endorsePubkey} placeholder="02…" />
                  </label>
                  <div class="note-form-actions">
                    <button on:click={doEndorseBill} disabled={billBusy || !endorsePubkey}>
                      {billBusy ? "…" : "Endorse bill"}
                    </button>
                    <button class="secondary" on:click={() => (billAction = null)}>Cancel</button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

      <div class="card">
        <h3>Receive a bill</h3>
        <p class="muted">Share a new bill key so someone can hand a bill to you.</p>
        {#if newBillPubkey}
          <div class="address-display">
            <code>{newBillPubkey}</code>
            <button on:click={() => navigator.clipboard.writeText(newBillPubkey)}>Copy</button>
          </div>
        {/if}
        <button on:click={getBillPubkey}>New bill key</button>
      </div>

      <details class="advanced tools">
        <summary>House tools: issue a bill</summary>
      <div class="card">
        <h3>Issue a bill</h3>
        <p class="muted">Draw and accept a bill: a dated promise for a face amount, backed by a bond the holder can claim if it isn't paid.</p>
        <div class="form">
          <label>
            Description
            <input type="text" bind:value={billBody} placeholder="e.g. 90-day trade bill" />
          </label>
          <label>
            Face amount ({BASE_TICKER})
            <input type="number" bind:value={billAmount} placeholder="e.g. 1.0" step="0.00000001" />
          </label>
          <label>
            Escrow bond ({BASE_TICKER})
            <input type="number" bind:value={billEscrow} placeholder="e.g. 0.1" step="0.00000001" />
          </label>
          <label>
            Matures in (FreeBank blocks, about 144 a day){billMatureIn ? `: about ${days(parseInt(billMatureIn) || 0)}` : ""}
            <input type="number" bind:value={billMatureIn} placeholder="1000" />
          </label>
          <label>
            Then blocks to pay it in{billGrace ? `: about ${days(parseInt(billGrace) || 0)}` : ""}
            <input type="number" bind:value={billGrace} placeholder="1008" />
          </label>
          <button on:click={doIssueBill} disabled={billBusy || !billAmount || !billEscrow || balance === 0}>
            {billBusy ? "…" : "Issue bill"}
          </button>
          {#if balance === 0}<p class="blocked-why">Issuing a bill takes a bond in ECX. {NEED_COINS}</p>{/if}
        </div>
      </div>
      </details>
{/if}

<style>
  /* The issuer's forms, folded at the foot of each segment (v0.2.6), in the app's fold (.advanced, v0.2.7: the
     walk-through found its ▶ unlike the others). The forms' cards lie flat inside it. */
  .tools {
    margin-top: 8px;
    margin-bottom: 14px;
  }
  .tools > :global(.card) {
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 4px 0 14px;
    margin: 0;
  }
  .tools > :global(.card + .card) {
    border-top: 1px solid var(--border-color);
    padding-top: 14px;
  }
  .locked-read {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .locked-read p {
    margin: 0;
  }
</style>
