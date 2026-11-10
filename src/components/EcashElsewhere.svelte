<script lang="ts">
  // "Use this wallet in Sparrow or Electrum" (v0.7.3). The main eCash wallet's keys come from the recovery words through
  // BIP85 (ecash/keys.rs), so the words alone open other addresses in those wallets. This card gives them the wallet
  // itself instead: watch-only from its zpub (or Sparrow's descriptor), and, behind the passphrase, the private keys
  // to spend from there too. The private keys can't be copied (as Wallet settings' xprv): type them or scan the QR.
  // Point them at eCash, never at Bitcoin: eCash keeps Bitcoin's address formats and replays its payments.
  import { onDestroy } from "svelte";
  import QrCode from "./QrCode.svelte";
  import PassphraseFields from "./PassphraseFields.svelte";
  import { nice } from "../lib/errors";
  import { ecashExport, ecashExportPrivate, type EcashExport } from "../lib/ecash";

  let open = false;
  let ex: EcashExport | null = null;
  let error = "";
  let copied = "";
  let qr = "";

  async function show() {
    open = !open;
    error = "";
    if (!open) return hide();
    try {
      ex = await ecashExport();
    } catch (e) {
      error = nice(e);
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = text;
      setTimeout(() => (copied = copied === text ? "" : copied), 1500);
    } catch {
      error = "Couldn't copy it; select it and copy it by hand.";
    }
  }

  // ---- The private keys ----
  let fields: PassphraseFields;
  let pass = "";
  let passOk = false;
  let asking = false;
  let opening = false;
  let master = "";
  let zprv = "";
  async function reveal() {
    if (!passOk) return;
    opening = true;
    error = "";
    try {
      const s = await ecashExportPrivate(pass);
      fields?.clear();
      master = s.master_xprv;
      zprv = s.zprv;
    } catch (e) {
      error = nice(e);
      fields?.clear();
    }
    opening = false;
  }
  function hide() {
    master = "";
    zprv = "";
    qr = "";
    asking = false;
    fields?.clear();
  }
  onDestroy(hide);
  // A hidden window hides the private keys, as Wallet settings' xprv (WalletSettings.svelte).
  function onVisibility() {
    if (document.hidden) hide();
  }
  document.addEventListener("visibilitychange", onVisibility);
  onDestroy(() => document.removeEventListener("visibilitychange", onVisibility));

  $: testnet = ex?.chain === "test";
  $: regtest = ex?.chain === "regtest";
  // A node on another computer: Sparrow can't read its cookie or data folder, so it needs a login the node accepts.
  $: remote = !!ex?.node && !ex.node_dir && !/^(127\.|localhost[:]|\[::1\])/.test(ex.node);
  $: rows = ex
    ? [
        { label: "Master fingerprint", value: ex.fingerprint },
        { label: "Derivation", value: ex.path },
        { label: testnet ? "vpub" : regtest ? "Extended public key" : "zpub", value: regtest ? ex.xpub : ex.zpub },
      ]
    : [];
</script>

