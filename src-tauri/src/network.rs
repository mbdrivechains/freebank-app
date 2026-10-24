//! The networks this app runs on (v0.5.0): beta as before, easynet (mbdrivechains' CPU-mined test network on eCash's
//! rules), mainnet from the fork, and any later test network. Each is a set of facts here, not code elsewhere.
//!
//! The app runs one network at a time, chosen at launch: `network.json` in the app's folder (FREEBANK_NETWORK for a
//! developer). Each network keeps its own app folder: beta's is the app's folder itself, as it always was, and the
//! others are `networks/<id>/` inside it, so their settings, wallets' records, receipts and phone pairings never mix.
//! Switching restarts the app (`switch`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What kind of eCash chain a network's L1 is, for the app's eCash wallet: its address and key prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L1Kind {
    /// Bitcoin's main prefixes (bc1…, xpub): eCash beta, and eCash mainnet.
    Main,
    /// Bitcoin's test prefixes (tb1…, tpub): easynet, which the enforcer runs as testnet4.
    Test,
}

/// Which FreeBank releases a network runs, and what its archive is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// v0.2.N, archive `<asset>-0.2.N-<triplet>.tar.gz` (beta, and easynet's builds of the same releases).
    V02,
    /// v1.x.y (mainnet).
    V1,
}

/// An eCash program the app can install for a network: one archive per platform, its sha256 pinned here (the
/// release's SHA256SUMS isn't signed, so the app trusts only these).
#[derive(Debug)]
pub struct Download {
    /// The archive's URL with `{platform}` for "x86_64-linux-gnu" or "universal-apple-darwin".
    pub url: &'static str,
    pub linux: &'static str,
    pub mac: &'static str,
}

/// A UTXO snapshot the network's node can start from (v0.5.1): the node then reaches the tip in about an hour and checks
/// the older history behind it in the background. The node itself refuses a file whose UTXO hash isn't the one compiled
/// into it for that height; the sha256 here only spares a wasted load.
#[derive(Debug)]
pub struct Snapshot {
    /// "ava", "ava935" (until v0.6.3 "ours", and "l2l" for LayerTwo Labs' copy of 935,000): what the screens and the
    /// settings call it.
    pub id: &'static str,
    /// Who publishes it, as the screens name it.
    pub source: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub height: u64,
    /// Its block, as `getchainstates` names a loaded snapshot.
    pub blockhash: &'static str,
    /// The network bytes the node wants in the file's header (v0.6.3). A file made on another network carries that
    /// network's (Bitcoin's in Ava Chow's files, alphanet's in LayerTwo Labs'), so the app writes these over them once the
    /// download is whole; `sha256` is of the file after that.
    pub network: [u8; 4],
}

/// The eCash node and enforcer the app installs and runs for a network itself (v0.5.0), and their default ports.
#[derive(Debug)]
pub struct Stack {
    /// The release, e.g. "easynet-3.1": its programs' folder name.
    pub tag: &'static str,
    /// The chain it runs, e.g. "easynet-3" (reset 3): its data folder's name. A new release on the same chain keeps the
    /// data; a reset (a new chain) starts a folder of its own.
    pub chain: &'static str,
    pub node: Download,
    pub enforcer: Download,
    /// The node's config section (`[easynet]`, `[main]`).
    pub section: &'static str,
    /// The folder inside its data folder the node keeps this chain's files in: "easynet"; "" for main, whose files are
    /// at the top.
    pub folder: &'static str,
    /// The transaction index (`txindex=1`). Not on beta: freebankd reads deposits from the block itself (v0.2.17), and
    /// on beta's chain the index alone is about 70 GB.
    pub txindex: bool,
    /// Snapshots to start from, the first that answers first (v0.5.1). None: a sync from the start.
    pub snapshots: &'static [Snapshot],
    /// The whole chain on disk once the node has every block, in GB of 10^9 bytes (as a Mac's Finder counts), for the disk
    /// check (a full beta node of ours, 2026-10-20: blocks 818 GiB and chainstate 11 GiB by `du -h`, so about 890 GB; no
    /// txindex).
    pub full_gb: u64,
    pub p2p: u16,
    pub rpc: u16,
    pub zmq: u16,
    /// The enforcer's gRPC and JSON-RPC. Not the enforcer's own defaults (50051, 8122): those are BitWindow's beta
    /// enforcer's, and both stacks may run on one computer.
    pub grpc: u16,
    pub erpc: u16,
    /// The enforcer's network flags.
    pub enforcer_args: &'static [&'static str],
    /// Peers the node asks first (`addnode=`): without them it waits a minute before trying its fixed seeds.
    pub seeds: &'static [&'static str],
}

