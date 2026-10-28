<script lang="ts">
  // The words' coverage: one sentence, the detail behind More (v0.2.6).
  let coverageMore = false;
  // Settings > Wallet (desktop, the node on this computer).
  //   What the wallet is: its file, passphrase and lock, balances, transactions, key pool, the HD
  //   seed's id, FreeBank's recovery words, the last backup.
  //   Back up; Set or Change the passphrase; Show recovery words and Show xprv (behind the passphrase,
  //   shown on a click, hidden again when the screen goes or the window is left, never copied); Restore
  //   from a backup file or from recovery words (each into the wallet's place, the current wallet moved
  //   aside and kept); Move my coins, when coins sit on addresses the words don't cover.
  import { onDestroy, onMount } from "svelte";
  import PassphraseFields from "./PassphraseFields.svelte";
  import PathText from "./PathText.svelte";
  import RecoveryWords from "./RecoveryWords.svelte";
  import { fmtEcx } from "../lib/amount";
  import { BASE_TICKER } from "../lib/brand";
  import { nice } from "../lib/errors";
  import { walletList } from "../lib/wallets";
  import {
    MAX_BACKUP_BYTES,
    WORD_COUNT,
    loadProtection,
    openWalletFlow,
    protection,
    walletFlow,
    walletSeed,
    when,
    type BackupFile,
    type Checked,
    type MovePlan,
    type WalletInfo,
  } from "../lib/walletSeed";

  let info: WalletInfo | null = null;
  /** The wallet card's technical rows, folded (v0.2.7). */
  let moreFacts = false;
  let loadError = "";
  let plan: MovePlan | null = null;

  async function load() {
    try {
      info = await walletSeed.info();
      loadError = "";
    } catch (e) {
      loadError = nice(e);
      return;
    }
    plan = info.protected ? await walletSeed.movePlan().catch(() => null) : null;
  }

  // Again whenever a flow closes (it may have changed the wallet).
  let flowWasOpen = false;
  $: {
    if ($walletFlow) flowWasOpen = true;
    else if (flowWasOpen) {
      flowWasOpen = false;
      load();
    }
  }

  type Open = "change" | "words" | "xprv" | "file" | "address" | "check-words" | null;
  let open: Open = null;
  const reveals: { which: "words" | "xprv"; title: string; text: string }[] = [
    { which: "words", title: "Show recovery words", text: `The ${WORD_COUNT} words FreeBank keeps, locked with your passphrase.` },
    { which: "xprv", title: "Show xprv", text: "For experts: the wallet's master extended private key, to export." },
  ];
  function toggle(what: Open) {
    hideSecrets();
    // Hide takes FreeBank's copy of the words off the clipboard too (v0.4.10 review); a hidden window doesn't, as
    // that is how a paste into a password manager goes.
    walletSeed.forgetCopiedWords().catch(() => {});
    open = open === what ? null : what;
    changeMsg = "";
    changeError = "";
    revealError = "";
    fileError = "";
  }

  // ---- Back up ----
  let backingUp = false;
  let saved: string[] = [];
  let backupError = "";
  async function backup() {
    backingUp = true;
    backupError = "";
    try {
      saved = await walletSeed.backupNow();
      await Promise.all([load(), loadProtection()]);
    } catch (e) {
      backupError = nice(e);
    }
    backingUp = false;
  }

  // ---- Change the passphrase ----
  let currentFields: PassphraseFields;
  let newFields: PassphraseFields;
  let current = "";
  let currentOk = false;
  let next = "";
  let nextOk = false;
  let changing = false;
  let changeMsg = "";
  let changeError = "";
  async function change() {
    if (!currentOk || !nextOk) return;
    changing = true;
    changeError = "";
    changeMsg = "";
    try {
      const r = await walletSeed.changePassphrase(current, next);
      currentFields?.clear();
      newFields?.clear();
      changeMsg =
        r.seed_file === "updated"
          ? "Your passphrase is changed. FreeBank's copy of your recovery words opens with the new one."
          : r.note ?? "Your passphrase is changed.";
      await load();
    } catch (e) {
      changeError = nice(e);
      currentFields?.clear();
    }
    changing = false;
  }

  // ---- Show the words or the xprv ----
  let revealFields: PassphraseFields;
  let revealPass = "";
  let revealOk = false;
  let revealing = false;
  let revealError = "";
  let shownWords: string[] = [];
  let shownXprv = "";
  let matches: boolean | null = null;
  async function reveal(what: "words" | "xprv") {
    if (!revealOk) return;
    revealing = true;
    revealError = "";
    try {
      const r = await walletSeed.reveal(revealPass, what);
      revealFields?.clear();
      matches = r.matches_wallet;
      if (what === "words") shownWords = r.words ?? [];
      else shownXprv = r.xprv ?? "";
    } catch (e) {
      revealError = nice(e);
      revealFields?.clear();
    }
    revealing = false;
  }
  function hideSecrets() {
    shownWords.fill("");
    shownWords = [];
    shownXprv = "";
    matches = null;
    wordsInput = "";
    wordsResult = null;
    addrFields?.clear();
  }

  // ---- Is this address mine? Check my words (v0.6.8) ----
  let addrInput = "";
  let addrFields: PassphraseFields;
  let addrPass = "";
  let addrPassOk = false;
  let addrResult: Checked | null = null;
  let addrError = "";
  let checkingAddr = false;
  // An eCash address is looked for among the ones the words make, which the passphrase opens.
  $: addrIsEcash = /^(bc1|tb1|bcrt1)/i.test(addrInput.trim());
  // A FreeBank address needs no passphrase: none is kept for it.
  $: if (!addrIsEcash) addrPass = "";
  async function checkAddress() {
    checkingAddr = true;
    addrError = "";
    addrResult = null;
    const asked = addrInput.trim();
    try {
      const r = await walletSeed.addressMine(asked, addrIsEcash ? addrPass : undefined);
      // Only for the address still in the box.
      if (addrInput.trim() === asked) addrResult = r;
    } catch (e) {
      addrError = nice(e);
    }
    addrFields?.clear();
    addrPass = "";
    checkingAddr = false;
  }
  let wordsInput = "";
  let wordsResult: Checked | null = null;
  let wordsError = "";
  let checkingWords = false;
  async function checkWords() {
    checkingWords = true;
    wordsError = "";
    wordsResult = null;
    const typed = wordsInput;
    // Off the screen as soon as they are sent.
    wordsInput = "";
    try {
      wordsResult = await walletSeed.wordsMatch(typed);
    } catch (e) {
      wordsError = nice(e);
    }
    checkingWords = false;
  }
  function onVisibility() {
    if (document.hidden) hideSecrets();
  }

  // ---- Restore from a backup file ----
  let fileInput: HTMLInputElement;
  let picked: (BackupFile & { name: string; modified: number }) | null = null;
  let checkingFile = false;
  let fileError = "";
  async function onFile() {
    const f = fileInput.files?.[0];
    fileInput.value = "";
    if (!f) return;
    hideSecrets();
    open = "file";
    fileError = "";
    picked = null;
    if (f.size > MAX_BACKUP_BYTES) {
      fileError = "That file is too big to be a FreeBank wallet backup.";
      return;
    }
    checkingFile = true;
    try {
      const r = await walletSeed.restoreFileCheck(new Uint8Array(await f.arrayBuffer()));
      picked = { ...r, name: f.name, modified: f.lastModified };
    } catch (e) {
      fileError = nice(e);
    }
    checkingFile = false;
  }
  function chooseFile() {
    hideSecrets();
    open = "file";
    fileError = "";
    fileInput?.click();
  }
  function restoreFile() {
    if (!picked) return;
    openWalletFlow({ kind: "restore-file", file: picked });
    picked = null;
    open = null;
  }

  // 1536 -> "1.5 KB"
  function size(n: number): string {
    if (n < 1024) return `${n} bytes`;
    const units = ["KB", "MB", "GB"];
    let v = n / 1024;
    let u = 0;
    while (v >= 1024 && u < units.length - 1) {
      v /= 1024;
      u++;
    }
    return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
  }

  $: seedWords = !info
    ? ""
    : info.app_seed === "matches"
      ? info.words_confirmed
        ? "Saved in FreeBank, checked"
        : "Saved in FreeBank, not checked yet"
      : info.app_seed === "other"
        ? "Saved, but for another seed"
        : "Not set up";
  $: unlockedNow = !!info && info.unlocked_until * 1000 > Date.now();
  // Restoring needs the node this app runs.
  $: external = !!info && !info.node_is_ours;

  onMount(() => {
    load();
    document.addEventListener("visibilitychange", onVisibility);
  });
  onDestroy(() => {
    document.removeEventListener("visibilitychange", onVisibility);
    hideSecrets();
    walletSeed.forgetCopiedWords().catch(() => {});
  });
  // The wallet got another seed (a flow elsewhere): ask again, once for each new seed.
  let reloadedFor: string | null = null;
  $: if ($protection && info && $protection.hd_seed_id !== info.hd_seed_id && $protection.hd_seed_id !== reloadedFor) {
    reloadedFor = $protection.hd_seed_id;
    load();
  }