<div class="card" data-testid="ecash-elsewhere">
  <div class="ee-head">
    <h3>Use this wallet in Sparrow or Electrum</h3>
    <button class="link-btn" on:click={show} data-testid="ecash-elsewhere-open">{open ? "Hide" : "Show how"}</button>
  </div>
  {#if !open}
    <p class="muted small">
      Your recovery words open other addresses in those wallets, so give them this wallet's key instead: to watch it, or
      to spend from there too.
    </p>
  {:else if ex}
    <p class="small ee-warn" data-testid="ecash-elsewhere-warn">
      Point them at your eCash node, never at a Bitcoin one. eCash addresses look like Bitcoin's and many payments made
      on one chain can be repeated on the other, so send eCash from FreeBank where you can: it marks each payment so it
      can't be repeated on Bitcoin, as Electrum for eCash does; Sparrow doesn't.
    </p>
    <h4>First, set Sparrow up for eCash</h4>
    <p class="small">Before you make the wallet there, so Sparrow never asks a Bitcoin server about its addresses:</p>
    <ol class="small ee-steps" data-testid="ecash-elsewhere-sparrow">
      {#if testnet}
        <li>easynet uses Bitcoin's test prefixes (tb1…): Tools › Restart In › <em>Testnet</em>.</li>
      {:else if regtest}
        <li>This is a regtest wallet: Tools › Restart In › <em>Regtest</em>.</li>
      {/if}
      <li>
        File › Settings › General: <em>Fee rates source</em> Server, <em>Block explorer</em> None and <em>Exchange rate
        source</em> None. Sparrow calls the coins BTC; left as it is, it shows Bitcoin's fees and prices your eCash as
        bitcoin.
      </li>
      <li>
        File › Settings › Server: <em>Bitcoin Core</em>{#if ex.node}, URL and port <span class="mono">{ex.node}</span>{/if}.
        {#if remote}
          Authentication <em>User / Pass</em>, with a login that node accepts.
        {:else if testnet || regtest}
          Sparrow looks for the login in a folder of its own, so choose <em>User / Pass</em>, with the two parts of the
          <span class="mono">.cookie</span> file in {#if ex.node_dir}<span class="mono">{ex.node_dir}</span>{:else}the eCash node's data folder{/if}
          (user:password; it changes when the node restarts).
        {:else}
          Authentication <em>Default</em>, data folder {#if ex.node_dir}<span class="mono">{ex.node_dir}</span>{:else}your eCash
            node's (BitWindow's, unless you run your own){/if}.
        {/if}
      </li>
    </ol>
    {#if testnet}
      <p class="muted small" data-testid="ecash-elsewhere-electrum">
        Electrum can't follow this test network, Electrum for eCash included: use Sparrow here.
      </p>
    {:else}
      <p class="muted small" data-testid="ecash-elsewhere-electrum">
        Stock Electrum can't follow eCash; <strong>Electrum for eCash</strong> (github.com/ecash-com/electrum-ecash) can. It
        is a separate project, not part of FreeBank, and its releases carry checksums, not signatures. Electrum servers for
        eCash: explorer.ecash.ninja. Any server you use sees your IP address and the addresses your wallet looks up.
      </p>
    {/if}


    <h4>To watch it (can't spend)</h4>
    <dl class="ee-keys">
      {#each rows as r (r.label)}
        <div>
          <dt>{r.label}</dt>
          <dd><span class="mono">{r.value}</span> <button class="link-btn small" on:click={() => copy(r.value)}>{copied === r.value ? "Copied" : "Copy"}</button></dd>
        </div>
      {/each}
    </dl>
    <ul class="small ee-steps">
      <li>
        <strong>Sparrow:</strong> File › New Wallet, then <em>xPub / Watch Only Wallet</em>, script type
        <em>Native Segwit (P2WPKH)</em>, with the fingerprint, derivation and key above. Or File › Import Wallet ›
        <em>Output Descriptor</em>:
        <span class="mono ee-desc">{ex.descriptor}</span>
        <button class="link-btn small" on:click={() => copy(ex?.descriptor ?? "")}>{copied === ex.descriptor ? "Copied" : "Copy"}</button>
      </li>
      {#if !testnet}
        <li>
          <strong>Electrum for eCash:</strong> New wallet › Standard wallet › <em>Use a master key</em>, with the zpub.
        </li>
      {/if}
    </ul>
    <p class="small">
      Check: its first receiving address should be <span class="mono" data-testid="ecash-elsewhere-first">{ex.first_address}</span>.
      Whoever has this key sees every address and payment of this wallet, but can't spend.
    </p>

    <h4>To spend from there too</h4>
    {#if !master}
      {#if !asking}
        <button class="secondary" on:click={() => (asking = true)} data-testid="ecash-elsewhere-private">Show the private key…</button>
      {:else}
        <form class="ee-form" on:submit|preventDefault={reveal}>
          <PassphraseFields bind:this={fields} bind:value={pass} bind:valid={passOk} twice={false} label="Your wallet's passphrase" />
          <div class="row-actions">
            <button type="submit" disabled={!passOk || opening}>{opening ? "Opening…" : "Show the private key"}</button>
            <button type="button" class="secondary" on:click={hide} disabled={opening}>Cancel</button>
          </div>
        </form>
      {/if}
    {:else}
      <p class="small">
        <strong>Sparrow:</strong> File › New Wallet, then <em>Master Private Key (BIP32)</em>, script type
        <em>Native Segwit (P2WPKH)</em>, derivation <span class="mono">{ex.path}</span>:
      </p>
      <!-- svelte-ignore a11y-no-static-element-interactions -->
      <div class="ee-secret mono" on:copy|preventDefault on:cut|preventDefault on:contextmenu|preventDefault data-testid="ecash-elsewhere-master">{master}</div>
      <button class="link-btn small" on:click={() => (qr = qr === master ? "" : master)}>{qr === master ? "Hide the QR" : "Show as a QR"}</button>
      {#if qr && qr === master}<div class="ee-qr"><QrCode text={qr} size={220} /></div>{/if}
      {#if !testnet}
        <p class="small"><strong>Electrum for eCash:</strong> New wallet › Standard wallet › <em>Use a master key</em>:</p>
        <!-- svelte-ignore a11y-no-static-element-interactions -->
        <div class="ee-secret mono" on:copy|preventDefault on:cut|preventDefault on:contextmenu|preventDefault data-testid="ecash-elsewhere-zprv">{zprv}</div>
        <button class="link-btn small" on:click={() => (qr = qr === zprv ? "" : zprv)}>{qr === zprv ? "Hide the QR" : "Show as a QR"}</button>
        {#if qr && qr === zprv}<div class="ee-qr"><QrCode text={qr} size={220} /></div>{/if}
      {/if}
      <p class="small">
        Anyone who has either key can spend this eCash wallet from anywhere. They can't be copied here: type them, or scan
        the QR with Sparrow's or Electrum's camera. They cover this eCash wallet and the bidding wallet, not your FreeBank
        wallet.
      </p>
      <button class="secondary" on:click={hide}>Hide</button>
    {/if}
  {/if}
  {#if error}<p class="soft-error" role="alert">{error}</p>{/if}
</div>

<style>
  .ee-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .ee-head h3 {
    flex: 1;
  }
  .ee-head button {
    white-space: nowrap;
  }
  p.small,
  .ee-steps {
    margin: 8px 0;
  }
  h4 {
    margin: 16px 0 6px;
  }
  .ee-warn {
    border-left: 3px solid var(--warning-color, #c58b00);
    padding-left: 10px;
  }
  .ee-keys {
    display: grid;
    gap: 6px;
    margin: 8px 0;
  }
  .ee-keys dt {
    font-size: 12px;
    color: var(--text-muted);
  }
  .ee-keys dd {
    margin: 0;
    overflow-wrap: anywhere;
    font-size: 12.5px;
  }
  .ee-steps {
    padding-left: 18px;
  }
  .ee-steps li {
    margin-bottom: 6px;
  }
  .ee-steps .mono,
  p .mono {
    overflow-wrap: anywhere;
  }
  .ee-desc {
    display: block;
    overflow-wrap: anywhere;
    font-size: 12px;
    margin-top: 4px;
  }
  .ee-secret {
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
  .ee-qr {
    margin: 10px 0;
  }
  .ee-form {
    max-width: 420px;
  }
</style>