#[derive(Debug)]
pub struct Network {
    /// "beta", "easynet", "mainnet": the app folder's name under networks/ (beta keeps the top), and network.json's word.
    pub id: &'static str,
    /// What the screens call it.
    pub name: &'static str,
    /// What the screens call its eCash chain.
    pub l1: &'static str,
    /// Offered by the network switch (mainnet: from fork day).
    pub ready: bool,
    // ---- The FreeBank node
    pub line: Line,
    /// The start of its archives' names: "freebank", or "freebank-easynet" (freebankd's easynet build, the same release).
    pub asset: &'static str,
    /// freebankd's network folder inside its -datadir: None on beta (its files are at the top), else the folder an
    /// easynet or mainnet build keeps its files in. The app's data folder setting is that folder itself.
    pub net_folder: Option<&'static str>,
    /// Where its data folder goes by default: freebankd's default for beta; beside it for the others (their own top, so
    /// removing one network's folder never takes another's wallets).
    pub datadir: fn() -> PathBuf,
    pub rpc_port: u16,
    pub p2p_port: u16,
    /// The L1 block freebankd is pinned to (-mainchainblockpin): eCash beta's fork block, easynet's genesis.
    pub pin_height: u64,
    pub pin_hash: &'static str,
    /// -mainchainchain, the L1 chain name freebankd checks; "" checks none.
    pub l1_chain: &'static str,
    /// The names its eCash node gives its chain (getblockchaininfo's `chain`).
    pub l1_names: &'static [&'static str],
    pub l1_kind: L1Kind,
    /// eCash's replay stamp on this chain: the nLockTime its node takes as final whatever the height, given an input
    /// below SEQUENCE_FINAL (`IsFinalTx`, src/consensus/tx_verify.cpp). 499999999 on beta (eCash's betanet branch);
    /// 499999998 on mainnet (eCash master 6e58233) and easynet (easynet-3.1). Each node refuses the other's as
    /// non-final (both checked on regtest, 2026-10-24).
    pub replay_locktime: u32,
    /// FreeBank's explorer for this network: tx links and the tip. None: the network has none.
    pub explorer: Option<&'static str>,
    /// Demo mode (no eCash node, FreeBank's gateway) is offered.
    pub demo: bool,
    /// The seed's coinbase tag, which nobody else may take.
    pub seed_tag: &'static str,
    /// The Telegram bot with play money, if the network has one.
    pub bot: Option<&'static str>,
    /// FreeBank's faucet for this network (`POST` an address, v0.6.1), if it has one.
    pub faucet: Option<&'static str>,
    // ---- Where an eCash stack already running is looked for (BitWindow's, or one set up by hand)
    pub rest: &'static str,
    pub enforcer: &'static str,
    /// The stack the app can install for this network, if any.
    pub stack: Option<&'static Stack>,
}

fn freebank_default() -> PathBuf {
    if cfg!(target_os = "macos") {
        crate::node::home().join("Library/Application Support/FreeBank")
    } else {
        crate::node::home().join(".freebank")
    }
}

/// `~/.freebank-<id>/<folder>` (a Mac: `~/Library/Application Support/FreeBank-<id>/<folder>`).
fn beside(id: &str, folder: &str) -> PathBuf {
    let top = freebank_default();
    let name = format!("{}-{}", top.file_name().and_then(|n| n.to_str()).unwrap_or(".freebank"), id);
    top.with_file_name(name).join(folder)
}

fn easynet_datadir() -> PathBuf {
    beside("easynet", "easynet")
}

