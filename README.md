# FreeBank

The desktop app for the [**FreeBank**](https://github.com/mbdrivechains/freebank) credit-creation
drivechain (BIP 300/301, slot 130): it sets up and runs a FreeBank node beside your eCash beta node,
shows its blocks and peers, and is its wallet.

## Install

Download from the [releases page](https://github.com/mbdrivechains/freebank-app/releases/latest):

- **Ubuntu 22.04+ / Debian 12+:** `freebank_<version>_amd64.deb`. Double-click it and the software installer does the
  rest (or `sudo apt install ./freebank_<version>_amd64.deb`).
- **Other Linux:** the `.AppImage`. Make it executable (`chmod +x`), then run it.
- **macOS (Apple Silicon and Intel):** the `.dmg`. Drag FreeBank to Applications. The FreeBank node needs macOS 14 or
  later on Apple Silicon, and macOS 15 or later on Intel. This build is not notarised yet, so the first
  time macOS will refuse to open it: go to **System Settings → Privacy & Security** and choose **Open Anyway**.

### Updates

From v0.2.4 on, FreeBank says when a new version is out (and Settings > App updates checks on request). The AppImage
and the Mac app update themselves: **Update and restart** downloads the new version, checks it and opens it. They take
an update only when the release's `SHA256SUMS` carries the release key's signature (below) and the package matches its
line there: the same check FreeBank makes before installing the node. The `.deb` updates with Software Updater once
[FreeBank's apt repository](https://apt.ecxfreebank.com) is set up, or with the new `.deb` from the releases page. apt
checks the repository's own signing key, which the release workflow holds, not the release key.

### Verify your download

Each release lists its files' SHA-256 hashes in `SHA256SUMS`. From v0.2.0 it is signed with the FreeBank release key
(`SHA256SUMS.sig`), the same key that signs the FreeBank node's releases. The key's public half is below and is also
published as a signing key on the maintainer's GitHub account
([mblowes](https://api.github.com/users/mblowes/ssh_signing_keys)), so you can check it from two places:

```
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAi2C9Lpi3gHPva6tlbLE+wdF1Cer3uUnmwZYr6SeRjR FreeBank release signing
fingerprint SHA256:1d0zm9Qb9ZtzDnQHH593fgjAkk7nPqMDG79XyWlyeeY
```

To verify (OpenSSH 8.1 or later):

```
echo 'freebank-release ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAi2C9Lpi3gHPva6tlbLE+wdF1Cer3uUnmwZYr6SeRjR' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I freebank-release -n file -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
```

The packages are built by this repository's GitHub workflow from the tagged source, and GitHub records a signed build
attestation for each. Check one with GitHub CLI 2.49 or later, logged in (`gh auth login`):

```
gh attestation verify freebank_<version>_amd64.deb --repo mbdrivechains/freebank-app
```

To review the source, and for what these checks do and don't prove, see [VERIFY.md](VERIFY.md).

## Model: your own node, your own keys

Your wallet lives in your own `freebankd` node, and the app runs it for you.
- **A passphrase and 24 recovery words** (v0.2.0). The words are standard BIP39 words. The node's wallet key comes
  from them by BIP85 (the HD-Seed WIF application, index 0), so a BIP85 tool can rebuild it without this app. The app
  keeps the words only encrypted with your passphrase, and never stores the passphrase.
- **Your phone as a remote** (v0.2.0). Pair it in Settings > Phone. It reaches this computer through the relay at
  app.ecxfreebank.com, end-to-end encrypted: the relay only passes sealed messages along. The phone has a daily
  sending limit, and bigger payments wait for you here. It shows your house notes and the houses, and sends, redeems
  or demands notes under the same limit (v0.2.5).
- **Approve sends on my phone** (v0.2.5, opt in). Once this computer's payments in a day come to more than an amount
  you set, your phone approves the next one with Face ID. It guards the app, not the node: someone with this computer
  and your wallet passphrase could still use the node directly.
- Advanced: the app can also connect to your own node on another computer, over Tailscale.

## First run (desktop)

On a computer that already runs an **eCash beta full node and its enforcer** (the easiest way
is [BitWindow](https://releases.drivechain.info) in full-node mode on eCash beta), FreeBank
finds them, downloads and verifies the newest FreeBank node, asks for the name your blocks
carry on the [explorer](https://explorer.ecxfreebank.com), and starts the node. The **Node**
tab then shows its height against the explorer and its peers. If the node is already running,
the app just connects to it.

## Features

- Connect to a `freebankd` node via RPC (local / Tailscale / custom)
- Balance and every amount in **ECX**, the only unit while gold is switched off; transaction
  history
- **Send** with Max, a speed choice and the fee shown before you confirm; a receipt with the transaction id and its
  confirmations up to 3; **Speed up** while a payment waits; **History** with CSV export
- **Receive** and **Deposit** (from eCash, through BitWindow) with QR codes; coins still arriving show on Home
- **Wallet** (Settings): passphrase, recovery words, back up, restore from a file or from the words, change
  passphrase
- **Security** (Settings): the wallet's passphrase, the node's ports, old unencrypted backups, file permissions and
  the node program's signature, with red items on Home until fixed
- **Phone remote**: pair a phone, set its daily limit, allow or refuse bigger payments here; notes and houses on the
  phone; approve this computer's bigger payments on the phone
- Keep the node running after you close the app
- **Notes** — hold / mint / send / redeem / demand, per issuing house
- **Houses** — directory, registration, reserve attestation
- **Clearing pools** — swap notes ↔ ECX, add/remove liquidity, LP positions
- **Bills of exchange** — issue / endorse / retire / claim escrow
- (Planned) v0.3.0: the app's own eCash wallet, and Deposit and Withdraw three ways (at par, atomic swap, money
  changer); v0.4.0: bidding for FreeBank blocks

## Quick Start

### Prerequisites

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Node.js 20+
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
# Tauri CLI + Linux webview deps
cargo install tauri-cli
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

### Development

```bash
git clone https://github.com/mbdrivechains/freebank-app.git
cd freebank-app
npm install
cargo tauri dev        # desktop, hot reload
# or, browser/PWA:
npm run dev            # then open http://localhost:5173
```

### Build Release

```bash
npx tauri build        # -> src-tauri/target/release/bundle/  (always via the tauri CLI: it bundles the front end)
```

Releases are built by `.github/workflows/release.yml` on a `vX.Y.Z` tag: Linux (`.deb`, AppImage) on
Ubuntu 22.04, one universal macOS app (Apple Silicon and Intel), and a GitHub Release with checksums.

## Connect to a node

The wallet needs a running `freebankd` node with RPC enabled:

```bash
# main (RPC 8454); the app reads the node's cookie file, so no password is needed locally
freebankd -daemon

# to reach it from another computer, bind RPC to the Tailscale interface and allow only that client
# (the Security panel then shows RPC as reachable from the network, on purpose):
#   -rpcbind=<tailscale-ip> -rpcallowip=<client-tailscale-ip>
```

FreeBank has two networks only — **main** (RPC 8454) and **regtest** (RPC 18457); there is
no testnet. Then enter the host/port/credentials in the connect screen.

### Browser (PWA) mode + CORS

A browser page can't send RPC directly (CORS). For local development, run the bundled CORS proxy
on this computer; it listens on 127.0.0.1 only, answers only the dev origin, and passes on the
login the page sends rather than adding one:

```bash
python3 proxy.py --rpc-port 8454
```

## Architecture

```
┌─────────────────────────────────────────┐
│         Svelte Frontend (+ PWA)         │
│ Connect · Notes · Houses · Pools · Bills│
└─────────────────┬───────────────────────┘
     Tauri IPC    │    or  direct fetch (PWA + CORS proxy)
┌─────────────────▼───────────────────────┐
│        Rust Backend (FreeBankClient)    │
│         JSON-RPC client (reqwest)       │
└─────────────────┬───────────────────────┘
                  │ JSON-RPC (local / Tailscale / Tor)
┌─────────────────▼───────────────────────┐
│      freebankd  (drivechain slot 130)   │
│  keys · notes · houses · pools · bills  │
└─────────────────────────────────────────┘
```

## Report a problem

In the app: **Settings, then Help**. Choose a problem, an idea or a security problem, and either:
- **Send to FreeBank:** no account needed. The app sends your text, and if you leave the box ticked, the app's version,
  your system and the node's version. Nothing else: no addresses, balances or logs. Only the FreeBank team reads it.
- **Open on GitHub:** opens a prefilled [issue](https://github.com/mbdrivechains/freebank-app/issues/new/choose) in
  your browser for you to check and post. Issues are public.

Security problems go privately: see [SECURITY.md](SECURITY.md).

## License

MIT, see [LICENSE](LICENSE).
