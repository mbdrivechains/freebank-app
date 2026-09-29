<script lang="ts">
  // Settings for the node on this computer: its connection (Advanced, with Test connection), what
  // happens to it when the app closes (KeepRunning), and three deliberate actions, each of which
  // asks first: "Delete chain data", "Remove FreeBank", and "Obliterate: remove everything".
  // Obliterate lists what would go (worked out by the app, with sizes; a node folder is ticked only
  // if FreeBank created it), shows the wallet's balance when it can be trusted, offers a backup (a
  // backup covers only the wallet it copies), and wants OBLITERATE typed before its button works.
  // Each tick goes back with the path it showed, and the connection can't be changed while the list
  // is open. Also reachable from setup (the header's gear), for someone stuck there.
  import { createEventDispatcher, onMount } from "svelte";
  import AdvancedSettings from "./AdvancedSettings.svelte";
  import KeepRunning from "./KeepRunning.svelte";
  import PathText from "./PathText.svelte";
  import { BASE_TICKER } from "../lib/brand";
  import {
    node,
    type NodeStatus,
    type ObliteratePlan,
    type Obliterated,
    type Removed,
    type SetupInfo,
    type WipeItem,
  } from "../lib/node";

  const dispatch = createEventDispatcher<{ removed: Removed; obliterated: Obliterated }>();

  let info: SetupInfo | null = null;
  let st: NodeStatus | null = null;
  let loadError = "";

  async function load() {
    try {
      [info, st] = await Promise.all([node.setupInfo(), node.status()]);
      loadError = "";
    } catch (e) {
      loadError = String(e);
    }
  }
  onMount(load);

  let confirm: "wipe" | "remove" | "obliterate" | null = null;

  $: external = !!st && !st.managed && st.state !== "down" && st.state !== "busy";
  // While the Obliterate list is open, the data folder it names must stay the one shown.
  $: lockedReason =
    confirm === "obliterate"
      ? "Close the Obliterate list below to change these."
      : st?.managed
        ? "Stop the node on the Node tab to change these."
        : external
          ? "A FreeBank node started by another program is running; stop it there to change these."
          : "";
  let busy = false;
  let error = "";
  let done = "";

  async function wipe() {
    busy = true;
    error = "";
    try {
      await node.deleteChainData();
      done = "Chain data deleted. Your node is starting again and will download the chain from its peers; the Node tab shows how far it has got.";
      confirm = null;
    } catch (e) {
      error = String(e);
    }
    busy = false;
    load();
  }

  async function remove() {
    busy = true;
    error = "";
    try {
      const r = await node.removePrograms();
      confirm = null;
      dispatch("removed", r);
    } catch (e) {
      error = String(e);
    }
    busy = false;
  }

  // Obliterate. The app works out the list; the screen sends back the ticked ids, each with the
  // path it showed. Ticks are kept per line and path, so a line that comes back naming another
  // folder starts as the app suggests.
  let plan: ObliteratePlan | null = null;
  let ticked: Record<string, boolean> = {};
  const key = (i: WipeItem) => `${i.id}\n${i.path}`;
  let typed = "";
  let backingUp = false;
  let backupError = "";

  async function loadPlan(fresh: boolean) {
    try {
      const p = await node.obliteratePlan();
      // Keep what the user ticked; anything new starts as the app suggests.
      ticked = Object.fromEntries(
        p.items.map((i) => {
          const k = key(i);
          const keep = !fresh && k in ticked;
          return [k, i.allowed && (keep ? ticked[k] : i.checked)];
        }),
      );
      plan = p;
    } catch (e) {
      error = String(e);
    }
  }

  function openObliterate() {
    confirm = "obliterate";
    error = "";
    done = "";
    typed = "";
    backupError = "";
    plan = null;
    loadPlan(true);
  }

  $: chosen = plan ? plan.items.filter((i) => i.allowed && ticked[key(i)]) : [];
  // Wallets live in node folders and in folders FreeBank moved aside (during setup or a restore).
  $: walletCount = plan ? plan.items.reduce((n, i) => n + i.wallets.length, 0) : 0;
  $: nodeWallets = chosen.filter((i) => i.kind === "node").flatMap((i) => i.wallets);
  $: otherItems = chosen.filter((i) => i.kind !== "node" && i.wallets.length > 0);
  $: otherWallets = otherItems.flatMap((i) => i.wallets);
  $: nodeWalletGoes = nodeWallets.length > 0;
  $: otherWalletGoes = otherWallets.length > 0;
  $: walletGoes = nodeWalletGoes || otherWalletGoes;
  // The node says the wallet holds nothing, not even coins still on their way. (It says so only with
  // one wallet and the node caught up; otherwise there is no balance.)
  $: nodeEmpty = nodeWalletGoes && plan?.balance === 0 && !plan?.pending;
  // A backup covers only the wallet it copies: one of another folder's wallet covers nothing here.
  $: backups = plan?.backups ?? [];
  $: uncovered = [...(nodeEmpty ? [] : nodeWallets), ...otherWallets].filter((w) => !backups.some((b) => b.wallet === w));
  $: someCovered = backups.some((b) => nodeWallets.includes(b.wallet) || otherWallets.includes(b.wallet));
  $: backedUp = walletGoes && uncovered.length === 0 && someCovered;
  // Nothing at stake: the wallet that goes is empty and no other wallet goes with it. The box is
  // then plain, not red, and backing up is offered without urging.
  $: calm = walletGoes && nodeEmpty && !otherWalletGoes;
  $: others = otherItems.every((i) => i.kind === "aside")
    ? otherWallets.length === 1
      ? "the old wallet in the folder FreeBank moved aside"
      : "the old wallets in the folders FreeBank moved aside"
    : otherItems.every((i) => i.kind === "earlier")
      ? otherWallets.length === 1
        ? "the wallet in the earlier data folder"
        : "the wallets in the earlier data folders"
      : "the wallets in the other folders ticked above";
  $: walletHeading =
    nodeWalletGoes && otherWalletGoes
      ? `Your wallet will be deleted, and so will ${others}.`
      : nodeEmpty
        ? "Your wallet is empty. It will be deleted."
        : nodeWalletGoes
          ? "Your wallet will be deleted."
          : `${others[0].toUpperCase()}${others.slice(1)} will be deleted.`;
  // The recovery words restore the node's own wallet, not the older ones in other folders.
  $: orWords = plan?.seed && nodeWalletGoes && !nodeEmpty && !otherWalletGoes ? " or your recovery words" : "";
  // Once every wallet that goes is backed up, the backup is what the box says instead.
  $: lossWarning =
    backedUp || uncovered.length === 0
      ? ""
      : someCovered
        ? "Your backup doesn't cover every wallet that goes: without one, any coins in the others are gone for good."
        : nodeWalletGoes && !otherWalletGoes && plan?.balance !== null
          ? `Without a backup${orWords}, those coins are gone for good.`
          : `Without a backup${orWords}, any coins in ${uncovered.length > 1 ? "them" : "it"} are gone for good.`;
  // The app's copy of the recovery words goes with its own folder.
  $: seedGoes = !!plan?.seed && chosen.some((i) => i.kind === "app");
  // ".freebank/wallet.dat": enough to tell apart wallets of the same name in different folders.
  const walletName = (path: string) => path.split("/").slice(-2).join("/");

  async function backup() {
    backingUp = true;
    backupError = "";
    try {
      await node.walletBackup();
      await loadPlan(false);
    } catch (e) {
      backupError = String(e);
    }
    backingUp = false;
  }

  async function obliterate() {
    busy = true;
    error = "";
    try {
      const r = await node.obliterate(chosen.map((i) => ({ id: i.id, path: i.path })));
      confirm = null;
      dispatch("obliterated", r);
    } catch (e) {
      error = String(e);
      loadPlan(false);
    }
    busy = false;
  }

  // 1536 -> "1.5 KB"
  function size(n: number): string {
    if (n < 1024) return `${n} bytes`;
    const units = ["KB", "MB", "GB", "TB"];
    let v = n / 1024;
    let u = 0;
    while (v >= 1024 && u < units.length - 1) {
      v /= 1024;
      u++;
    }
    return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
  }