</script>

<div class="card wallet-card">
  <!-- It acts on the main wallet whichever the header chose (the phone and the words' protection are the main one's). -->
  <h3>{$walletList.length > 1 ? "Main wallet" : "Wallet"}</h3>
  {#if $walletList.length > 1 && !$walletList.find((w) => w.active && w.name === null)}
    <p class="muted small">This is your main wallet, whichever one the header shows.</p>
  {/if}
  {#if loadError}
    <p class="soft-error">{loadError}</p>
  {:else if !info}
    <p class="muted small">Asking your node about the wallet…</p>
  {:else}
    <!-- What the owner acts on first; the technical rows (and empty amounts) behind More details (v0.2.7, the walk-through:
         the card was 11 rows). -->
    <dl class="facts">
      <div><dt>Passphrase</dt><dd>{info.encrypted ? "Set" : "Not set"}</dd></div>
      <div>
        <dt>Now</dt>
        <dd>{!info.encrypted ? "Open to anyone with the file" : unlockedNow ? `Unlocked until ${new Date(info.unlocked_until * 1000).toLocaleTimeString()}` : "Locked"}</dd>
      </div>
      <div><dt>Spendable</dt><dd>{fmtEcx(info.balance_sats)} {BASE_TICKER}</dd></div>
      {#if moreFacts || info.unconfirmed_sats !== 0}
        <div><dt>Unconfirmed</dt><dd>{fmtEcx(info.unconfirmed_sats)} {BASE_TICKER}</dd></div>
      {/if}
      {#if moreFacts || info.immature_sats !== 0}
        <div><dt>Newly mined</dt><dd>{fmtEcx(info.immature_sats)} {BASE_TICKER}</dd></div>
      {/if}
      <div><dt>Recovery words</dt><dd>{seedWords}</dd></div>
      <div><dt>Last backup</dt><dd>{info.backup_at ? when(info.backup_at) : "Never"}</dd></div>
      {#if moreFacts}
        <div><dt>Wallet file</dt><dd class="mono">{#if info.wallet_file}<PathText path={info.wallet_file} />{:else}{info.wallet_name}{/if}</dd></div>
        <div><dt>Transactions</dt><dd>{info.txcount.toLocaleString()}</dd></div>
        <div><dt>Key pool</dt><dd>{info.keypool.toLocaleString()} to receive, {info.keypool_change.toLocaleString()} for change</dd></div>
        <div><dt>HD seed id</dt><dd class="mono seed-id">{info.hd_seed_id ?? "none"}</dd></div>
      {/if}
    </dl>
    <button class="link-btn facts-more" on:click={() => (moreFacts = !moreFacts)} aria-expanded={moreFacts} type="button">
      {moreFacts ? "Fewer details" : "More details"}
    </button>
    {#if info.seed_file_problem}<p class="soft-error">{info.seed_file_problem}</p>{/if}

    <div class="wallet-actions">
      <div class="maint">
        <div class="maint-text">
          <strong>Back up</strong>
          <span class="muted small">A copy of the wallet file in your Documents folder (your home folder if there isn't one){info.encrypted ? ", locked with your passphrase" : ""}.</span>
        </div>
        <button class="secondary" on:click={backup} disabled={backingUp}>{backingUp ? "Backing up…" : "Back up"}</button>
      </div>
      {#each saved as s}<p class="hint">Saved to <span class="mono"><PathText path={s} /></span></p>{/each}
      {#if backupError}<p class="soft-error">{backupError}</p>{/if}

      {#if !info.encrypted}
        <div class="maint">
          <div class="maint-text">
            <strong>Set passphrase</strong>
            <span class="muted small">Protect the wallet and get your recovery words.</span>
          </div>
          <button on:click={() => openWalletFlow({ kind: "protect" })}>Protect…</button>
        </div>
      {:else}
        <div class="maint">
          <div class="maint-text">
            <strong>Change passphrase</strong>
            <span class="muted small">Your recovery words stay the same.</span>
          </div>
          <button class="secondary" on:click={() => toggle("change")} disabled={open === "change"}>Change…</button>
        </div>
        {#if open === "change"}
          <form class="confirm-box" on:submit|preventDefault={change}>
            <PassphraseFields bind:this={currentFields} bind:value={current} bind:valid={currentOk} twice={false} label="Current passphrase" />
            <PassphraseFields bind:this={newFields} bind:value={next} bind:valid={nextOk} label="New passphrase" autofocus={false} />
            {#if changeError}<p class="soft-error" role="alert">{changeError}</p>{/if}
            <div class="row-actions">
              <button type="submit" disabled={!currentOk || !nextOk || changing}>{changing ? "Changing…" : "Change passphrase"}</button>
              <button type="button" class="secondary" on:click={() => toggle(null)} disabled={changing}>Cancel</button>
            </div>
          </form>
        {/if}
        {#if changeMsg}<p class="hint ok-note">{changeMsg}</p>{/if}
      {/if}

      {#if info.app_seed !== "none"}
        <!-- Each row's box opens right under it. -->
        {#each reveals as r (r.which)}
          <div class="maint">
            <div class="maint-text">
              <strong>{r.title}</strong>
              <span class="muted small">{r.text}</span>
            </div>
            <button class="secondary" on:click={() => toggle(r.which)} disabled={open === r.which}>Show…</button>
          </div>
          {#if open === r.which}
            <div class="confirm-box">
              {#if open === "words" ? shownWords.length === 0 : !shownXprv}
                <form class="reveal-form" on:submit|preventDefault={() => reveal(open === "xprv" ? "xprv" : "words")}>
                  <PassphraseFields bind:this={revealFields} bind:value={revealPass} bind:valid={revealOk} twice={false} label="Your wallet's passphrase" />
                  {#if revealError}<p class="soft-error" role="alert">{revealError}</p>{/if}
                  <div class="row-actions">
                    <button type="submit" disabled={!revealOk || revealing}>{revealing ? "Opening…" : open === "words" ? "Show my words" : "Show the xprv"}</button>
                    <button type="button" class="secondary" on:click={() => toggle(null)} disabled={revealing}>Cancel</button>
                  </div>
                </form>
              {:else}
                {#if matches === false}
                  <p class="soft-error">These belong to another seed than your wallet has now.</p>
                {/if}
                {#if open === "words"}
                  <RecoveryWords words={shownWords} />
                  <p>Anyone who has these words can take your coins. Write them down; never type them into a website.</p>
                {:else}
                  <!-- svelte-ignore a11y-no-static-element-interactions -->
                  <div class="xprv mono" on:copy|preventDefault on:cut|preventDefault on:contextmenu|preventDefault>{shownXprv}</div>
                  <p>
                    The master extended private key freebankd derives from this wallet's HD seed, for export only: it can't
                    be used to restore a FreeBank wallet, but other tools can read this wallet's addresses from it
                    (m/0'/0'/k' to receive, m/0'/1'/k' for change). Anyone who has it can take your coins.
                  </p>
                {/if}
                <div class="row-actions">
                  <button class="secondary" on:click={() => toggle(null)}>Hide</button>
                </div>
              {/if}
            </div>
          {/if}
        {/each}

        <!-- v0.6.8: two checks that change nothing. -->
        <div class="maint">
          <div class="maint-text">
            <strong>Is this address mine?</strong>
            <span class="muted small">Paste a FreeBank or eCash address: FreeBank says whether this wallet's words make it.</span>
          </div>
          <button class="secondary" on:click={() => toggle("address")} disabled={open === "address"}>Check…</button>
        </div>
        {#if open === "address"}
          <div class="confirm-box">
            <form class="reveal-form" on:submit|preventDefault={checkAddress}>
              <label>
                Address
                <input
                  type="text"
                  bind:value={addrInput}
                  on:input={() => { addrResult = null; addrError = ""; }}
                  disabled={checkingAddr}
                  placeholder="X…, bc1… or tb1…"
                  autocomplete="off"
                  autocapitalize="off"
                  spellcheck="false"
                  data-testid="address-mine-input"
                />
              </label>
              {#if addrIsEcash}
                <!-- It appears as the address is typed: the focus stays in the address box. -->
                <PassphraseFields bind:this={addrFields} bind:value={addrPass} bind:valid={addrPassOk} twice={false} autofocus={false} label="Your wallet's passphrase" />
                <p class="hint">
                  An eCash address is looked for among the addresses your recovery words make, for your eCash wallet and
                  your bidding wallet. The passphrase opens FreeBank's copy of the words; nothing is shown or changed.
                </p>
              {/if}
              {#if addrResult}<p class="hint" class:ok-note={addrResult.yes} data-testid="address-mine-result">{addrResult.said}</p>{/if}
              {#if addrError}<p class="soft-error" role="alert">{addrError}</p>{/if}
              <div class="row-actions">
                <button type="submit" disabled={!addrInput.trim() || checkingAddr || (addrIsEcash && !addrPassOk)} data-testid="address-mine-check">
                  {checkingAddr ? "Looking…" : "Check"}
                </button>
                <button type="button" class="secondary" on:click={() => toggle(null)} disabled={checkingAddr}>Close</button>
              </div>
            </form>
          </div>
        {/if}

        <div class="maint">
          <div class="maint-text">
            <strong>Check my words</strong>
            <span class="muted small">Type in the {WORD_COUNT} words you wrote down: FreeBank says whether they are this wallet's. Nothing is restored or changed.</span>
          </div>
          <button class="secondary" on:click={() => toggle("check-words")} disabled={open === "check-words"}>Check…</button>
        </div>
        {#if open === "check-words"}
          <div class="confirm-box">
            <form class="reveal-form" on:submit|preventDefault={checkWords}>
              <label>
                Your {WORD_COUNT} words, in order
                <textarea
                  rows="4"
                  bind:value={wordsInput}
                  on:input={() => { wordsResult = null; wordsError = ""; }}
                  autocomplete="off"
                  autocapitalize="off"
                  spellcheck="false"
                  data-testid="check-words-input"
                ></textarea>
              </label>
              <p class="hint">Type them only here, never into a website. They leave the screen as soon as you press Check.</p>
              {#if wordsResult}
                <p class={wordsResult.yes ? "hint ok-note" : "soft-error"} data-testid="check-words-result">{wordsResult.said}</p>
              {/if}
              {#if wordsError}<p class="soft-error" role="alert">{wordsError}</p>{/if}
              <div class="row-actions">
                <button type="submit" disabled={!wordsInput.trim() || checkingWords} data-testid="check-words-check">{checkingWords ? "Checking…" : "Check"}</button>
                <button type="button" class="secondary" on:click={() => toggle(null)} disabled={checkingWords}>Close</button>
              </div>
            </form>
          </div>
        {/if}
      {/if}

      {#if plan && plan.coins > 0}
        <div class="maint">
          <div class="maint-text">
            <strong>Move my coins to the new words</strong>
            <span class="muted small">{fmtEcx(plan.total_sats)} {BASE_TICKER} sits on addresses your recovery words don't cover.</span>
          </div>
          <button on:click={() => openWalletFlow({ kind: "move" })}>Move…</button>
        </div>
      {/if}

      <div class="maint">
        <div class="maint-text">
          <strong>Restore from a backup file</strong>
          <span class="muted small">Put a wallet backup in this wallet's place. The current wallet is kept.</span>
        </div>
        <button class="secondary" on:click={chooseFile} disabled={checkingFile || external}>
          {checkingFile ? "Checking…" : "Choose file…"}
        </button>
      </div>
      <input bind:this={fileInput} type="file" class="file-input" on:change={onFile} tabindex="-1" aria-hidden="true" />
      {#if fileError}<p class="soft-error" role="alert">{fileError}</p>{/if}
      {#if picked && open === "file"}
        <div class="confirm-box">
          <p>
            <strong class="picked-name">{picked.name}</strong>
            A wallet backup, {size(picked.size)}{picked.modified ? `, saved ${new Date(picked.modified).toLocaleString()}` : ""}.
            {picked.encrypted === true ? "It has a passphrase." : picked.encrypted === false ? "It has no passphrase." : ""}
          </p>
          <p>
            FreeBank stops your node, moves your current wallet aside (it is kept, never deleted), puts this backup in
            its place, starts the node again and looks through the chain for the backup's coins.
          </p>
          <div class="row-actions">
            <button on:click={restoreFile}>Restore this backup</button>
            <button class="secondary" on:click={() => { picked = null; open = null; }}>Cancel</button>
          </div>
        </div>
      {/if}

      <div class="maint">
        <div class="maint-text">
          <strong>Restore from recovery words</strong>
          <span class="muted small">A new wallet from your 24 words. The current wallet is kept.</span>
        </div>
        <button class="secondary" on:click={() => openWalletFlow({ kind: "restore-words" })} disabled={external}>Restore…</button>
      </div>
      {#if external}
        <p class="hint">Your node was started by another program, so FreeBank can't restore into it. Stop it there first.</p>
      {/if}
    </div>

    <p class="hint coverage">
      Your {WORD_COUNT} recovery words cover this wallet and the app's eCash wallets, every address made from them (not keys
      added by hand).
      {#if coverageMore}
        Other wallets can't read these addresses from the words directly, but a BIP85 tool can rebuild them from the
        words. Its "WIF" at index 0 (the HD-Seed WIF application, <span class="mono">{info.bip85_path}</span>) is this
        wallet's seed, which a FreeBank node takes with <span class="mono">sethdseed</span>. Its "XPRV" at index 0 is the
        eCash wallets' own key: under it they are standard BIP84 wallets (accounts 0 and 1), which any BIP84 wallet
        connected to eCash finds. (The eCash keys aren't those of a Bitcoin wallet made from the same words: on purpose,
        so that a Bitcoin wallet's words typed in here can't put its keys at risk.)
      {:else}
        <button class="link-btn inline" on:click={() => (coverageMore = true)}>More</button>
      {/if}
    </p>
  {/if}
</div>

<style>
  .wallet-card .facts {
    margin-top: 4px;
  }
  .seed-id {
    font-size: 11.5px;
  }
  .wallet-actions {
    margin-top: 16px;
    border-top: 1px solid var(--border-color);
  }
  .wallet-actions .maint:first-of-type {
    border-top: none;
    padding-top: 12px;
  }
  .reveal-form {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .xprv {
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    color: var(--text-color);
    font-size: 12.5px;
    overflow-wrap: anywhere;
    user-select: none;
    -webkit-user-select: none;
  }
  .file-input {
    display: none;
  }
  .picked-name {
    display: block;
    color: var(--text-color);
    overflow-wrap: anywhere;
  }
  .coverage {
    margin-top: 14px;
  }
</style>
