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

  type Open = "change" | "words" | "xprv" | "file" | null;
  let open: Open = null;
  const reveals: { which: "words" | "xprv"; title: string; text: string }[] = [
    { which: "words", title: "Show recovery words", text: `The ${WORD_COUNT} words FreeBank keeps, locked with your passphrase.` },
    { which: "xprv", title: "Show xprv", text: "For experts: the wallet's master extended private key, to export." },
  ];
  function toggle(what: Open) {
    hideSecrets();
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
        The eCash wallets are standard BIP84 wallets (accounts 0 and 1), so any BIP84 wallet finds them from the words.
        Other wallets can't read FreeBank's addresses from the words directly, but a BIP85 tool can rebuild this wallet
        from them: its "WIF" for these words at index 0 (the HD-Seed WIF application,
        <span class="mono">{info.bip85_path}</span>) is this wallet's seed, which a FreeBank node takes with
        <span class="mono">sethdseed</span>.
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
