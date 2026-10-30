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
  with it to show the words or the xprv, and restores from the words or a backup file. "Is this address mine?" opens
  `seed.enc` with it to look for an eCash address among the words' addresses, showing nothing else
  (`src-tauri/src/wallet_check.rs`, v0.6.8); "Check my words" needs no passphrase (it compares key ids).
- **The addresses you gave out** (v0.6.8): `<app data>/wallet/addresses.json`, readable by your user only: each
  address Receive handed out, when, where, and your note (`src-tauri/src/given.rs`). No keys.
- **The node's RPC login:** its `.cookie` file, or `rpcuser` and `rpcpassword` from `freebank.conf`
  (`src-tauri/src/node/detect.rs`), or the password typed in the connect form. It is kept in memory only
  (`src-tauri/src/rpc/`) and sent to the node as HTTP Basic auth.
- **Phone sends:** with "Let my phone send" on, `src-tauri/src/phone/mod.rs` keeps the passphrase in memory while the
  app is open. `src-tauri/src/phone/background.rs` hands it over a pipe to the background part that keeps your phone
  connected after the window closes.
- **The phone link's keys:** `<app data>/phone/`, readable by your user only. It holds `desktop.key`, the desktop's
  pairing key (P-256, not encrypted with the passphrase), `nostr.key`, the desktop's own Nostr key for the link (never
  yours), each paired phone's public keys (`devices.json`), the Nostr relays and the phone page's address
  (`config.json`), held sends (`held.json`) and the log of phone sends (`sends.log`). The crypto is in
  `src-tauri/src/phone/crypto.rs` (P-256 ECDH, HKDF-SHA256, AES-256-GCM). Face ID passkeys are checked
  on the desktop, in `src-tauri/src/phone/webauthn.rs`.
