<script lang="ts">
  // The wallet flows, over every screen; WalletGate shows the one $walletFlow asks for.
  //   protect        the passphrase first (twice; once if the wallet already has one), then new
  //                  recovery words (the default: shown, then three asked back) or restored ones
  //                  (typed in; the chain is scanned for their coins)
  //   restore-words  Settings: typed words into a new wallet, the current one moved aside first
  //   confirm-words  the saved words again, behind the passphrase, then three asked back
  //   move           coins on older addresses moved to one new address the words cover
  //   restore-file   Settings: a checked backup file into the wallet's place
  // The long parts run in the app (src-tauri/src/recovery/job.rs), which may restart the node; this
  // follows their progress. Passphrases and words go to the app and are dropped here once used.
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import Notice from "./Notice.svelte";
  import PassphraseFields from "./PassphraseFields.svelte";
  import PathText from "./PathText.svelte";
  import RecoveryWords from "./RecoveryWords.svelte";
  import { fmtEcx } from "../lib/amount";
  import { BASE_TICKER } from "../lib/brand";
  import { nice } from "../lib/errors";
  import { showReceipt } from "../lib/receipts";
  import { withUnlock } from "../lib/wallet";
  import {
    WORD_COUNT,
    loadProtection,
    threePositions,
    walletSeed,
    wordsIn,
    type Moved,
    type MovePlan,
    type Protection,
    type SetupProgress,
    type SetupStage,
    type WalletFlowRequest,
    type WordsCheck,
  } from "../lib/walletSeed";

  export let request: WalletFlowRequest;

  const dispatch = createEventDispatcher<{ close: void }>();

  type Step =
    | "loading"
    | "aside"
    | "passphrase"
    | "choose"
    | "words-in"
    | "working"
    | "unlock-words"
    | "show-words"
    | "confirm"
    | "done"
    | "move";
  let step: Step = "loading";
  let prot: Protection | null = null;
  let error = "";
  let busy = false;

  // The passphrase: a new one twice, or the wallet's own once when it already has one.
  let fields: PassphraseFields;
  let pass = "";
  let passOk = false;
  $: fresh = request.kind === "restore-words";
  $: hasPass = !!prot?.encrypted && !fresh;

  // Restoring: the words typed in, checked as they come.
  let restoring = request.kind === "restore-words";
  let typed = "";
  let check: WordsCheck | null = null;
  let checkTimer: ReturnType<typeof setTimeout> | null = null;

  // The job, while it runs.
  let prog: SetupProgress | null = null;
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let polling = false;

  // New words, shown once, then three of them asked back.
  let words: string[] = [];
  let hidden = false;
  let positions: number[] = [];
  let answers = ["", "", ""];
  let confirmError = "";

  // The end: a backup, and coins still on older addresses.
  let plan: MovePlan | null = null;
  let moved: Moved | null = null;
  let backupSaved: string[] = [];
  let backupError = "";
  // The new words couldn't be collected to show (too long after the setup): they are saved in
  // FreeBank, and the passphrase shows them.
  let wordsMissed = false;

  const STAGE_LABEL: Record<string, string> = {
    aside: "Move your current wallet aside",
    start: "Start FreeBank",
    encrypt: "Encrypt the wallet with your passphrase",
    restart: "Start FreeBank again",
    seed: "Set up your recovery words",
    save: "Keep an encrypted copy of the words",
    scan: "Look for your coins",
  };
  $: stageLabel = (s: SetupStage) =>
    prog?.kind === "restore-file" && s === "start" ? "Start FreeBank with the backup" : STAGE_LABEL[s] ?? s;

  $: title =
    step === "show-words" || step === "unlock-words"
      ? "Your recovery words"
      : step === "confirm"
        ? "Check your words"
        : step === "move"
          ? "Move your coins to your recovery words"
          : step === "choose"
            ? "Your recovery words"
            : step === "words-in"
              ? "Type your recovery words"
              : step === "working"
                ? prog?.kind === "new"
                  ? "Protecting your wallet"
                  : "Restoring your wallet"
                : step === "done"
                  ? doneTitle()
                  : request.kind === "restore-words"
                    ? "Restore from recovery words"
                    : hasPass
                      ? "Set up your recovery words"
                      : "Protect your wallet";

  function doneTitle(): string {
    if (request.kind === "confirm-words") return "Your words are checked";
    if (prog?.kind === "restore-file") return "Your backup is restored";
    if (prog?.kind === "restore-words") return "Your wallet is restored";
    return "Your wallet is protected";
  }

  // Closing is only offered where nothing is under way.
  $: closable = step !== "working" && step !== "show-words" && step !== "confirm" && !busy;

  function close() {
    wipeWords();
    dispatch("close");
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && closable) close();
  }

  // Leaving the window hides the words until asked for again.
  function onVisibility() {
    if (document.hidden && words.length) hidden = true;
  }

  function wipeWords() {
    words.fill("");
    words = [];
    answers = ["", "", ""];
  }

  onMount(async () => {
    document.addEventListener("visibilitychange", onVisibility);
    prot = await loadProtection();
    // A job already under way (the window reloaded part way through): follow it.
    const p = await walletSeed.setupProgress().catch(() => null);
    if (p?.running) {
      prog = p;
      step = "working";
      watch();
      return;
    }
    if (!prot && request.kind !== "restore-file") {
      error = "Your FreeBank node isn't answering. It may be stopped or still starting; the Node tab shows which.";
    }
    switch (request.kind) {
      case "protect":
        step = "passphrase";
        break;
      case "restore-words":
        step = "aside";
        break;
      case "confirm-words":
        step = "unlock-words";
        break;
      case "move":
        step = "move";
        loadPlan();
        break;
      case "restore-file":
        await startRestoreFile();
        break;
    }
  });

  onDestroy(() => {
    document.removeEventListener("visibilitychange", onVisibility);
    if (pollTimer) clearTimeout(pollTimer);
    if (checkTimer) clearTimeout(checkTimer);
    polling = false;
    pass = "";
    typed = "";
    wipeWords();
  });

  // ---- Passphrase, then new or restored words ----

  function passphraseDone() {
    if (!passOk) return;
    error = "";
    step = fresh ? "words-in" : "choose";
  }

  function choose(restore: boolean) {
    restoring = restore;
    if (restore) {
      step = "words-in";
    } else {
      begin();
    }
  }

  $: onTyped(typed);
  function onTyped(t: string) {
    if (checkTimer) clearTimeout(checkTimer);
    if (!t.trim()) {
      check = null;
      return;
    }
    checkTimer = setTimeout(async () => {
      try {
        check = await walletSeed.checkWords(t);
      } catch {
        check = null;
      }
    }, 200);
  }

  $: wordsStatus = !check
    ? ""
    : check.unknown.length
      ? `Word ${check.unknown[0]} isn't one of the recovery words. Check its spelling.`
      : check.count < WORD_COUNT
        ? `${check.count} of ${WORD_COUNT} words`
        : check.count > WORD_COUNT
          ? `That's ${check.count} words. FreeBank's recovery words are ${WORD_COUNT}.`
          : check.checksum_failed
            ? `These ${WORD_COUNT} words don't fit together: one of them is wrong or out of place.`
            : `${WORD_COUNT} words, and they fit together.`;

  async function begin() {
    error = "";
    busy = true;
    try {
      await walletSeed.setupStart(pass, restoring ? typed : null, fresh);
    } catch (e) {
      error = nice(e);
      busy = false;
      return;
    }
    // They went to the app; nothing keeps them here.
    pass = "";
    typed = "";
    check = null;
    busy = false;
    prog = null;
    step = "working";
    watch();
  }

  // ---- Following a job ----

  function watch() {
    polling = true;
    const tick = async () => {
      if (!polling) return;
      try {
        prog = await walletSeed.setupProgress();
      } catch {
        // The app is busy with the node; ask again.
      }
      if (prog && !prog.running && (prog.done || prog.error)) {
        polling = false;
        await finished(prog);
        return;
      }
      pollTimer = setTimeout(tick, 600);
    };
    tick();
  }

  async function finished(p: SetupProgress) {
    prot = await loadProtection();
    if (p.error) return;
    if (p.kind === "new") {
      const w = await walletSeed.setupWords().catch(() => null);
      if (w && w.length === WORD_COUNT) {
        words = w;
        hidden = false;
        step = "show-words";
        return;
      }
      wordsMissed = true;
    }
    await toDone();
  }

  function stageState(s: SetupStage, p: SetupProgress | null): "done" | "active" | "failed" | "todo" {
    if (!p) return "todo";
    if (p.done) return "done";
    const at = p.stages.indexOf(p.stage);
    const i = p.stages.indexOf(s);
    if (i < at) return "done";
    if (i === at) return p.error ? "failed" : "active";
    return "todo";
  }

  // After a failed job: back to the passphrase (the wallet may have one now), or out.
  async function afterFailure() {
    error = "";
    prot = await loadProtection();
    prog = null;
    if (request.kind === "restore-file") close();
    else step = request.kind === "restore-words" ? "aside" : "passphrase";
  }

  // ---- The words: shown once, then three asked back ----

  function wroteThemDown() {
    positions = threePositions();
    answers = ["", "", ""];
    confirmError = "";
    step = "confirm";
  }

  // Pasting all the words into any box fills the three asked for (a paste of one word is left alone).
  // Not straight from FreeBank's own copy, though: that proves no lasting backup, since the copy is
  // cleared after a minute (security review L5). Leaving the window after copying (to paste them
  // into notes or a password manager) counts as saving them.
  let copiedHere = false;
  function onCopied() {
    copiedHere = true;
  }
  function pasteWords(e: ClipboardEvent) {
    const got = wordsIn(e.clipboardData?.getData("text/plain") ?? "");
    // Exactly the 24 words: with a title or a stray word along, every position would shift (code review 9).
    if (got.length !== WORD_COUNT) return;
    e.preventDefault();
    if (copiedHere) {
      confirmError =
        "That is FreeBank's own copy, which it clears in a minute. Paste the words into your notes or password manager first, then copy them from there, or type the three words.";
      return;
    }
    confirmError = "";
    answers = positions.map((p) => got[p - 1] ?? "");
  }

  async function checkAnswers() {
    const wrong = positions.filter((pos, i) => answers[i].trim().toLowerCase() !== words[pos - 1]);
    if (wrong.length) {
      confirmError = `Word ${wrong[0]} isn't right. Look at your paper again, or show the words again.`;
      return;
    }
    busy = true;
    try {
      await walletSeed.wordsConfirmed();
    } catch (e) {
      confirmError = nice(e);
      busy = false;
      return;
    }
    busy = false;
    wipeWords();
    prot = await loadProtection();
    await toDone();
  }

  async function revealWords() {
    if (!passOk) return;
    busy = true;
    error = "";
    try {
      const r = await walletSeed.reveal(pass, "words");
      pass = "";
      fields?.clear();
      if (r.matches_wallet === false) {
        error = "These words belong to another seed than your wallet has now, so they can't be checked for it.";
      }
      words = r.words ?? [];
      hidden = false;
      step = "show-words";
    } catch (e) {
      error = nice(e);
      fields?.clear();
    }
    busy = false;
  }

  // ---- The end ----

  async function toDone() {
    step = "done";
    if (prot?.protected && !prot.new_wallet) loadPlan();
  }

  async function loadPlan() {
    try {
      plan = await walletSeed.movePlan();
    } catch (e) {
      if (step === "move") error = nice(e);
    }
  }

  async function backup() {
    busy = true;
    backupError = "";
    try {
      backupSaved = await walletSeed.backupNow();
      prot = await loadProtection();
    } catch (e) {
      backupError = nice(e);
    }
    busy = false;
  }

  async function moveCoins() {
    busy = true;
    error = "";
    try {
      const m = await withUnlock(() => walletSeed.moveCoins(), { what: "move your coins to your recovery words" });
      moved = m;
      showReceipt({
        txid: m.txid,
        what: `Moved ${fmtEcx(m.sent_sats)} ${BASE_TICKER} to your recovery words`,
        rows: [
          { label: "To", value: m.to, mono: true },
          { label: "Fee", value: `${fmtEcx(m.fee_sats)} ${BASE_TICKER}` },
        ],
      });
      plan = null;
      step = "move";
    } catch (e) {
      error = nice(e);
    }
    busy = false;
  }

  // ---- Restore a backup file ----

  async function startRestoreFile() {
    if (request.kind !== "restore-file") return;
    error = "";
    try {
      await walletSeed.restoreFileStart(request.file.token);
    } catch (e) {
      error = nice(e);
      step = "done";
      return;
    }
    prog = null;
    step = "working";
    watch();
  }

  function asProtect() {
    request = { kind: "protect" };
    restoring = false;
    error = "";
    step = "passphrase";
  }