</script>

{#if loadError}
  <p class="soft-error">{loadError}</p>
{/if}

{#if info && st}
  <KeepRunning keepRunning={info.settings.keep_running} />

  <div class="card">
    <h3>Connection</h3>
    <p class="muted small">Where your FreeBank node finds eCash beta, and where it keeps its data.</p>
    <div class="settings-adv">
      <AdvancedSettings
        settings={info.settings}
        defaultDatadir={info.default_datadir}
        open
        {lockedReason}
        saveLabel="Save"
        on:saved={load}
      />
    </div>
    <p class="hint">Saved changes take effect the next time FreeBank starts.</p>
  </div>

  <div class="card">
    <h3>Start over</h3>

    <div class="maint">
      <div class="maint-text">
        <strong>Delete chain data</strong>
        <span class="muted small">Removes the downloaded blocks and chain state, then downloads them again. Your wallet stays.</span>
      </div>
      <button class="secondary" on:click={() => { confirm = "wipe"; error = ""; done = ""; }} disabled={busy || confirm === "wipe"}>Delete…</button>
    </div>
    {#if confirm === "wipe"}
      <div class="confirm-box">
        <p>
          This stops your node and deletes <code>blocks</code>, <code>chainstate</code> and <code>indexes</code> in
          <span class="path"><PathText path={st.datadir} /></span>
          Your wallet (<code>wallet.dat</code>) and <code>freebank.conf</code> stay. Your node then starts again and re-syncs,
          which takes a while.
        </p>
        {#if external}<p class="hint">A node started by another program is running; stop it there first.</p>{/if}
        <div class="row-actions">
          <button on:click={wipe} disabled={busy || external}>{busy ? "Deleting…" : "Delete and re-sync"}</button>
          <button class="secondary" on:click={() => (confirm = null)} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}
    {#if done}<p class="hint ok-note">{done}</p>{/if}

    <div class="maint">
      <div class="maint-text">
        <strong>Remove FreeBank</strong>
        <span class="muted small">Stops the node and removes the programs this app downloaded. Your data folder and wallet stay.</span>
      </div>
      <button class="secondary" on:click={() => { confirm = "remove"; error = ""; done = ""; }} disabled={busy || confirm === "remove"}>Remove…</button>
    </div>
    {#if confirm === "remove"}
      <div class="confirm-box">
        <p>
          This stops your node and removes the FreeBank node program and grpcurl from the app's own folder.
          Nothing in <span class="path"><PathText path={st.datadir} /></span> is touched, so your wallet and settings stay.
          You can install FreeBank again at any time.
        </p>
        <div class="row-actions">
          <button on:click={remove} disabled={busy}>{busy ? "Removing…" : "Remove"}</button>
          <button class="secondary" on:click={() => (confirm = null)} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}

    <div class="maint">
      <div class="maint-text">
        <strong>Obliterate: remove everything</strong>
        <span class="muted small">Removes everything FreeBank put on this computer, your wallet included. It can't be undone.</span>
      </div>
      <button class="secondary danger-text" on:click={openObliterate} disabled={busy || confirm === "obliterate"}>Obliterate…</button>
    </div>
    {#if confirm === "obliterate"}
      <div class="confirm-box">
        {#if !plan}
          {#if !error}<p>Looking at what FreeBank put on this computer…</p>{/if}
          <div class="row-actions">
            <button class="secondary" on:click={() => (confirm = null)}>Cancel</button>
          </div>
        {:else}
          <p>
            This stops your node and deletes everything ticked below, for good. The eCash node, the enforcer and
            BitWindow are not touched.
          </p>
          <ul class="wipe-list">
            {#each plan.items as item (item.id)}
              <li>
                <label class:off={!item.allowed}>
                  <input type="checkbox" bind:checked={ticked[key(item)]} disabled={!item.allowed || busy} />
                  <span class="wipe-text">
                    <span class="wipe-head"><strong>{item.label}</strong><span class="wipe-size">{size(item.size)}</span></span>
                    <span class="path"><PathText path={item.path} /></span>
                    {#if item.note}<span class="wipe-note">{item.note}</span>{/if}
                  </span>
                </label>
              </li>
            {:else}
              <li>FreeBank has nothing left on this computer.</li>
            {/each}
          </ul>

          {#if walletCount}
            <div class="wallet-warn" class:kept={!walletGoes} class:calm>
              {#if walletGoes}
                <strong>{walletHeading}</strong>
                {#if nodeEmpty}
                  {#if otherWalletGoes}<span>Your wallet is empty.{lossWarning ? ` ${lossWarning}` : ""}</span>{/if}
                {:else if nodeWalletGoes && plan.balance !== null}
                  <span>
                    Your wallet holds {plan.balance.toFixed(8)} {BASE_TICKER}{plan.pending ? `, ${plan.pending.toFixed(8)} of it not spendable yet` : ""}.{lossWarning ? ` ${lossWarning}` : ""}
                  </span>
                {:else if lossWarning || (nodeWalletGoes && plan.balance_note)}
                  <span>
                    {[lossWarning, nodeWalletGoes ? plan.balance_note : ""].filter(Boolean).join(" ")}
                  </span>
                {/if}
              {:else}
                <span>{walletCount > 1 ? "Every wallet stays" : "Your wallet stays"}: nothing ticked holds one.</span>
              {/if}
              {#each plan.backups as b}
                <span>Backed up {walletName(b.wallet)} to <span class="path"><PathText path={b.saved} /></span></span>
              {/each}
              <button class="secondary" on:click={backup} disabled={backingUp || busy}>
                {backingUp
                  ? "Backing up…"
                  : backedUp
                    ? "Back up again"
                    : `Back up ${walletCount > 1 ? "wallets" : "wallet"}${walletGoes && !calm ? " first" : ""}`}
              </button>
              {#if plan.backup_note}<span class="wipe-note">{plan.backup_note}</span>{/if}
              {#if backupError}<span class="soft-error">{backupError}</span>{/if}
            </div>
          {/if}
          {#if seedGoes}
            <div class="wallet-warn">
              <strong>Your recovery words go too.</strong>
              <span>
                FreeBank keeps them encrypted in its own folder, which is ticked. Make sure you have them written down: with
                them you can restore your wallet in FreeBank on any computer.
              </span>
            </div>
          {/if}

          {#if plan.blocked}
            <p class="hint">{plan.blocked} <button class="link-btn" on:click={() => loadPlan(false)}>Check again</button></p>
          {/if}
          <label class="type-confirm">
            <span>Type <code>OBLITERATE</code> to confirm</span>
            <input type="text" bind:value={typed} autocomplete="off" autocapitalize="off" spellcheck="false" disabled={busy} />
          </label>
          <div class="row-actions">
            <button
              class="danger"
              on:click={obliterate}
              disabled={busy || typed !== "OBLITERATE" || chosen.length === 0 || !!plan.blocked}
            >{busy ? "Removing…" : "Obliterate"}</button>
            <button class="secondary" on:click={() => (confirm = null)} disabled={busy}>Cancel</button>
          </div>
        {/if}
      </div>
    {/if}

    {#if error}<p class="soft-error">{error}</p>{/if}
  </div>
{/if}