- **Approve sends on my phone** (v0.2.5): `src-tauri/src/phone/mod.rs`, "Approve on my phone". `config.json` keeps the
  amount, the day's counted payments and a change the recovery words made that waits its day. What asks: `send.rs`
  (`approve_first`), `commands.rs` (`rpc_call`, with `phone::credit_payment` for the credit calls' costs),
  `Phone::clear_held` (a phone's held payment confirmed here) and `recovery/commands.rs` (`wallet_reveal`). Someone who
  can change your files can change `config.json` too: it guards the app, not the files.
- **The eCash wallets** (v0.2.6): `src-tauri/src/ecash/`. Their keys come from the words (`keys.rs`: BIP85's XPRV
  application at m/83696968'/32'/0', then BIP84 accounts 0 and 1). The eCash node holds both wallets watch-only, with
  public descriptors; FreeBank checks each payment the node funds (`sign.rs`, `check`: every input its own and its
  parent transaction matching, one output to the recipient, change to its own change branch, eCash's replay stamp
  (the network's nLockTime, `Network::replay_locktime`: 499999999 on beta, 499999998 on easynet and mainnet, v0.6.4),
  the fee quoted and under a rate worked out here) and signs it here. The main eCash wallet's key is derived from
  `seed.enc` with the passphrase for each payment. The bidding wallet's key is in `<app data>/wallet/ecash-bids.key`,
  readable by your user only, so bids can go out with nobody there; it holds only what you move into it. A typed eCash
  login's password is in `<app data>/wallet/ecash-login`; the wallets' public record and the bidding settings and
  rounds are in `ecash.json` and `bmm.json` beside it, all readable by your user only.
- **Several wallets** (v0.2.6): `src-tauri/src/wallets.rs`. A wallet from the words is BIP85's HD-Seed WIF at index
  1, 2 and on, given to the node with `sethdseed`; it shares the wallet passphrase. A wallet file is copied into the
  node's wallet folder, readable by your user only, and keeps its own passphrase.
- **Wallets kept for others** (v0.2.8): `src-tauri/src/phone/hosted.rs`. An invited phone's wallet is made in your node
  (`createwallet hosted-<id>`, `encryptwallet` with a random 32-byte passphrase, `sethdseed` from fresh words, BIP85
  index 0). The passphrase, and the words' entropy until their phone confirms them, are in
  `<app data>/phone/hosted-keys.json` (readable by your user only, with a `.bak` of the last one); the list of those
  phones is `hosted.json` beside it. The words are shown once to their phone and never stored after. A hosted phone
  is served by `serve_hosted` only, never by the owner's narrow door. Deleting a copy: emptied by the move home, the
  file and its passphrase are deleted at the node's next start; otherwise they move into `<app data>/hosted-removed/`.
- **Networks** (v0.5.0, `src-tauri/src/network.rs`): each network is a set of facts in that file: where its FreeBank
  node comes from, its eCash node and enforcer with their sha256, its ports and its pinned block. The app runs one at a
  time, each with its own folder (`<app data>/networks/<id>/`; beta's stays at the top) and its own wallet. Switching
  restarts the app.
- **The eCash node and enforcer the app runs itself** (v0.5.0, easynet; `src-tauri/src/stack.rs`): downloaded from
  `github.com/mbdrivechains/bitcoin/releases/download/<tag>/` and used only if each archive's sha256 is the one in
  `network.rs` (that release's `SHA256SUMS` isn't signed, so the app trusts only these). The node's RPC, its ZMQ and
  the enforcer listen on 127.0.0.1 only. The node's RPC logs in by its cookie (readable by your user only); the
  enforcer has no login, so it stays on this computer. Before starting them the app looks at which programs hold their
  ports (Linux `/proc`, Mac `lsof`) and takes free ones; it never changes another program's settings. Their data:
  `<app data>/networks/<id>/stack-data/<chain>/` (on beta `<app data>/stack-data/beta/`). On easynet: easynet-4's node
  and enforcer (v0.6.9, reset 4; the enforcer is LayerTwo Labs' current one, run with `--fork=ecash`, easynet's
  installer's flag), on the chain `easynet-4`. Until v0.6.8 it was the `easynet-3.2` node and easynet-3.1's enforcer on
  `easynet-3`, whose folder the app leaves where it is.
- **A reset** (v0.6.9, `node/restart.rs`): when the eCash block FreeBank's node pins is no longer the one its data was
  made on (the pin it was last started with, `.freebank-pin`; else freebankd's `mainblockhash.dat`; else a block older
  than the reset), the node's chain data moves to `<data folder>.old-<time>` before it starts (inside the folder, as
  `old-chain-<time>`, when the folder is a disk of its own); nothing is deleted, and Start over knows the folder. A node
  an older app left running on the old chain is started again for it. The wallets keep their keys, passphrases and
  addresses; their old-chain payments stay in them, in no balance, and the app's lists leave them out (in no block, and
  seen before the move, whose time is kept in the folder as `.freebank-moved`; withdrawals recorded before it too). They are not removed: freebankd's `removeprunedfunds` crashes the node at the next
  `listtransactions` (Core 0.16's dangling pointer, fixed upstream in bitcoin#13437).
- **Beta's own eCash node** (v0.5.1, same file): mbdrivechains' `betanet-965000-1` node (v0.6.3; `betanet-967000-1`
  before) and easynet-3.1's enforcer in its `betanet` preset, without the transaction index (the FreeBank node reads
  deposits from the block itself). The whole chain is about 890 GB. It starts from a UTXO snapshot when one answers: Ava
  Chow's at 965,000, else hers at 935,000 (both v0.6.3; until then ours at 967,000, else LayerTwo Labs' copy of 935,000,
  which said alphanet and never loaded), else it syncs from the start. Both files were made on Bitcoin, so once one is
  downloaded the app writes beta's 4 network bytes over theirs at offset 7 of its header (`own_network` in `stack.rs`)
  and nothing else. The file is then checked against the sha256 in `network.rs` (of the file with beta's bytes), then
  the node checks its UTXO hash against the one compiled into it for that height and refuses any other, so a swapped
  file can't be loaded. The file is deleted once loaded. Its blocks may go on another disk (`blocksdir=`): always in a
  folder the app makes itself, `freebank-<chain>-blocks`, inside an existing folder given as a full path without `..`
  and resolved through links first (`blocks_folder`). Every folder the app makes for it carries a mark
  (`.freebank-stack`); "Stop and delete" stops the FreeBank node, the eCash node and the enforcer and deletes only
  marked folders, or a chain's folder directly inside the app's own `stack-data/` when neither is a link (`in_own_data`:
  v0.5.0 made no mark). The node is stopped when less than 20 GB is free where it writes (`LOW_DISK`; 2 GB for easynet's
  small chain). The node's RPC listens on 127.0.0.1 only. Obliterate lists those folders as a line of their own; untick
  it to keep the chain. A reinstall or a port move from Settings leaves a user who chose another computer's node on it
  (`record_stack`). An update refuses while the node is still checking the history behind a snapshot the new node has no
  entry for (`stranded_snapshot`: 967,000 in v0.6.3), as that node would stop as it starts; Stop and delete, then
  Install, starts afresh.
- **Another computer's eCash node** (v0.5.1, `find_remote` in `stack.rs`): setup can look for an eCash node and
  enforcer at an IP address or Tailscale name, on their usual ports (18302, 18303, 8532 and 38532 for the node's REST,
  50051 and 38551 for the enforcer, or the port typed), and use them. It reads `/rest/chaininfo.json`, the pinned block
  and the enforcer's tip there, and changes nothing on that computer while it looks. Once it is used, the app's eCash
  wallet (given a login under Settings, eCash) adds its two watch-only wallets to that node (`createwallet`, then
  `importdescriptors` with a rescan from beta's pinned block) and sends the user's own payments and bids through it, as
  with any eCash node; it opens no other wallet there and deletes nothing. A typed login is forgotten when "Use them"
  picks another host (`use_remote`), so it is never sent to a computer it wasn't typed for. It says when the address
  isn't a Tailscale one
  (100.64.0.0/10, `fd7a:115c:a1e0::/48`): REST and the enforcer have no login and nothing on that link is encrypted.
  Obliterate deletes nothing on another computer, and says so.
- **Copying the words to the clipboard** (`src-tauri/src/clipboard.rs`, `copy_words`; v0.4.10, the Truthcoin app's
  reviewed version): after a warning on the screen, the app puts the words on the clipboard as a numbered table and as
  plain words. The command takes only 24 recovery words (the ones the screen shows); the grid on screen can't be
  selected or copied.
  - On Linux the copy is marked `x-kde-passwordManagerHint: secret`. Some clipboard managers honour that (Klipper);
    some don't (GNOME's own clipboard keeper, which holds the latest copy).
  - On a Mac it is written with AppKit, not arboard: one item, marked concealed and transient (the nspasteboard.org
    convention for clipboard managers), and "current host only", so Universal Clipboard doesn't offer it to your other
    Apple devices.
  - The app clears it after 60 seconds, at quit, and after a panic, if the clipboard still holds the app's copy. A
    SIGTERM, SIGINT or SIGHUP quits the app the same way (one that was ignored when the app started, as `nohup` does to
    SIGHUP, stays ignored); a second one, a second or more later, ends it at once after one more try at the clear. On a
    Mac the app's copy is told by the pasteboard's change count, so the app never reads another app's copy. A `kill -9`
    or a crash outside the app's code skips the clear: then the words stay until something else is copied. At quit and
    after a panic the app waits at most a second for the clear.
  - The three-word check refuses the app's own copy, told from the clipboard itself (on Linux its HTML, on a Mac the
    change count). A paste of all the words from elsewhere (notes, a password manager) is taken, and the app then clears
    the clipboard (on Linux also the selection for middle-click paste, and any X11 cut buffer holding words, where xterm
    puts its selections). It says the words are gone only when it could check: on Linux it reads them back at a quarter
    second and at a second (a clipboard manager may put the words back; one slower than that is missed), and on a
    Wayland desktop it can't check, so it says so; on a Mac `clearContents` empties the pasteboard. Otherwise it asks you
    to copy something else. It can't clear what a clipboard history has already kept, and says so. A copy you make in
    another app has none of the protections above.
  - Closing the wallet screens takes the app's copy off the clipboard if it is still there.
  - The app's process can't be dumped on Linux (no core dump with words in it), except when `GTK_USE_PORTAL=1`: then
    GTK's file chooser goes through xdg-desktop-portal, which refuses a process that can't be dumped. That covers the
    app's own process only. The page runs in WebKit's web process (`WebKitWebProcess`), which starts dumpable again,
    and the page holds the words while they are shown or typed. If that process crashes then, Ubuntu's crash reporter
    (apport) can write a report with its memory to `/var/crash` and offer to send it, whatever the core-dump limit says.
    Don't send such a report, and delete it.
  - The clipboard is reached through X11 on Linux (XWayland on a Wayland desktop); it has been checked on X11 only.
- **Restoring from a list:** words typed or pasted in order, or a numbered list where every number has a word and the
  numbers run 1 to n. Anything else is refused, not guessed at. For a numbered list the app shows the words in the
  order it read them and asks before restoring (`seed.rs` `tokens`, and `wordsIn` in `src/lib/walletSeed.ts`, the same
  rule). A paste of words into the restore box clears the clipboard the same way.
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
    `github.com/mbdrivechains/freebank-app/releases/download/v<version>/` only when you press Update and restart;
  - on easynet (v0.5.0): its eCash node and enforcer from `github.com/mbdrivechains/bitcoin/releases/download/<tag>/`,
    when you press Install or Update, checked as above; FreeBank's easynet node (`freebank-easynet-<version>-...`) from
    the same FreeBank releases, with the same signature check: the newest v0.2.N, or since v0.6.10 v1.x from v1.0.1,
    pre-releases included (FreeBank keeps v1.x pre-releases until mainnet's switch-on; `network.rs`, `Line`);
  - on beta (v0.5.1), only while setup offers FreeBank's own eCash node, or one waits to load its snapshot: a `HEAD`
    request to each snapshot's address (`achow101.com`; `data.drivechain.dev` too before v0.6.3), at
    most every ten minutes, to see which answers with its size. When you press Install: the node and enforcer from
    `github.com/mbdrivechains/bitcoin/releases/download/<tag>/`, and the snapshot you chose from its address (resumed
    with `Range` after an interruption).
- **explorer.ecxfreebank.com:** the chain's tip height, for Setup's sync progress and the Node tab
  (`src-tauri/src/node/process.rs`, `mod.rs`). On easynet there is no explorer: the tip comes from the headers the
  node has checked.
- **Public Nostr relays** (`relay.damus.io`, `nos.lol` and `relay.primal.net`, or the ones set in Settings > Phone >
  Nostr relays): the phone link, over WebSocket, only while a phone is paired or you are pairing one
  (`src-tauri/src/phone/carrier.rs`, `relays.rs`, `Phone::nostr_wanted` in `mod.rs`). Messages are end-to-end
  encrypted: the relays only pass sealed messages along, between the desktop's own Nostr key and each phone's. Until
  v0.6.8 the link could also go through a relay at `wss://app.ecxfreebank.com/ws`; the app no longer uses it.
- **app.ecxfreebank.com:**
  - the phone page, which your phone opens (the desktop only names it in the pairing link); it is in
    [mbdrivechains/freebank-phone](https://github.com/mbdrivechains/freebank-phone). The page uses the phone's camera
    only to read a FreeBank pairing QR code, when you tap Scan the code on your desktop; the pictures stay on the
    phone (`phone/src/lib/scan.ts` there);
  - the report desk (`POST /feedback`), only when you send a report (`src-tauri/src/feedback.rs`). If you tick
    "Include recent activity", the report also carries the latest lines of the app's own log
    (`<app data>/logs/app.log`) and chosen lines of the node's `debug.log` (progress, start and stop, errors: never
    wallet lines), to the minute, with hashes, coin addresses, amounts, IP addresses, long tokens and your name
    masked (`src-tauri/src/activity.rs`), exactly as the dialog shows them first.
- **The eCash node and its enforcer** that run beside FreeBank, at the addresses found or entered at setup. The eCash
  wallets (v0.2.6) reach the eCash node's JSON-RPC with BitWindow's login or the one typed in Settings > Node &
  connection: plain HTTP only (`https://` is refused), no proxy, host and port only (`src-tauri/src/ecash/conn.rs`).
  On easynet the eCash node the app runs talks to easynet's other nodes (P2P, its network port), starting from the
  seed in `network.rs`; on beta (v0.5.1) to beta's other nodes, from the node's own seeds. With another computer's
  node (v0.5.1), the FreeBank node and the eCash wallet reach it at the address typed at setup.
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
  - `createwallet` and `loadwallet` (several wallets, v0.2.6; and wallets kept for others, v0.2.8);
  - for wallets kept for others (v0.2.8): `addhousemembers` and `removehousemembers` from your wallet (signed with the
    phone-send passphrase), a 0.01 ECX fee float to them, and from their wallet `transfernote`, `redeemnote` and
    `sendtoaddress` (each with their phone's Face ID, or moving their money home), `listmynotes`, `getbalance`;
    `listhousemembers` and `listreceivedbyaddress` to find member addresses.
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
> (`src-tauri/src/ecash/`); any way a hosted phone (`src-tauri/src/phone/hosted.rs`) reaches the owner's wallet,
> another hosted wallet, or more than its own house's member changes;
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
- **The Nostr relays** are other people's servers; they only pass sealed messages along, and the desktop decides
  everything a phone may do (its daily limit, your approval, Face ID). The server at app.ecxfreebank.com can't be
  checked yet; the page it serves can (above).
- **The eCash node and enforcer the app installs on easynet and on beta** are mbdrivechains' builds of eCash's node and
  LayerTwo Labs' enforcer. The app checks they are the files this release names, not how they were built: their
  release notes at `github.com/mbdrivechains/bitcoin/releases` say how each was built (betanet-965000-1's Linux build is
  a Guix build, rebuilt twice to the same sha256). The snapshots' UTXO hashes are compiled into that node, both
  Bitcoin Core's own: 935,000 from Bitcoin Core 31.1, and 965,000 from Ava Chow's commit adding it (Bitcoin Core
  6e582335d1, cherry-picked unchanged onto eCash's betanet branch).
- **Other software you run with FreeBank** (on beta, the eCash node and enforcer you run, BitWindow) is not covered
  here.