</script>

<svelte:window on:keydown={onKey} on:blur={() => (copiedHere = false)} />

<div class="wf-back">
  <div class="wf card" role="dialog" aria-modal="true" aria-labelledby="wf-title">
    <h2 id="wf-title">{title}</h2>

    {#if step === "loading"}
      <div class="spinner big" aria-hidden="true"></div>
    {:else if step === "aside"}
      <p class="wf-lede">
        FreeBank moves your current wallet aside, where it is kept and never deleted. It then makes a new wallet with a
        passphrase you choose, gives it your recovery words, and looks through the chain for their coins.
      </p>
      {#if prot && !prot.node_is_ours}
        <Notice kind="error" dismissible={false}>
          Your node was started by another program, so FreeBank can't restart it with a new wallet. Stop it there first.
        </Notice>
      {/if}
      <div class="row-actions">
        <button on:click={() => (step = "passphrase")} disabled={!!prot && !prot.node_is_ours}>Continue</button>
        <button class="secondary" on:click={close}>Cancel</button>
      </div>
    {:else if step === "passphrase"}
      {#if fresh}
        <p class="wf-lede">Choose a passphrase for the restored wallet. FreeBank asks for it whenever you send.</p>
      {:else if hasPass}
        <p class="wf-lede">Your wallet already has a passphrase. Enter it, and FreeBank sets up your recovery words.</p>
      {:else}
        <p class="wf-lede">
          Choose a passphrase for your wallet. FreeBank asks for it whenever you send, and it keeps your wallet file safe
          on this computer.
        </p>
        <p class="wf-warn">
          Nobody can recover your passphrase for you. If you forget it, your recovery words bring the wallet back.
        </p>
      {/if}
      <form class="wf-form" on:submit|preventDefault={passphraseDone}>
        <PassphraseFields
          bind:this={fields}
          bind:value={pass}
          bind:valid={passOk}
          twice={!hasPass}
          label={hasPass ? "Your wallet's passphrase" : "Passphrase"}
        />
        {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
        <div class="row-actions">
          <button type="submit" disabled={!passOk}>Continue</button>
          {#if request.kind === "protect"}
            <button type="button" class="secondary" on:click={close}>Not now</button>
          {:else}
            <button type="button" class="secondary" on:click={close}>Cancel</button>
          {/if}
        </div>
      </form>
      {#if request.kind === "protect" && request.firstRun}
        <p class="hint">Until your wallet is protected, FreeBank doesn't show any address to receive coins on.</p>
      {/if}
    {:else if step === "choose"}
      <p class="wf-lede">
        Your recovery words are 24 words you write down. With them and a new passphrase, your wallet comes back on any
        computer.
      </p>
      <div class="wf-choices">
        <button class="wf-choice" on:click={() => choose(false)} disabled={busy}>
          <strong>New recovery words</strong>
          <span>FreeBank makes 24 new words for this wallet.</span>
        </button>
        <button class="wf-choice secondary" on:click={() => choose(true)} disabled={busy}>
          <strong>Restore from recovery words</strong>
          <span>You have 24 words from an earlier FreeBank wallet.</span>
        </button>
      </div>
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      <button class="link-btn" on:click={() => { pass = ""; step = "passphrase"; }} disabled={busy}>← Back</button>
    {:else if step === "words-in"}
      <p class="wf-lede">All {WORD_COUNT} words, in order, with spaces or new lines between them. A numbered list works too.</p>
      <textarea
        class="wf-words"
        bind:value={typed}
        rows="6"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        aria-label="Your recovery words"
        aria-describedby="wf-words-status"
      ></textarea>
      <p id="wf-words-status" class="wf-status" class:ok={check?.ok} class:bad={!!check && !check.ok && (check.unknown.length > 0 || check.checksum_failed || check.count > WORD_COUNT)} aria-live="polite">
        {wordsStatus}
      </p>
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      <div class="row-actions">
        <button on:click={begin} disabled={!check?.ok || busy}>{busy ? "Starting…" : "Restore my wallet"}</button>
        <button class="secondary" on:click={() => (step = fresh ? "passphrase" : "choose")} disabled={busy}>Back</button>
      </div>
    {:else if step === "working"}
      <ul class="stages">
        {#each prog?.stages ?? [] as s}
          {@const st = stageState(s, prog)}
          <li class="stage {st}">
            <span class="stage-icon">
              {#if st === "done"}✓{:else if st === "failed"}!{:else if st === "active"}<span class="spinner"></span>{/if}
            </span>
            <span class="stage-text">
              {stageLabel(s)}
              {#if st === "active" && s === "scan" && prog?.scan_to != null}
                <span class="stage-note">Block {(prog.scan_at ?? 0).toLocaleString()} of {prog.scan_to.toLocaleString()}</span>
                <span class="bar"><span class="bar-fill" style="width:{prog.scan_to ? ((prog.scan_at ?? 0) / prog.scan_to) * 100 : 0}%"></span></span>
              {:else if st === "active" && prog?.note && !prog.waiting_for_node}
                <span class="stage-note">{prog.note}</span>
              {/if}
            </span>
          </li>
        {:else}
          <li class="stage active"><span class="stage-icon"><span class="spinner"></span></span><span class="stage-text">Checking your wallet…</span></li>
        {/each}
      </ul>
      {#if prog?.waiting_for_node}
        <Notice kind="info" dismissible={false}>
          Start your FreeBank node again in the program that runs it, for example BitWindow. FreeBank carries on by
          itself once it's back.
        </Notice>
      {/if}
      {#if prog?.moved_aside}
        <p class="hint">Your previous wallet is kept at <span class="mono"><PathText path={prog.moved_aside} /></span></p>
      {/if}
      {#if prog?.error}
        <p class="soft-error" role="alert">{prog.error}</p>
        <div class="row-actions">
          <button on:click={afterFailure}>{request.kind === "restore-file" ? "Close" : "Back"}</button>
        </div>
      {:else}
        <p class="hint">This can take a few minutes. FreeBank's node restarts along the way.</p>
      {/if}
    {:else if step === "unlock-words"}
      <p class="wf-lede">Enter your wallet passphrase to see your recovery words, then check three of them.</p>
      <form class="wf-form" on:submit|preventDefault={revealWords}>
        <PassphraseFields bind:this={fields} bind:value={pass} bind:valid={passOk} twice={false} label="Your wallet's passphrase" />
        {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
        <div class="row-actions">
          <button type="submit" disabled={!passOk || busy}>{busy ? "Opening…" : "Show my words"}</button>
          <button type="button" class="secondary" on:click={close} disabled={busy}>Cancel</button>
        </div>
      </form>
    {:else if step === "show-words"}
      <p class="wf-lede">
        Write these {WORD_COUNT} words on paper, in order, and keep them somewhere safe. They bring your wallet back if this
        computer is lost or you forget your passphrase.
      </p>
      <p class="wf-warn">
        Anyone who has these words can take your coins. Never type them into a website or a message. FreeBank only asks
        for them to restore a wallet.
      </p>
      {#if hidden}
        <div class="wf-hidden">
          <span>Your words are hidden.</span>
          <button class="secondary" on:click={() => (hidden = false)}>Show them</button>
        </div>
      {:else}
        <RecoveryWords {words} on:copied={onCopied} />
      {/if}
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      <p class="hint">
        These words also cover the app's eCash wallets. Other wallets can't read FreeBank's addresses from them
        directly.
      </p>
      <div class="row-actions">
        <button on:click={wroteThemDown} disabled={hidden}>I've written them down</button>
      </div>
    {:else if step === "confirm"}
      <p class="wf-lede">To be sure your paper (or copy) is right, type these three words from it, or paste all {WORD_COUNT}.</p>
      <form class="wf-form" on:submit|preventDefault={checkAnswers}>
        {#each positions as pos, i}
          <label class="wf-answer">
            Word {pos}
            <input type="text" bind:value={answers[i]} on:paste={pasteWords} autocomplete="off" autocapitalize="off" spellcheck="false" />
          </label>
        {/each}
        {#if confirmError}<p class="soft-error" role="alert">{confirmError}</p>{/if}
        <div class="row-actions">
          <button type="submit" disabled={answers.some((a) => !a.trim()) || busy}>Check</button>
          <button type="button" class="secondary" on:click={() => { step = "show-words"; hidden = false; }} disabled={busy}>
            Show the words again
          </button>
        </div>
      </form>
    {:else if step === "done"}
      {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
      {#if request.kind === "confirm-words"}
        <p class="wf-lede">Your paper matches your wallet's recovery words. Keep it safe.</p>
      {:else if prog?.kind === "restore-file" || prog?.kind === "restore-words"}
        <p class="wf-lede">
          {prog.kind === "restore-file" ? "Your backup is in place" : "Your wallet has your recovery words"}, and FreeBank
          looked through the chain for its coins{prog.scan_to != null ? `, up to block ${prog.scan_to.toLocaleString()}` : ""}.
          Coins still arriving show up as your node catches up.
        </p>
        {#if prog.moved_aside}
          <p class="hint">Your previous wallet is kept at <span class="mono"><PathText path={prog.moved_aside} /></span></p>
        {/if}
        {#if prot && !prot.protected}
          <Notice kind="info" dismissible={false}>
            {prot.encrypted
              ? "FreeBank's recovery words don't cover this wallet. Set up recovery words for it, and FreeBank offers to move its coins onto them."
              : "This wallet has no passphrase. Protect it now: choose a passphrase and get your recovery words."}
          </Notice>
          <div class="row-actions">
            <button on:click={asProtect}>{prot.encrypted ? "Set up recovery words" : "Protect my wallet"}</button>
          </div>
        {/if}
      {:else if wordsMissed}
        <p class="wf-lede">Your wallet has a passphrase and its recovery words, saved in FreeBank.</p>
        <Notice kind="info" dismissible={false}>Write your recovery words down now: FreeBank shows them after your passphrase.</Notice>
        <div class="row-actions">
          <button on:click={() => { request = { kind: "confirm-words" }; wordsMissed = false; error = ""; step = "unlock-words"; }}>Show my words</button>
        </div>
      {:else}
        <ul class="wf-done">
          <li>Your wallet has a passphrase. FreeBank asks for it when you send.</li>
          <li>You have your {WORD_COUNT} recovery words on paper.</li>
        </ul>
      {/if}

      {#if prot?.protected && request.kind !== "confirm-words"}
        <div class="wf-box">
          <strong>Back up your wallet</strong>
          <span>
            A backup keeps your notes, labels and history too. Your wallet got a new seed just now, so backups made
            before today don't cover the addresses it gives from now on.
          </span>
          {#each backupSaved as s}<span class="mono small">Saved to <PathText path={s} /></span>{/each}
          {#if backupError}<span class="soft-error">{backupError}</span>{/if}
          <button class="secondary" on:click={backup} disabled={busy}>{backupSaved.length ? "Back up again" : "Back up now"}</button>
        </div>
      {/if}

      {#if plan && plan.coins > 0}
        <div class="wf-box">
          <strong>Move your coins to your recovery words</strong>
          <span>
            {fmtEcx(plan.total_sats)} {BASE_TICKER} sits on addresses from before your recovery words, which the words don't
            cover. FreeBank can move it to a new address they do.
          </span>
          <button on:click={() => (step = "move")} disabled={busy}>Move my coins…</button>
        </div>
      {/if}
      <div class="row-actions">
        <button class="secondary" on:click={close} disabled={busy}>Close</button>
      </div>
    {:else if step === "move"}
      {#if moved}
        <p class="wf-lede">
          Moved {fmtEcx(moved.sent_sats)} {BASE_TICKER} in {moved.coins} {moved.coins === 1 ? "coin" : "coins"} to
          <span class="mono">{moved.to}</span>, for a fee of {fmtEcx(moved.fee_sats)} {BASE_TICKER}. Your receipt follows it
          until it is confirmed.
        </p>
        <p class="wf-warn">
          Backups made before you set the passphrase have no passphrase, and they hold the keys to your old addresses:
          anyone who has one can spend whatever still reaches those addresses. Delete them, and don't give out your old
          addresses again.
        </p>
        {#if moved.later > 0}
          <p class="hint">{moved.later} more {moved.later === 1 ? "coin is" : "coins are"} left. Move again once this payment is on its way.</p>
          <div class="row-actions">
            <button on:click={() => { moved = null; loadPlan(); }}>Move the rest…</button>
          </div>
        {/if}
      {:else if !plan}
        {#if error}<p class="soft-error" role="alert">{error}</p>{:else}<div class="spinner big" aria-hidden="true"></div>{/if}
      {:else if plan.coins === 0}
        <p class="wf-lede">Nothing to move: all your coins are on addresses your recovery words cover.</p>
      {:else}
        <p class="wf-lede">
          {fmtEcx(plan.total_sats)} {BASE_TICKER} in {plan.coins} {plan.coins === 1 ? "coin" : "coins"} sits on addresses
          from before your recovery words, or on keys added by hand. FreeBank moves it in one payment to a new address
          of this wallet that your words cover. The fee comes out of the amount.
        </p>
        {#if plan.has_notes}
          <p class="hint">
            Notes, bills, term deposits and pool shares don't move this way: this moves plain {BASE_TICKER} only. They stay
            in this wallet and its backups, but your recovery words don't cover them.
          </p>
        {/if}
        {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
        <div class="row-actions">
          <button on:click={moveCoins} disabled={busy}>{busy ? "Moving…" : "Move my coins"}</button>
        </div>
      {/if}
      <div class="row-actions wf-close">
        <button class="secondary" on:click={close} disabled={busy}>{moved ? "Done" : "Close"}</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .wf-back {
    position: fixed;
    inset: 0;
    z-index: 40; /* under the unlock prompt (60) and the phone's prompts (50) */
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 24px 16px;
    overflow-y: auto;
    background: rgba(0, 0, 0, 0.7);
  }
  .wf {
    width: 100%;
    max-width: 480px;
    margin: auto 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .wf h2 {
    margin: 0;
  }
  .wf-lede {
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .wf-warn {
    font-size: 13px;
    line-height: 1.5;
    color: #e0c07a;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--accent-tint);
    border: 1px solid rgba(217, 164, 65, 0.3);
  }
  .wf-form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .wf-choices {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .wf-choice {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
    text-align: left;
    padding: 14px 16px;
  }
  .wf-choice span {
    font-size: 13px;
    font-weight: 400;
  }
  .wf-choice.secondary span {
    color: var(--text-secondary);
  }
  .wf-words {
    width: 100%;
    padding: 11px 12px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
    background: var(--bg-inset);
    color: var(--text-color);
    font-family: ui-monospace, Menlo, Consolas, monospace;
    font-size: 14px;
    line-height: 1.6;
    resize: vertical;
  }
  .wf-words:focus {
    outline: none;
    border-color: var(--accent-color);
    box-shadow: 0 0 0 3px var(--accent-tint);
  }
  .wf-status {
    min-height: 1.4em;
    font-size: 12.5px;
    color: var(--text-secondary);
    margin-top: -6px;
  }
  .wf-status.ok {
    color: var(--success-color);
  }
  .wf-status.bad {
    color: #e0a458;
  }
  .wf-hidden {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 18px 14px;
    border-radius: 10px;
    border: 1px dashed var(--border-color);
    font-size: 13.5px;
    color: var(--text-secondary);
  }
  .wf-answer {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    color: var(--text-secondary);
  }
  .wf-done {
    padding-left: 18px;
    font-size: 14px;
    line-height: 1.6;
    color: var(--text-color);
  }
  .wf-box {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 10px;
    border: 1px solid var(--border-color);
    background: var(--bg-inset);
    font-size: 13px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .wf-box strong {
    color: var(--text-color);
    font-size: 14px;
  }
  .wf-box button {
    align-self: flex-start;
    font-size: 13px;
    padding: 7px 14px;
  }
  .wf-close {
    margin-top: 4px;
  }
</style>