fn mainnet_datadir() -> PathBuf {
    beside("mainnet", "mainnet")
}

pub static BETA: Network = Network {
    id: "beta",
    name: "Beta",
    l1: "eCash beta",
    ready: true,
    line: Line::V02,
    asset: "freebank",
    net_folder: None,
    datadir: freebank_default,
    rpc_port: 8454,
    p2p_port: 8455,
    pin_height: 967_680,
    pin_hash: "00000000000000030101ba5cfea54b22becc79f95dc6040beb76e01dd9d04042",
    l1_chain: "main",
    l1_names: &["main"],
    l1_kind: L1Kind::Main,
    replay_locktime: 499_999_999,
    explorer: Some("https://explorer.ecxfreebank.com"),
    // Retired in v0.6.2: newcomers start on easynet instead, where FreeBank runs its own eCash node in minutes; a
    // profile left in demo mode goes back to setup (node/mod.rs).
    demo: false,
    seed_tag: "ecxfreebank.com",
    bot: Some("freebank_beta_bot"),
    faucet: None,
    rest: "127.0.0.1:18302",
    enforcer: "127.0.0.1:50051",
    stack: Some(&BETA_STACK),
};

/// easynet-3.1's enforcer: L2L's bip300301_enforcer at master 6e5f79f (2026-09-24) with mbdrivechains' fixes and the
/// easynet preset. It has L2L's `betanet` preset too, and runs beta with it (tested to beta's tip, 2026-10-19).
const ENFORCER_EASYNET_3_1: Download = Download {
    url: "https://github.com/mbdrivechains/bitcoin/releases/download/easynet-3.1/bip300301_enforcer-easynet-3.1-{platform}.tar.gz",
    linux: "324e2eef90704ccb3d3da47f57772284960cd3708bb691176bc58d15501a9706",
    mac: "f83640ad58f5eff94863afec7360705a649ef37826d0925618c19e0e16d01f34",
};

/// Beta's network bytes: how its node's messages and its snapshots start (`pchMessageStart`, ec a5 b1 04).
const BETA_NETWORK: [u8; 4] = [0xec, 0xa5, 0xb1, 0x04];

/// beta's own stack (v0.5.1): mbdrivechains' node (eCash's betanet branch plus a snapshot entry; github.com/mbdrivechains/bitcoin,
/// its SHA256SUMS) and easynet-3.1's enforcer in its betanet preset. Since v0.6.3 the node is betanet-965000-1: the
/// branch plus Bitcoin Core's own 965,000 entry (Ava Chow's commit, cherry-picked unchanged), in place of
/// betanet-967000-1's 967,000 of ours. Its ports keep off BitWindow's beta node and enforcer, and off the eCash node's
/// own defaults (8532, 8533).
static BETA_STACK: Stack = Stack {
    tag: "betanet-965000-1",
    chain: "beta",
    node: Download {
        url: "https://github.com/mbdrivechains/bitcoin/releases/download/betanet-965000-1/bitcoin-betanet-965000-1-{platform}.tar.gz",
        linux: "ae56ef6e88a4352e02fcc1f7bf6730d96d643cfac18e372637f8ba90317e751e",
        mac: "5a9059c46cd65c73b8b17697a1dbfc4d205f5c176586a1deaf1282e96f5b871c",
    },
    enforcer: ENFORCER_EASYNET_3_1,
    section: "main",
    folder: "",
    txindex: false,
    snapshots: &[
        // Ava Chow's: Bitcoin Core's file for its own 965,000 entry (beta's chain is Bitcoin's up to the fork). As
        // downloaded its sha256 is 89351f71…7dda, with Bitcoin's network bytes.
        Snapshot {
            id: "ava",
            source: "Ava Chow",
            url: "https://achow101.com/files/utxo-snapshots/mainnet-965000-utxos.dat",
            sha256: "5583006dc2f191980b0df734e9f986cfd089e79c13d278597c1b78f88f8a657d",
            size: 9_468_312_119,
            height: 965_000,
            blockhash: "00000000000000000001595977e6000ce56129f5c9b4073e31ccc30b90b97da9",
            network: BETA_NETWORK,
        },
        // Ava Chow's file for Bitcoin Core 31.1's own 935,000 entry. As downloaded its sha256 is e572ddbe…d486, with
        // Bitcoin's network bytes. Until v0.6.3 this was LayerTwo Labs' copy (BitWindow's, for alphanet): the same file
        // with alphanet's bytes, which beta's node never loaded.
        Snapshot {
            id: "ava935",
            source: "Ava Chow",
            url: "https://achow101.com/files/utxo-snapshots/mainnet-935000-utxos.dat",
            sha256: "dd38115648221d796a42685d449b84e7ebf352a57bdda37679cece5f6137d1e0",
            size: 9_387_990_306,
            height: 935_000,
            blockhash: "0000000000000000000147034958af1652b2b91bba607beacc5e72a56f0fb5ee",
            network: BETA_NETWORK,
        },
    ],
    full_gb: 890,
    p2p: 38533,
    rpc: 38532,
    zmq: 38535,
    grpc: 38551,
    erpc: 38552,
    enforcer_args: &["--network-preset=betanet"],
    // The node's own seeds find beta's peers (tested 2026-10-19).
    seeds: &[],
};

