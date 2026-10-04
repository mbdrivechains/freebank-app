# Verifying a FreeBank app release

This page is for anyone, or anyone's AI assistant, who wants to answer two questions before running the FreeBank
app. The app keeps your 24 recovery words (encrypted) and runs the node that holds your coins, so both matter:

1. **Is the source safe?** Does it contain exploits, backdoors, phone-home code, or a way to leak your recovery words
   or steer your coins?
2. **Was the package you downloaded built from that source,** and from nothing else?

For the second, you can check that GitHub built each package from the public tag, and that the maintainer published
it. From v0.2.3 you can also rebuild the Linux package yourself and get the same bytes. The first question can only be
answered by reviewing the code. This page shows how to do these, and what none of them proves.

## What you can and cannot check

| Claim | How you check it | Holds for |
|---|---|---|
| The source at the tag is what you reviewed | `git checkout <tag>` | every release |
| The package is the one GitHub built from the public tag | GitHub attestation (step 3) | v0.2.0 and later |
| The files are the ones the maintainer published | signed `SHA256SUMS` (step 3) | v0.2.0 and later |
| The phone page at app.ecxfreebank.com is its public source, built | rebuild it and compare (freebank-phone's README) | since 2026-10-01 |
| Your own build of the Linux program and `.deb` gives the same bytes | rebuild it (step 4) | v0.2.3 and later |
| Your own build of the AppImage or the Mac app gives the same bytes | not yet possible | |
| The code is free of exploits | nobody can prove this; review reduces the risk (step 2) | |

FreeBank is **experimental, pre-audit software**. A clean review, by an AI or a person, is evidence, not a
guarantee.

## Step 1: get the source at the release tag

```sh
git clone https://github.com/mbdrivechains/freebank-app
cd freebank-app
git checkout v0.2.2             # the release you are checking (this page first shipped in v0.2.2)
git rev-parse HEAD              # note the commit
```

Since v0.1.1, each release is one commit on `main`, so `git diff v0.2.1 v0.2.2` shows everything a release changed.

## Step 2: review the source

FreeBank is a [Tauri 2](https://tauri.app) desktop app. The Rust side in `src-tauri/src/` does everything that
touches keys, files, processes and the network. The screens in `src/` (Svelte) ask it through Tauri commands. The app
runs your own FreeBank node (`freebankd`) and talks to it over JSON-RPC. Below, `<app data>` is
`~/.local/share/com.ecxfreebank.freebank` on Linux and `~/Library/Application Support/com.ecxfreebank.freebank` on
macOS.

**Where the secrets are:**
- **The 24 recovery words:** `src-tauri/src/seed.rs`. They are standard BIP39 words. Their entropy is kept at
  `<app data>/wallet/seed.enc`, encrypted with the wallet passphrase (Argon2id, then XChaCha20-Poly1305). The
  passphrase itself is never stored. The node's wallet key comes from the words by BIP85 (the HD-Seed WIF
  application, index 0, in `seed.rs`) and goes to the node with `sethdseed` (`src-tauri/src/recovery/job.rs`).
- **The passphrase:** `src-tauri/src/wallet.rs` unlocks and locks the node's wallet. Every screen that signs goes
  through `withUnlock` (`src/lib/wallet.ts`): it asks in `UnlockPrompt.svelte`, unlocks for 30 seconds, then locks
  again. `src-tauri/src/recovery/` sets it (`encryptwallet`), changes it (`walletpassphrasechange`), opens `seed.enc`
  with it to show the words or the xprv, and restores from the words or a backup file.
- **The node's RPC login:** its `.cookie` file, or `rpcuser` and `rpcpassword` from `freebank.conf`
  (`src-tauri/src/node/detect.rs`), or the password typed in the connect form. It is kept in memory only
  (`src-tauri/src/rpc/`) and sent to the node as HTTP Basic auth.
- **Phone sends:** with "Let my phone send" on, `src-tauri/src/phone/mod.rs` keeps the passphrase in memory while the
  app is open. `src-tauri/src/phone/background.rs` hands it over a pipe to the background part that keeps your phone
  connected after the window closes.
- **The phone link's keys:** `<app data>/phone/`, readable by your user only. It holds `desktop.key`, the desktop's
  pairing key (P-256, not encrypted with the passphrase), each paired phone's public keys (`devices.json`), the
  relay's address (`config.json`), held sends (`held.json`) and the log of phone sends (`sends.log`). The crypto is
  in `src-tauri/src/phone/crypto.rs` (P-256 ECDH and ECDSA, HKDF-SHA256, AES-256-GCM). Face ID passkeys are checked
  on the desktop, in `src-tauri/src/phone/webauthn.rs`.
- **Approve sends on my phone** (v0.2.5): `src-tauri/src/phone/mod.rs`, "Approve on my phone". `config.json` keeps the
  amount, the day's counted payments and a change the recovery words made that waits its day. What asks: `send.rs`
  (`approve_first`), `commands.rs` (`rpc_call`, with `phone::credit_payment` for the credit calls' costs),
  `Phone::clear_held` (a phone's held payment confirmed here) and `recovery/commands.rs` (`wallet_reveal`). Someone who
  can change your files can change `config.json` too: it guards the app, not the files.
- **The eCash wallets** (v0.2.6): `src-tauri/src/ecash/`. Their keys come from the words (`keys.rs`: BIP85's XPRV
  application at m/83696968'/32'/0', then BIP84 accounts 0 and 1). The eCash node holds both wallets watch-only, with
  public descriptors; FreeBank checks each payment the node funds (`sign.rs`, `check`: every input its own and its
  parent transaction matching, one output to the recipient, change to its own change branch, eCash's replay stamp,
  the fee quoted and under a rate worked out here) and signs it here. The main eCash wallet's key is derived from
  `seed.enc` with the passphrase for each payment. The bidding wallet's key is in `<app data>/wallet/ecash-bids.key`,
  readable by your user only, so bids can go out with nobody there; it holds only what you move into it. A typed eCash
  login's password is in `<app data>/wallet/ecash-login`; the wallets' public record and the bidding settings and
  rounds are in `ecash.json` and `bmm.json` beside it, all readable by your user only.
- **Several wallets** (v0.2.6): `src-tauri/src/wallets.rs`. A wallet from the words is BIP85's HD-Seed WIF at index
  1, 2 and on, given to the node with `sethdseed`; it shares the wallet passphrase. A wallet file is copied into the
  node's wallet folder, readable by your user only, and keeps its own passphrase.
- **Copying the words to the clipboard:** `src-tauri/src/clipboard.rs`.
- **The screens that show or take the words:** `src/components/RecoveryWords.svelte`, `WalletFlow.svelte` and
  `WalletSettings.svelte` (Show recovery words, and Show xprv: the wallet's master extended private key, derived from
  the words, after the passphrase). The passphrase is typed in `UnlockPrompt.svelte`, `PassphraseFields.svelte`,
  `PhoneSettings.svelte` and `PhoneAlerts.svelte`.

**Every network contact:**
- **Your node:** JSON-RPC on 127.0.0.1, or on another computer if you point the app at one (`src-tauri/src/rpc/`).
- **GitHub:**
  - the FreeBank node's releases (`api.github.com/repos/mbdrivechains/freebank/releases`, at each start when the app
    installed the node, at most every 30 minutes; downloads from `github.com/mbdrivechains/freebank/releases`, which
    redirect to GitHub's file host, `*.githubusercontent.com`). A node is installed only if its `SHA256SUMS`
    signature checks against the key pinned in `src-tauri/src/node/release_key.rs`;
  - the app's own releases (v0.2.4, `src-tauri/src/app_update.rs`): `SHA256SUMS` and `SHA256SUMS.sig` from
    `github.com/mbdrivechains/freebank-app/releases/latest/download/`, 15 seconds after the app starts and every 12
    hours while it runs (an answer is reused for 6 hours), and when you press Check for updates; the new package from
    `github.com/mbdrivechains/freebank-app/releases/download/v<version>/` only when you press Update and restart.
- **explorer.ecxfreebank.com:** the chain's tip height, for Setup's sync progress and the Node tab
  (`src-tauri/src/node/process.rs`, `mod.rs`).
- **app.ecxfreebank.com:**
  - the phone relay (`wss://app.ecxfreebank.com/ws`, or another set in Settings > Phone > Relay), over WebSocket,
    only while a phone is paired or you are pairing one (`src-tauri/src/phone/link.rs`, `Phone::wanted` in
    `mod.rs`). Messages are end-to-end encrypted: the relay only passes sealed messages along. The relay and the
    phone page are in [mbdrivechains/freebank-phone](https://github.com/mbdrivechains/freebank-phone). The page uses
    the phone's camera only to read a FreeBank pairing QR code, when you tap Scan the code on your desktop; the
    pictures stay on the phone (`phone/src/lib/scan.ts` there);
  - the report desk (`POST /feedback`), only when you send a report (`src-tauri/src/feedback.rs`). If you tick
    "Include recent activity", the report also carries the latest lines of the app's own log
    (`<app data>/logs/app.log`) and chosen lines of the node's `debug.log` (progress, start and stop, errors: never
    wallet lines), to the minute, with hashes, coin addresses, amounts, IP addresses, long tokens and your name
    masked (`src-tauri/src/activity.rs`), exactly as the dialog shows them first.
- **The eCash node and its enforcer** that run beside FreeBank, at the addresses found or entered at setup. The eCash
  wallets (v0.2.6) reach the eCash node's JSON-RPC with BitWindow's login or the one typed in Settings > Node &
  connection: plain HTTP only (`https://` is refused), no proxy, host and port only (`src-tauri/src/ecash/conn.rs`).
- **This computer's own addresses:** Settings > Security tries a few TCP connections to them, to see which of the
  node's ports other computers could reach (`src-tauri/src/security.rs`). They stay on this computer.
- **Links** (the explorer, GitHub, BitWindow's site) open in your browser, not in the app. The allowed ones are
  listed in `src/lib/node.ts` and `src-tauri/tauri.conf.json`.

The window's content security policy (`src-tauri/tauri.conf.json`: `default-src 'self'`) stops the screens from
contacting anything themselves. `dangerousDisableAssetCspModification: ["script-src"]` there only keeps Tauri from
adding its own hashes of the app's JS files to `script-src`: they allowed nothing `'self'` doesn't, and Tauri lists them
in the order the build machine's disk returns the files, which made builds differ between machines. It doesn't cover opening links: those go through the allowed list above, in the
browser. `src/lib/api.ts` can also run the screens in an ordinary browser (see the README); the
desktop app doesn't use that mode.

**How the app updates itself** (v0.2.4, `src-tauri/src/app_update.rs`): it trusts only what the node installer
trusts. A new version is offered only when the latest release's `SHA256SUMS` checks against the release key pinned in
`src-tauri/src/node/release_key.rs`, and its version (read from the package names in that signed file) is higher than
the app's. Update and restart downloads the package beside the running app and puts it in place only if its SHA-256 is
the one on its line in the signed file: an AppImage is renamed over the running one; a Mac app
(`FreeBank_<version>_universal.app.tar.gz`) is unpacked from that same checked file beside the running bundle, checked
to be FreeBank at that version for a macOS this Mac has, and exchanged with the running bundle in one step (two renames
where the disk can't). Then the app restarts the usual way. The `.deb` is never replaced by the app:
Software Updater (with FreeBank's apt repository, which trusts the repository's own key, held by the release
workflow) or a new `.deb` does that. Nothing GitHub holds can make an update the app accepts; only the release key can.
But the AppImage and the Mac app don't rebuild byte for byte yet (below), so the key vouches for GitHub's build of them
as checked before signing, not for more. The app also updates only where no other user could swap a file in (the
login item's rule in `src-tauri/src/phone/login_item.rs`), waits until nothing is being done to the node before it
restarts, and doesn't restart after Obliterate. A build with the `update-test` Cargo feature reads
another address and key from the environment, for the update's end-to-end test; no release enables it
(`src-tauri/src/security/tests.rs` checks the release workflow and build scripts).

**What the app asks the node:**
- **Wallet calls** each have their own command on the Rust side:
  - sending: `sendtoaddress`, or `createrawtransaction`, `fundrawtransaction`, `signrawtransactionwithwallet` and
    `sendrawtransaction`;
  - `bumpfee` (Speed up);
  - the passphrase: `encryptwallet`, `walletpassphrase`, `walletlock`, `walletpassphrasechange`;
  - `sethdseed` (the key from the words), `backupwallet`, `rescanblockchain` and `stop`;
  - `getnewaddress` (Receive, a phone's Receive, and moving coins to new words);
  - `createwallet` and `loadwallet` (several wallets, v0.2.6).
- **Bidding** (v0.2.6, `src-tauri/src/ecash/bmm.rs`): the FreeBank node's `get_block_template`, `get_bmm_inclusions`
  and `connect_block`, the calls for an outside bidder.
- **The eCash node** (v0.2.6): its wallets watch-only (`createwallet`, `importdescriptors`, `loadwallet`), addresses,
  balances, history, `walletcreatefundedpsbt` to fund a payment FreeBank then checks and signs, and
  `sendrawtransaction` for the signed payment. It is never given a passphrase or a private key.
- **Read-only calls** the Rust side makes for itself: the balance, history, fees, the node's status, Settings >
  Security and the phone link.
- **Everything else the screens ask** goes through one command, `rpc_call`. It allows only the calls listed in
  `RPC_ALLOWED` (`src-tauri/src/security.rs`): read-only calls, `getdepositaddress`, and FreeBank's notes, houses,
  pools and bills (and, node v0.2.19, a house's members), including credit actions that sign with the wallet and move coins or notes (`transfernote`,
  `swapnote`, `addpoolliquidity`, `issuebill` and others). A locked wallet refuses those until you give the
  passphrase.
- The app never calls `dumpprivkey` or `dumpwallet`. Only a test calls `dumpwallet`, against a scratch node. It can
  show the wallet's master xprv (Settings > Wallet > Show xprv, after the passphrase), derived from the words: the
  key `dumpwallet` would print.

**The programs the app starts:**
- `freebankd`, from `<app data>/releases/`, after its signature has checked (`src-tauri/src/node/install.rs` and
  `process.rs`);
- `freebankd -version`, to read an installed node's version;
- no grpcurl any more (v0.2.5): Setup checks the enforcer over the Connect protocol, a plain HTTP request to the
  enforcer's own port (`enforcer_tip` in `src-tauri/src/node/detect.rs`), as freebankd (v0.2.17 on) talks to it. The
  app neither downloads nor runs grpcurl, nor names one to freebankd;
- the app itself, as the phone link's background part (`--phone-background`), and again after an update (the
  restart);
- on macOS also `xattr` (to clear a download's quarantine), `/bin/ps`, `/usr/bin/sw_vers`, and `touch` on the app
  after an update (so Finder notices it);
- your system's link opener, for the allowed links (Tauri's shell plugin).

**Third-party code:** the Rust crates are pinned by `src-tauri/Cargo.lock`, and the npm packages by
`package-lock.json`. The QR code encoder is the app's own (`src/lib/qr.ts`).

[`SECURITY.md`](SECURITY.md) says how to report a problem privately.

### A prompt for an AI reviewer

Give your assistant the checked-out tree and something like this:

> You are reviewing the FreeBank desktop app, a Tauri 2 app (Rust in `src-tauri/src/`, Svelte screens in `src/`), at
> commit `<commit>`, for a person deciding whether to run it. It keeps the person's 24 recovery words, encrypted with
> their passphrase, and runs their FreeBank node, which holds their coins. VERIFY.md, step 2, maps where the secrets
> are handled, every network contact, and what the app asks the node. Check that map against the code, then look
> for:
> (1) the recovery words, the passphrase, the node's keys or the wallet's files leaving the computer, being logged,
> or being written anywhere the map doesn't say;
> (2) any network contact not in the map, including from the screens (check the content security policy and the
> shell plugin's `open` list in `src-tauri/tauri.conf.json`, and `src-tauri/capabilities/`);
> (3) node calls that could move coins, export keys or change the wallet without the person asking, including
> through `rpc_call` and its allowlist in `src-tauri/src/security.rs`, and through any Tauri command registered in
> `src-tauri/src/lib.rs`;
> (4) anything that lets a paired phone, the relay or a web page do more than the documented limits (the daily
> limit, approval on the desktop, Face ID; `src-tauri/src/phone/`), and any app path that pays or shows the recovery
> words without "Approve sends on my phone" asking while it is on;
> (5) any way the eCash node can make FreeBank sign a payment other than the one shown, or bid beyond the daily cap
> (`src-tauri/src/ecash/`);
> (6) downloads or processes started from untrusted input, and whether the node's signature is checked before it runs;
> (7) dependencies in `src-tauri/Cargo.lock` or `package-lock.json` that look out of place or come from outside the
> usual registries.
> For each finding give the file and line, what an attacker needs, and the impact. Say plainly what you did not
> check.

## Step 3: check the signature and GitHub's attestation

- **Signature:** `SHA256SUMS` is signed with the FreeBank release key, the same key that signs the FreeBank node's
  releases. The key and the commands are in the README, under [Verify your download](README.md#verify-your-download).
- **GitHub attestation:** this repository's release workflow builds each release tag on GitHub, and GitHub records a
  signed attestation naming the workflow, the tag and each package's hash. With a recent GitHub CLI, logged in
  (`gh auth login`):

  ```sh
  gh attestation verify freebank_<version>_amd64.deb --repo mbdrivechains/freebank-app \
    --signer-workflow mbdrivechains/freebank-app/.github/workflows/release.yml \
    --source-ref refs/tags/v<version>
  ```

  The same works for the AppImage, the `.dmg` and the Mac app's update (`.app.tar.gz`). If the attestation verifies and the hash matches `SHA256SUMS`,
  GitHub built exactly these bytes from the tag's source, with `.github/workflows/release.yml`. (`--source-ref`
  pins the tag: the workflow can also be started by hand, though it records attestations only for tags.)

## Step 4: rebuild the Linux package yourself

From v0.2.3, GitHub builds the Linux program and `.deb` inside a pinned Docker image, `build/linux/Dockerfile`:
Ubuntu 22.04 fixed by its digest, Ubuntu's packages from a dated snapshot, and Rust 1.98.1 and Node 20.20.2 checked
against their published hashes. Build the same tag in the same image and you get the same bytes. With Docker:

```sh
git checkout v<version>
build/linux/rebuild.sh v<version>        # 10 to 30 minutes; prints the hashes
grep freebank_<version>_amd64.deb SHA256SUMS
```

The `.deb`'s hash must be the one in the release's `SHA256SUMS`, and the program inside it
(`dpkg-deb -x freebank_<version>_amd64.deb x`, then `x/usr/bin/freebank`) is `build/linux/out/<commit>/freebank`.

## What this does not cover

- **Only the Linux program and `.deb` rebuild byte for byte.** The AppImage is built in the same image, but its
  packing tools are downloaded at build time and give its copy of the program a library path inside the AppImage, so
  neither compares byte for byte yet. The Mac app is built and signed on GitHub's Macs. For those you trust GitHub's
  build machines, which the attestation names. The image itself trusts its base image and Ubuntu's
  packages: a reproducible build, not one that builds its compilers from source.
- **macOS** packages are not notarised yet; macOS asks you to allow the app the first time (README, Install).
- **Releases before v0.2.0** have neither a signature nor an attestation.
- **The FreeBank node** (`freebankd`) is a separate program. The app installs it only if its signature checks; to
  verify it yourself, see "Verify your download" in [mbdrivechains/freebank](https://github.com/mbdrivechains/freebank).
- **The relay's own binary** at app.ecxfreebank.com can't be checked yet; the page it serves can (above). The relay
  only passes sealed messages along, and the desktop decides everything a phone may do (its daily limit, your
  approval, Face ID).
- **Other software you run with FreeBank** (the eCash node, the enforcer, BitWindow) is not covered here.