/// easynet-3.1 (reset 3, 2026-10-14): github.com/mbdrivechains/bitcoin, its SHA256SUMS
/// (ecash/easynet-release/easynet-3.1/SHA256SUMS). Its node and enforcer flags are the easynet installer's
/// (contrib/easynet/easynet.sh on the `easynet` branch).
static EASYNET_STACK: Stack = Stack {
    tag: "easynet-3.1",
    chain: "easynet-3",
    node: Download {
        url: "https://github.com/mbdrivechains/bitcoin/releases/download/easynet-3.1/bitcoin-easynet-3.1-{platform}.tar.gz",
        linux: "bec820f7edde1c22c38d100688fc26835ec3d49aaf007906e601fa27e433e842",
        mac: "f0d41e2c4b36d42e8e3b77c08a17be7bd5008877c9b97184cfb681382586cd78",
    },
    enforcer: ENFORCER_EASYNET_3_1,
    section: "easynet",
    folder: "easynet",
    txindex: true,
    snapshots: &[],
    full_gb: 2,
    p2p: 28533,
    rpc: 28532,
    zmq: 28535,
    grpc: 28551,
    erpc: 28552,
    enforcer_args: &["--network-preset=easynet"],
    // easynet's public door (freebanktest, relayed to our easynet node): TRACKING.md.
    seeds: &["163.47.9.132:28533"],
};

pub static EASYNET: Network = Network {
    id: "easynet",
    name: "easynet",
    l1: "easynet",
    ready: true,
    line: Line::V02,
    asset: "freebank-easynet",
    net_folder: Some("easynet"),
    datadir: easynet_datadir,
    rpc_port: 28454,
    p2p_port: 28455,
    pin_height: 0,
    pin_hash: "00000001a4105dc30c636e71ac2d844caa32d6fa2e9aacc5b2225ea772dd9490",
    l1_chain: "",
    l1_names: &["easynet"],
    l1_kind: L1Kind::Test,
    replay_locktime: 499_999_998,
    // FreeBank's own explorer on easynet (the explorer session's, 2026-10-21); easy.ecxfreebank.com shows easynet's
    // eCash chain, not its FreeBank.
    explorer: Some("https://fbeasy.ecxfreebank.com"),
    demo: false,
    seed_tag: "ecxfreebank.com",
    bot: None,
    faucet: Some("https://faucet.ecxfreebank.com/faucet"),
    rest: "127.0.0.1:28532",
    enforcer: "127.0.0.1:28551",
    stack: Some(&EASYNET_STACK),
};

/// FreeBank mainnet: v1.x, data in its own `mainnet` folder, the same ports as beta. Not offered until
/// the fork; its pin and stack follow when eCash mainnet and its enforcer are out.
pub static MAINNET: Network = Network {
    id: "mainnet",
    name: "Mainnet",
    l1: "eCash",
    ready: false,
    line: Line::V1,
    asset: "freebank",
    net_folder: Some("mainnet"),
    datadir: mainnet_datadir,
    rpc_port: 8454,
    p2p_port: 8455,
    pin_height: 0,
    pin_hash: "",
    l1_chain: "main",
    l1_names: &["main"],
    l1_kind: L1Kind::Main,
    replay_locktime: 499_999_998,
    explorer: None,
    demo: false,
    seed_tag: "ecxfreebank.com",
    bot: None,
    faucet: None,
    rest: "127.0.0.1:18302",
    enforcer: "127.0.0.1:50051",
    stack: None,
};

pub static ALL: &[&Network] = &[&BETA, &EASYNET, &MAINNET];

static CURRENT: OnceLock<&'static Network> = OnceLock::new();

/// The network this run of the app is on. Beta until `set` says otherwise (and in tests).
pub fn current() -> &'static Network {
    CURRENT.get().copied().unwrap_or(&BETA)
}

pub fn by_id(id: &str) -> Option<&'static Network> {
    ALL.iter().copied().find(|n| n.id == id)
}

/// Set the network once, at launch.
pub fn set(n: &'static Network) {
    let _ = CURRENT.set(n);
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Choice {
    network: String,
}

/// The network chosen for this computer: FREEBANK_NETWORK, else `network.json` in the app's top folder, else the
/// default. One that isn't ready (mainnet before the fork) or unknown counts as the default.
pub fn chosen(top: &Path) -> &'static Network {
    let word = std::env::var("FREEBANK_NETWORK").ok().filter(|w| !w.is_empty()).or_else(|| {
        std::fs::read(top.join("network.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Choice>(&b).ok())
            .map(|c| c.network)
    });
    word.as_deref().and_then(by_id).filter(|n| n.ready).unwrap_or_else(|| default_for(top))
}

/// With nothing chosen: beta where the app has run on beta (its settings are in the top folder), else easynet, where a
/// newcomer starts from v0.6.2: FreeBank sets it all up in minutes, and the faucet gives test coins (in place of beta's
/// demo).
fn default_for(top: &Path) -> &'static Network {
    if present(top, &BETA) {
        &BETA
    } else {
        &EASYNET
    }
}

/// The app folder of network `n`: the top folder for beta, `networks/<id>` inside it for the others.
pub fn app_dir(top: &Path, n: &Network) -> PathBuf {
    if n.id == BETA.id {
        top.to_path_buf()
    } else {
        top.join("networks").join(n.id)
    }
}

/// Network `n` has its data on this computer: its app folder holds its settings (an empty folder doesn't count).
pub fn present(top: &Path, n: &Network) -> bool {
    app_dir(top, n).join("settings.json").is_file()
}

/// The data folders of the other networks on this computer, from their settings (their default when they name none):
/// (name, folder). For the folder checks: one network's data folder must never hold another's.
pub fn other_datadirs(top: &Path, here: &Network) -> Vec<(&'static str, PathBuf)> {
    ALL.iter()
        .filter(|n| n.id != here.id && present(top, n))
        .map(|n| {
            let d = std::fs::read(app_dir(top, n).join("settings.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["datadir"].as_str().map(PathBuf::from))
                .unwrap_or_else(|| (n.datadir)());
            (n.name, d)
        })
        .collect()
}

/// One folder is the other, or holds it (compared as written and, where they exist, with links resolved).
pub fn nested(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (ra, rb) = (real(a), real(b));
    a.starts_with(b) || b.starts_with(a) || ra.starts_with(&rb) || rb.starts_with(&ra)
}

/// The network an app folder belongs to (the phone link's background part is started with one).
pub fn of_app_dir(dir: &Path) -> &'static Network {
    let parent = dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
    match (parent, dir.file_name().and_then(|n| n.to_str())) {
        (Some("networks"), Some(id)) => by_id(id).unwrap_or(&BETA),
        _ => &BETA,
    }
}

/// Record the choice for the next launch.
pub fn choose(top: &Path, n: &Network) -> Result<(), String> {
    if !n.ready {
        return Err(format!("FreeBank doesn't run on {} yet.", n.name));
    }
    std::fs::create_dir_all(top).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&Choice { network: n.id.into() }).map_err(|e| e.to_string())?;
    std::fs::write(top.join("network.json"), json).map_err(|e| e.to_string())
}

/// freebankd's data-folder arguments for the folder its files are in (the app's data folder setting). On beta that
/// folder is the -datadir. On a network with its own folder, the -datadir is the folder above it, and the config is
/// read from the folder itself, so freebank.conf sits with the rest of that network's files.
pub fn datadir_args(n: &Network, datadir: &Path) -> Vec<String> {
    match (n.net_folder, datadir.parent()) {
        (Some(f), Some(top)) if datadir.file_name().and_then(|x| x.to_str()) == Some(f) => vec![
            format!("-datadir={}", top.display()),
            format!("-conf={}", datadir.join("freebank.conf").display()),
        ],
        _ => vec![format!("-datadir={}", datadir.display())],
    }
}

/// A data folder someone chose for network `n`: on a network with its own folder, that folder inside it, unless the
/// choice already is one (freebankd always keeps that network's files in a folder of that name).
pub fn fit_datadir(n: &Network, chosen: &Path) -> PathBuf {
    match n.net_folder {
        Some(f) if chosen.file_name().and_then(|x| x.to_str()) != Some(f) => chosen.join(f),
        _ => chosen.to_path_buf(),
    }
}

/// The archive of FreeBank release `tag` for this network and platform triplet, or None for a tag of another line.
pub fn asset_name(n: &Network, tag: &str, triplet: &str) -> Option<String> {
    order(n, tag)?;
    Some(format!("{}-{}-{}.tar.gz", n.asset, tag.trim_start_matches('v'), triplet))
}

/// A release tag's place in this network's line, for "which is newer": (major, minor, patch). None for another line's.
pub fn order(n: &Network, tag: &str) -> Option<(u32, u32, u32)> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let v: Vec<u32> = (0..3).map(|_| parts.next()?.parse().ok()).collect::<Option<_>>()?;
    if parts.next().is_some() {
        return None;
    }
    let ok = match n.line {
        Line::V02 => v[0] == 0 && v[1] == 2,
        Line::V1 => v[0] == 1,
    };
    ok.then_some((v[0], v[1], v[2]))
}

/// What the screens need to know of the network (`network_info`).
#[derive(Debug, Serialize)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
    pub l1: &'static str,
    pub explorer: Option<&'static str>,
    pub demo: bool,
    pub bot: Option<&'static str>,
    /// The network has a faucet (`faucet_ask`).
    pub faucet: bool,
    /// The app can install this network's eCash node and enforcer itself.
    pub stack: bool,
    /// Every network, for the switch: (id, name, offered).
    pub all: Vec<(&'static str, &'static str, bool)>,
}

pub fn info() -> Info {
    let n = current();
    Info {
        id: n.id,
        name: n.name,
        l1: n.l1,
        explorer: n.explorer,
        demo: n.demo,
        bot: n.bot,
        faucet: n.faucet.is_some(),
        stack: n.stack.is_some(),
        all: ALL.iter().map(|x| (x.id, x.name, x.ready)).collect(),
    }
}

#[tauri::command]
pub fn network_info() -> Info {
    info()
}

/// Switch to network `id`: recorded for the next launch, then the app restarts on it. The node of the network left
/// behind stops as the app closes, unless "Keep running" keeps it (it has ports of its own, except mainnet's and beta's).
#[tauri::command]
pub async fn network_switch(app: tauri::AppHandle, mgr: tauri::State<'_, std::sync::Arc<crate::node::NodeManager>>, id: String) -> Result<(), String> {
    let n = by_id(&id).ok_or_else(|| format!("FreeBank doesn't know the network \"{}\".", id))?;
    if n.id == current().id {
        return Ok(());
    }
    mgr.still_here()?;
    // Not in the middle of work a restart would cut short (review finding 6).
    if let Some(why) = busy_with(&mgr) {
        return Err(format!("{} Then switch.", why));
    }
    let _busy = mgr.busy("Switching network…")?;
    let top = top_of(&mgr.app_dir);
    choose(&top, n)?;
    crate::activity::note(&format!("network: switching to {}", n.id));
    // A restart skips the app's exit: this network's node and stack stop here first, as when the app closes (unless
    // "Keep running" keeps them).
    crate::node::background::at_exit(&mgr).await;
    crate::stack::at_exit(&mgr).await;
    app.restart();
}

/// Work under way that a switch (a restart) or Obliterate would cut short, in words; None when there is none.
pub fn busy_with(mgr: &crate::node::NodeManager) -> Option<&'static str> {
    if mgr.install.lock().unwrap().running {
        Some("FreeBank is being installed. Let it finish, or cancel it, first.")
    } else if mgr.update.lock().unwrap().running {
        Some("FreeBank is being updated. Let it finish first.")
    } else if mgr.app_updating.load(std::sync::atomic::Ordering::SeqCst) {
        Some("The FreeBank app is updating itself. Let it finish first.")
    } else if crate::stack::installing() {
        Some("The eCash node is being installed. Let it finish, or stop and delete it (Settings, Node & connection), first.")
    } else if crate::recovery::job::progress().running {
        Some("FreeBank is working on your wallet (its passphrase, words or a restore). Let it finish first.")
    } else {
        None
    }
}

/// The app's top folder, from a network's app folder.
pub fn top_of(dir: &Path) -> PathBuf {
    let n = of_app_dir(dir);
    if n.id == BETA.id {
        dir.to_path_buf()
    } else {
        dir.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders() {
        let top = Path::new("/a/app");
        assert_eq!(app_dir(top, &BETA), top);
        assert_eq!(app_dir(top, &EASYNET), Path::new("/a/app/networks/easynet"));
        assert_eq!(of_app_dir(top).id, "beta");
        assert_eq!(of_app_dir(&app_dir(top, &EASYNET)).id, "easynet");
        assert_eq!(of_app_dir(Path::new("/a/networks")).id, "beta");
        assert_eq!(top_of(&app_dir(top, &EASYNET)), top);
        assert_eq!(top_of(top), top);
    }

    #[test]
    fn each_chain_takes_its_own_replay_stamp() {
        // IsFinalTx: LOCKTIME_THRESHOLD - 1 on eCash's betanet branch, - 2 on its master and on easynet-3.1.
        assert_eq!(BETA.replay_locktime, 499_999_999);
        assert_eq!(EASYNET.replay_locktime, 499_999_998);
        assert_eq!(MAINNET.replay_locktime, 499_999_998);
    }

    #[test]
    fn the_choice_is_read_and_kept() {
        let d = std::env::temp_dir().join(format!("fbnet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(chosen(&d).id, "easynet", "a newcomer starts on easynet (v0.6.2)");
        choose(&d, &BETA).unwrap();
        assert_eq!(chosen(&d).id, "beta");
        choose(&d, &EASYNET).unwrap();
        assert_eq!(chosen(&d).id, "easynet");
        assert!(choose(&d, &MAINNET).is_err(), "mainnet isn't offered before the fork");
        std::fs::write(d.join("network.json"), r#"{"network":"mainnet"}"#).unwrap();
        assert_eq!(chosen(&d).id, "easynet", "a network not ready counts as the default");
        std::fs::write(d.join("network.json"), r#"{"network":"nowhere"}"#).unwrap();
        assert_eq!(chosen(&d).id, "easynet");
        // Someone who has run the app on beta stays there, with nothing chosen, as before v0.6.2.
        std::fs::remove_file(d.join("network.json")).unwrap();
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        assert_eq!(chosen(&d).id, "beta");
        std::fs::write(d.join("network.json"), r#"{"network":"nowhere"}"#).unwrap();
        assert_eq!(chosen(&d).id, "beta");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn data_folders() {
        assert_eq!(datadir_args(&BETA, Path::new("/h/.freebank")), vec!["-datadir=/h/.freebank".to_string()]);
        assert_eq!(
            datadir_args(&EASYNET, Path::new("/h/.freebank-easynet/easynet")),
            vec!["-datadir=/h/.freebank-easynet".to_string(), "-conf=/h/.freebank-easynet/easynet/freebank.conf".to_string()]
        );
        assert_eq!(fit_datadir(&EASYNET, Path::new("/big/fb")), Path::new("/big/fb/easynet"));
        assert_eq!(fit_datadir(&EASYNET, Path::new("/big/fb/easynet")), Path::new("/big/fb/easynet"));
        assert_eq!(fit_datadir(&BETA, Path::new("/big/fb")), Path::new("/big/fb"));
        let e = (EASYNET.datadir)();
        assert_eq!(e.file_name().unwrap(), "easynet");
        assert_ne!(e.parent(), Some((BETA.datadir)().as_path()), "easynet's data isn't inside beta's folder");
        assert!(!e.starts_with((BETA.datadir)()));
    }

    #[test]
    fn releases() {
        assert_eq!(asset_name(&BETA, "v0.2.29", "x86_64-linux-gnu").as_deref(), Some("freebank-0.2.29-x86_64-linux-gnu.tar.gz"));
        assert_eq!(
            asset_name(&EASYNET, "v0.2.29", "arm64-apple-darwin").as_deref(),
            Some("freebank-easynet-0.2.29-arm64-apple-darwin.tar.gz")
        );
        assert_eq!(asset_name(&BETA, "v1.0.0", "x86_64-linux-gnu"), None, "beta never takes mainnet's line");
        assert_eq!(asset_name(&MAINNET, "v1.0.0", "x86_64-linux-gnu").as_deref(), Some("freebank-1.0.0-x86_64-linux-gnu.tar.gz"));
        assert_eq!(asset_name(&MAINNET, "v0.2.29", "x86_64-linux-gnu"), None);
        assert!(order(&BETA, "v0.2.29") > order(&BETA, "v0.2.9"));
        assert_eq!(order(&BETA, "v0.3.5"), None, "the retired Rust line");
        assert_eq!(order(&BETA, "v0.2.7.1"), None);
        assert!(order(&MAINNET, "v1.0.10") > order(&MAINNET, "v1.0.9"));
    }

    #[test]
    fn stacks_keep_off_bitwindows_ports() {
        for n in [&BETA, &EASYNET] {
            let s = n.stack.unwrap();
            for p in [s.grpc, s.erpc, s.rpc, s.p2p, s.p2p + 1, s.zmq] {
                // BitWindow's beta node and enforcer, FreeBank's own ports, the eCash node's defaults.
                assert!(![50051, 8122, 18301, 18302, 18303, 8454, 8455, 8532, 8533, 8534].contains(&p), "{} {p}", n.id);
            }
        }
        let s = EASYNET.stack.unwrap();
        assert_eq!(EASYNET.enforcer, format!("127.0.0.1:{}", s.grpc));
        assert_eq!(EASYNET.rest, format!("127.0.0.1:{}", s.rpc));
        // On beta, an eCash stack already running is still looked for where BitWindow's listens.
        assert_eq!(BETA.rest, "127.0.0.1:18302");
        assert_eq!(BETA.enforcer, "127.0.0.1:50051");
        // The two stacks never share a port, so both may run at once.
        let (b, e) = (BETA.stack.unwrap(), EASYNET.stack.unwrap());
        let ports = |s: &Stack| vec![s.p2p, s.p2p + 1, s.rpc, s.zmq, s.grpc, s.erpc];
        assert!(ports(b).iter().all(|p| !ports(e).contains(p)));
    }

    #[test]
    fn betas_snapshots() {
        let s = BETA.stack.unwrap();
        assert_eq!(s.snapshots.iter().map(|x| x.id).collect::<Vec<_>>(), ["ava", "ava935"], "Ava Chow's 965,000, then her 935,000");
        for x in s.snapshots {
            assert_eq!(x.sha256.len(), 64);
            assert_eq!(x.blockhash.len(), 64);
            assert!(x.height < BETA.pin_height, "a snapshot below FreeBank's pin block");
            assert!(x.size > 9_000_000_000);
            assert_eq!(x.network, [0xec, 0xa5, 0xb1, 0x04], "beta's network bytes");
        }
        assert!(!s.txindex && s.folder.is_empty() && s.section == "main");
        assert!(EASYNET.stack.unwrap().snapshots.is_empty());
    }
}
