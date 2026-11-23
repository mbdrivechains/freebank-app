//! The eCash chain that FreeBank's node follows started again from its first block (an easynet reset, v0.6.9). The
//! node's chain data is then the old chain's. Before the next start it moves aside (nothing is deleted), and the node
//! syncs the new chain from its peers; a node an older app left running on the old chain is started again for that.
//! The wallets keep their keys, passphrases and addresses. Their payments on the old chain stay in them, in no block and
//! in no balance, and the app's lists leave them out (`from_old_chain`: in no block, and seen before the move). They are not taken out of the wallets:
//! freebankd's `removeprunedfunds` leaves the wallet's ordered list pointing at what it removed (Core 0.16; fixed
//! upstream in bitcoin#13437), and the next `listtransactions` crashes the node (seen 2026-10-29); `-zapwallettxes` is
//! refused with more than one wallet.

use super::{detect, pin_hash, process, NodeManager, CHAIN_DATA, DATADIR_MARK};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The L1 block the folder's node was last started with (`-mainchainblockpin`'s hash), written before each start from
/// v0.6.9 on. Without it, freebankd's own cache says (`mainblockhash.dat`), which it writes only when it stops cleanly.
pub const PIN_RECORD: &str = ".freebank-pin";

/// When the chain data last moved aside (unix seconds), kept in the data folder: what the wallets saw before then, in no
/// block, is the old chain's. Written before the move, so a move cut short and finished at the next start keeps one.
pub const MOVED_RECORD: &str = ".freebank-moved";

/// The FreeBank network bytes the chain data last moved aside for (hex), kept in the data folder (v0.7.8). The data moves
/// aside once per chain: blocks with other bytes after a move for these were written by the node since, so the
/// release disagrees with the chain its release names, and nothing more moves (it would every 30 seconds).
pub const MOVED_FOR_RECORD: &str = ".freebank-moved-for";

/// This run's data folder's MOVED_RECORD (0: never moved), for the lists.
static MOVED_AT: AtomicU64 = AtomicU64::new(0);

/// Read the data folder's MOVED_RECORD: at launch, before each start and every 30 seconds (the lists have no folder).
pub fn load_moved(datadir: &Path) {
    let t = std::fs::read_to_string(datadir.join(MOVED_RECORD)).ok().and_then(|t| t.trim().parse::<u64>().ok());
    MOVED_AT.store(t.unwrap_or(0), Ordering::Relaxed);
}

/// The old chain's files besides CHAIN_DATA. The eCash block-hash cache goes last: it is how the old chain is told,
/// so a move cut short is finished at the next start.
const OLD_CHAIN_TOO: &[&str] = &["peers.dat", "banlist.dat", "fee_estimates.dat", "withdrawalid.dat", "mainblockhash.dat"];

/// The pinned eCash block this folder's chain data was made on, when that isn't the network's: its eCash chain started
/// again since. None for a folder that isn't FreeBank's, or whose chain is the network's.
pub fn old_chain(datadir: &Path) -> Option<String> {
    let n = crate::network::current();
    old_chain_on(datadir, n.pin_hash, n.restarted, checked_magic(n))
}

/// The network bytes FreeBank's chain data is checked against: none while the chain is in doubt (`chain_in_doubt`),
/// so nothing moves aside for them then.
fn checked_magic(n: &crate::network::Network) -> Option<[u8; 4]> {
    (!crate::network::chain_in_doubt(n)).then(|| crate::network::fb_magic(n)).flatten()
}

fn old_chain_on(datadir: &Path, pin: &str, restarted: Option<u64>, magic: Option<[u8; 4]>) -> Option<String> {
    if !datadir.join(DATADIR_MARK).is_file() {
        return None;
    }
    // FreeBank's own chain started again on the same eCash chain (easynet on v1.1.0): its blocks carry other bytes.
    // Not when the data already moved aside for this chain: then the node wrote them (`mismatch`).
    if let Some(m) = old_magic(datadir, magic) {
        return (!moved_for(datadir, magic)).then(|| format!("FreeBank's chain {}", hex::encode(m)));
    }
    if pin.is_empty() {
        return None;
    }
    let record = std::fs::read_to_string(datadir.join(PIN_RECORD)).ok().map(|t| t.trim().to_string());
    let was = record
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .or_else(|| detect::pin_entry(&datadir.join("mainblockhash.dat")).filter(|h| h.bytes().any(|b| b != b'0')));
    match was {
        Some(h) => (h != pin).then_some(h),
        // Neither (a folder from before v0.6.9 whose node never stopped cleanly): a block older than the restart is the
        // old chain's.
        None => (first_block_time(datadir)? < restarted?).then(|| "blocks older than the restart".to_string()),
    }
}

/// The time of the first block after the genesis in the folder's first block file (the block files hold blocks in the
/// order they came: each record is 4 network bytes, the block's size, then the block, whose header starts with its
/// version, the block before it, the merkle root and its time).
fn first_block_time(datadir: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(datadir.join("blocks").join("blk00000.dat")).ok()?;
    for _ in 0..8 {
        let mut head = [0u8; 80];
        f.read_exact(&mut head).ok()?;
        let size = u32::from_le_bytes(head[4..8].try_into().ok()?) as i64;
        if head[..4] == [0; 4] || size < 72 {
            return None;
        }
        if head[12..44].iter().any(|&b| b != 0) {
            return Some(u32::from_le_bytes(head[76..80].try_into().ok()?) as u64);
        }
        f.seek(SeekFrom::Current(size - 72)).ok()?;
    }
    None
}

/// The network bytes the folder's first block file starts with, when they aren't this network's (`fb_magic`): an
/// earlier FreeBank chain on the same eCash chain. None when the network doesn't say its bytes, or there's no block yet.
fn old_magic(datadir: &Path, magic: Option<[u8; 4]>) -> Option<[u8; 4]> {
    use std::io::Read;
    let want = magic?;
    let mut head = [0u8; 4];
    std::fs::File::open(datadir.join("blocks").join("blk00000.dat")).ok()?.read_exact(&mut head).ok()?;
    (head != [0; 4] && head != want).then_some(head)
}

/// Did the chain data already move aside for these network bytes (`MOVED_FOR_RECORD`)?
fn moved_for(datadir: &Path, magic: Option<[u8; 4]>) -> bool {
    let Some(m) = magic else { return false };
    std::fs::read_to_string(datadir.join(MOVED_FOR_RECORD)).is_ok_and(|t| t.trim() == hex::encode(m))
}

/// Blocks with other bytes than the network's, written after the data moved aside for the network's: the installed
/// release runs another chain than its release names. What to say, if so.
pub fn mismatch(datadir: &Path) -> Option<String> {
    let n = crate::network::current();
    let want = checked_magic(n);
    let got = old_magic(datadir, want)?;
    moved_for(datadir, want).then(|| {
        format!(
            "Your FreeBank node writes chain {}, but its release names {} for {}. Nothing more was moved aside; \
             please send a report from Settings › About.",
            hex::encode(got),
            hex::encode(want.unwrap_or_default()),
            n.name
        )
    })
}

/// `mismatch`, once in the activity log and on the Node tab (`chain_error`).
fn say_mismatch(mgr: &NodeManager, datadir: &Path) {
    if let Some(m) = mismatch(datadir) {
        let mut e = mgr.chain_error.lock().unwrap();
        if e.as_deref() != Some(m.as_str()) {
            crate::activity::note(&format!("node: {m}"));
            *e = Some(m);
        }
    }
}

/// Before each start, after `settle`: the pin this start uses.
pub fn remember(datadir: &Path) {
    if !pin_hash().is_empty() && datadir.join(DATADIR_MARK).is_file() {
        let _ = std::fs::write(datadir.join(PIN_RECORD), pin_hash());
    }
}

/// Before a start, with nothing using the folder: an old chain's data moves aside. The folder it went to, if it did.
pub fn settle(mgr: &NodeManager, datadir: &Path) -> Result<Option<String>, String> {
    if old_chain(datadir).is_none() {
        say_mismatch(mgr, datadir);
        tidy(datadir);
        return Ok(None);
    }
    let n = crate::network::current();
    let magic = old_magic(datadir, checked_magic(n));
    // Another network's FreeBank chain (mainnet's, beta's) is never moved aside for this one: the folder is the wrong one.
    if let Some(other) = magic.and_then(crate::network::other_chain) {
        return Err(format!(
            "The data folder {} holds FreeBank's {} chain, so FreeBank on {} won't use it. Choose another data folder \
             under Advanced.",
            datadir.display(),
            other,
            n.name
        ));
    }
    let freebank_only = magic.is_some();
    let t = now();
    std::fs::write(datadir.join(MOVED_RECORD), t.to_string()).map_err(|e| e.to_string())?;
    MOVED_AT.store(t, Ordering::Relaxed);
    let away = move_aside(datadir)?;
    if let (true, Some(want)) = (freebank_only, checked_magic(n)) {
        let _ = std::fs::write(datadir.join(MOVED_FOR_RECORD), hex::encode(want));
    }
    tidy(datadir);
    let why = if freebank_only {
        format!("FreeBank on {} started a new chain, so your node syncs it from its first block.", n.name)
    } else {
        format!("{} started again from its first block, so FreeBank's node starts its chain again too.", n.l1)
    };
    let said = format!(
        "{} Your wallets keep their keys and addresses; their payments on the old chain count for nothing now and \
         aren't listed. The old chain's data is in {}.",
        why,
        away.display()
    );
    let which = if freebank_only { "FreeBank's chain" } else { "the eCash chain" };
    crate::activity::note(&format!("node: {which} started again; the old chain's data moved to {}", away.display()));
    *mgr.restarted.lock().unwrap() = Some(said);
    Ok(Some(away.to_string_lossy().into_owned()))
}

/// How many earlier chains' data stay set aside beside a data folder; older ones are deleted at the next start, so
/// a network that starts again often doesn't fill the disk (v0.7.6). Only a folder holding nothing but chain data counts
/// (what `move_aside` moves): one holding a wallet, a config or anything else, such as setup's move of a whole earlier
/// folder or a restore's wallet, is never touched.
pub const KEEP_OLD_CHAINS: usize = 2;

/// The old chains' folders of `datadir`, newest first: `<folder>.old-<time>[-n]` beside it and `old-chain-<time>`
/// inside it, each a real folder (not a link) holding only chain data.
fn old_chain_folders(datadir: &Path) -> Vec<PathBuf> {
    let Ok(real) = datadir.canonicalize() else { return Vec::new() };
    let only_chain_data = |p: &Path| {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
            && std::fs::read_dir(p).is_ok_and(|d| {
                d.flatten().all(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    CHAIN_DATA.iter().chain(OLD_CHAIN_TOO).any(|k| *k == n)
                })
            })
    };
    // "<time>" or "<time>-<n>" (two moves in one second), as a sortable pair.
    let stamp = |rest: &str| -> Option<(u64, u64)> {
        if !crate::node::aside_stamp(rest) {
            return None;
        }
        let (t, n) = rest.split_once('-').unwrap_or((rest, "1"));
        Some((t.parse().ok()?, n.parse().ok()?))
    };
    let mut found: Vec<((u64, u64), PathBuf)> = Vec::new();
    let mut scan = |dir: &Path, prefix: &str| {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(k) = n.strip_prefix(prefix).and_then(stamp) {
                if only_chain_data(&e.path()) {
                    found.push((k, e.path()));
                }
            }
        }
    };
    if let (Some(parent), Some(name)) = (real.parent(), real.file_name()) {
        scan(parent, &format!("{}.old-", name.to_string_lossy()));
    }
    scan(&real, "old-chain-");
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// Delete the old chains' folders past the newest KEEP_OLD_CHAINS; what it deleted. A folder it can't delete stays
/// for the next start.
pub fn tidy(datadir: &Path) -> Vec<PathBuf> {
    let mut gone = Vec::new();
    for p in old_chain_folders(datadir).into_iter().skip(KEEP_OLD_CHAINS) {
        let mb = crate::stack::tree_bytes(&p) / 1_000_000;
        if std::fs::remove_dir_all(&p).is_ok() {
            crate::activity::note(&format!("node: deleted an old chain's data, {} ({mb} MB)", p.display()));
            gone.push(p);
        }
    }
    gone
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The old chain's data into a folder beside the data folder (`<folder>.old-<time>`), or inside it (`old-chain-<time>`)
/// when the data folder is a disk of its own: a move is a rename, which can't cross disks.
fn move_aside(datadir: &Path) -> Result<PathBuf, String> {
    let real = datadir.canonicalize().map_err(|e| format!("{}: {}", datadir.display(), e))?;
    let away = if same_disk_as_parent(&real) {
        PathBuf::from(detect::away_path(&real))
    } else {
        real.join(format!("old-chain-{}", now()))
    };
    std::fs::create_dir(&away).map_err(|e| format!("{}: {}", away.display(), e))?;
    for name in CHAIN_DATA.iter().chain(OLD_CHAIN_TOO) {
        let from = real.join(name);
        if std::fs::symlink_metadata(&from).is_ok() {
            std::fs::rename(&from, away.join(name)).map_err(|e| format!("{}: {}", from.display(), e))?;
        }
    }
    Ok(away)
}

#[cfg(unix)]
fn same_disk_as_parent(d: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(d), d.parent().map(std::fs::metadata)) {
        (Ok(a), Some(Ok(b))) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_disk_as_parent(_: &Path) -> bool {
    true
}

/// A wallet payment in no block that the wallet saw before the node's chain moved aside: the old chain's. It is in no
/// balance, and the lists leave it out.
pub fn from_old_chain(e: &Value) -> bool {
    old_payment(e, MOVED_AT.load(Ordering::Relaxed).max(crate::network::current().fb_since.unwrap_or(0)))
}

fn old_payment(e: &Value, moved: u64) -> bool {
    moved > 0
        && e["confirmations"].as_i64().is_some_and(|c| c <= 0)
        && e["timereceived"].as_u64().or(e["time"].as_u64()).is_some_and(|t| t < moved)
}

/// Made before the node's chain moved aside (unix seconds): the app's own records of the old chain (withdrawals).
pub fn before_move(t: u64) -> bool {
    let moved = MOVED_AT.load(Ordering::Relaxed);
    moved > 0 && t > 0 && t < moved
}

/// Every 30 seconds (with `recheck`): this app's node still running on the old chain (an older app left it running) is
/// started again once the eCash stack is the network's, so its old chain moves aside.
pub async fn restart_old(mgr: &NodeManager) {
    let s = mgr.settings.lock().await.clone();
    load_moved(Path::new(&s.datadir));
    if s.installed_tag.is_none() || crate::stack::installing() || !process::child_alive(mgr).await {
        return;
    }
    if let (Some(st), Some(want)) = (&s.stack, crate::network::current().stack) {
        if st.tag != want.tag {
            return;
        }
    }
    // A release older than the chain now needs (easynet's moved on, read from its newest release): starting it again
    // would be refused, so it's left as it is until Update node, which moves the old chain aside.
    if s.installed_tag.as_deref().is_some_and(|t| crate::network::below_min(crate::network::current(), t)) {
        return;
    }
    let Some(why) = old_chain(Path::new(&s.datadir)) else {
        say_mismatch(mgr, Path::new(&s.datadir));
        return;
    };
    crate::activity::note(&format!("node: it runs on an old chain ({why}); starting it again"));
    if let Err(e) = process::stop(mgr).await {
        crate::activity::note(&format!("node: stopping it to start again failed: {e}"));
        return;
    }
    if let Err(e) = process::start(mgr).await {
        crate::activity::note(&format!("node: starting it again failed: {e}"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    /// A data folder as freebankd leaves it, its block-hash cache holding `pin` at the pin height.
    fn folder(pin: &str) -> PathBuf {
        let d = crate::node::testnode::temp("restart");
        std::fs::create_dir_all(d.join("blocks/index")).unwrap();
        std::fs::create_dir_all(d.join("chainstate")).unwrap();
        for f in [DATADIR_MARK, "wallet.dat", "freebank.conf", "bmm.dat", "peers.dat", "withdrawalid.dat", "hosted-1"] {
            std::fs::write(d.join(f), f.as_bytes()).unwrap();
        }
        // Sparse: beta's pin is block 967,680.
        use std::io::{Seek, SeekFrom, Write};
        let mut cache = std::fs::File::create(d.join("mainblockhash.dat")).unwrap();
        cache.seek(SeekFrom::Start(12 + crate::node::pin_height() * 32)).unwrap();
        let mut h = hex::decode(pin).unwrap();
        h.reverse();
        cache.write_all(&h).unwrap();
        d
    }

    #[test]
    fn the_same_chain_is_left_as_it_is() {
        let d = folder(pin_hash());
        assert_eq!(old_chain(&d), None);
        let mgr = NodeManager::new(d.join("app"));
        assert_eq!(settle(&mgr, &d).unwrap(), None);
        assert!(d.join("blocks").is_dir() && d.join("mainblockhash.dat").is_file());
        assert!(mgr.restarted.lock().unwrap().is_none());
    }

    #[test]
    fn a_restarted_chain_moves_aside_and_the_wallets_stay() {
        let old = "00000001a4105dc30c636e71ac2d844caa32d6fa2e9aacc5b2225ea772dd9490";
        let d = folder(old);
        assert_eq!(old_chain(&d).as_deref(), Some(old));
        let mgr = NodeManager::new(d.join("app"));
        let away = settle(&mgr, &d).unwrap().expect("moved");
        for kept in [DATADIR_MARK, "wallet.dat", "freebank.conf", "hosted-1", MOVED_RECORD] {
            assert!(d.join(kept).exists(), "{} stays", kept);
        }
        for gone in ["blocks", "chainstate", "bmm.dat", "peers.dat", "withdrawalid.dat", "mainblockhash.dat"] {
            assert!(!d.join(gone).exists(), "{} moves", gone);
        }
        let said = mgr.restarted.lock().unwrap().clone().unwrap();
        assert!(said.ends_with(&format!(" is in {}.", away)), "{said}");
        let away = Path::new(&away);
        assert_eq!(away.parent(), d.canonicalize().unwrap().parent(), "beside the data folder");
        assert!(away.join("blocks/index").is_dir() && away.join("mainblockhash.dat").is_file());
        let moved: u64 = std::fs::read_to_string(d.join(MOVED_RECORD)).unwrap().parse().unwrap();
        assert!(moved > 1_790_000_000);
        // The next start: nothing more to move, and the move's time stays.
        *mgr.restarted.lock().unwrap() = None;
        assert_eq!(settle(&mgr, &d).unwrap(), None);
        assert!(mgr.restarted.lock().unwrap().is_none());
        assert_eq!(std::fs::read_to_string(d.join(MOVED_RECORD)).unwrap(), moved.to_string());
    }

    #[test]
    fn only_the_two_newest_old_chains_stay_and_nothing_with_a_wallet_goes() {
        let top = crate::node::testnode::temp("tidy");
        let d = top.join("easynet");
        std::fs::create_dir_all(d.join("blocks")).unwrap();
        std::fs::write(d.join("wallet.dat"), b"w").unwrap();
        let old = |name: &str, files: &[&str]| {
            let p = if name.starts_with("old-chain-") { d.join(name) } else { top.join(name) };
            for f in files {
                if f.contains('.') {
                    std::fs::create_dir_all(&p).unwrap();
                    std::fs::write(p.join(f), f.as_bytes()).unwrap();
                } else {
                    std::fs::create_dir_all(p.join(f)).unwrap();
                }
            }
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let a100 = old("easynet.old-100", &["blocks", "chainstate"]);
        let a200 = old("easynet.old-200", &["chainstate", "mainblockhash.dat", "peers.dat"]);
        let in250 = old("old-chain-250", &["blocks"]);
        let a300 = old("easynet.old-300", &["blocks"]);
        let a300b = old("easynet.old-300-2", &["blocks", "indexes"]);
        let empty = old("easynet.old-90", &[]);
        // Never: a folder with a wallet or a config (setup's move of a whole folder), a name that isn't a move's, a
        // link, and another network's.
        let wallet = old("easynet.old-50", &["blocks", "wallets"]);
        let conf = old("easynet.old-60", &["blocks", "freebank.conf"]);
        let odd = old("easynet.old-abc", &["blocks"]);
        let other = old("mainnet.old-10", &["blocks"]);
        let target = old("elsewhere", &["blocks"]);
        std::os::unix::fs::symlink(&target, top.join("easynet.old-20")).unwrap();

        assert_eq!(old_chain_folders(&d), vec![a300b.clone(), a300.clone(), in250.clone(), a200.clone(), a100.clone(), empty.clone()]);
        let gone = tidy(&d);
        assert_eq!(gone, vec![in250.clone(), a200.clone(), a100.clone(), empty.clone()]);
        assert!(a300b.is_dir() && a300.is_dir(), "the two newest stay");
        assert!(!in250.exists() && !a200.exists() && !a100.exists() && !empty.exists());
        for p in [&wallet, &conf, &odd, &other, &target] {
            assert!(p.join("blocks").is_dir(), "{} untouched", p.display());
        }
        assert!(top.join("easynet.old-20").exists() && d.join("wallet.dat").is_file() && d.join("blocks").is_dir());
        assert!(tidy(&d).is_empty(), "nothing more the next time");
    }

    #[test]
    fn a_folder_without_freebanks_mark_or_a_record_is_not_touched() {
        let d = folder("00000001a4105dc30c636e71ac2d844caa32d6fa2e9aacc5b2225ea772dd9490");
        std::fs::remove_file(d.join(DATADIR_MARK)).unwrap();
        assert_eq!(old_chain(&d), None);
        let d = folder(pin_hash());
        std::fs::write(d.join("mainblockhash.dat"), vec![0u8; 12]).unwrap();
        assert_eq!(old_chain(&d), None, "a cache too short to hold the pin");
        std::fs::File::create(d.join("mainblockhash.dat")).unwrap().set_len(12 + 32 * (crate::node::pin_height() + 1)).unwrap();
        assert_eq!(old_chain(&d), None, "a cache not filled in yet");
    }

    /// A block file holding FreeBank's genesis (no block before it) and then a block made at `t`.
    fn blocks(d: &Path, t: u32) {
        let mut f = Vec::new();
        for (prev, time) in [([0u8; 32], 1_791_800_000u32), ([7u8; 32], t)] {
            let mut b = vec![0u8; 4];
            b.extend_from_slice(&prev);
            b.extend_from_slice(&[1u8; 32]);
            b.extend_from_slice(&time.to_le_bytes());
            b.extend_from_slice(&[2u8; 100]);
            f.extend_from_slice(&[0xfb, 0x4b, 0xea, 0x03]);
            f.extend_from_slice(&(b.len() as u32).to_le_bytes());
            f.extend_from_slice(&b);
        }
        f.resize(f.len() + 4096, 0);
        std::fs::write(d.join("blocks/blk00000.dat"), f).unwrap();
    }

    #[test]
    fn the_record_of_the_last_start_goes_first() {
        let old = "00000001a4105dc30c636e71ac2d844caa32d6fa2e9aacc5b2225ea772dd9490";
        let new = "000000018a449eb24e2c55744a19ad02db5928fa89296f87cbb2059784b2b211";
        let d = folder(old);
        assert_eq!(old_chain_on(&d, new, None, None).as_deref(), Some(old), "the cache, without a record");
        std::fs::write(d.join(PIN_RECORD), new).unwrap();
        assert_eq!(old_chain_on(&d, new, None, None), None, "the record says this start's pin");
        std::fs::write(d.join(PIN_RECORD), old).unwrap();
        assert_eq!(old_chain_on(&d, new, None, None).as_deref(), Some(old));
        std::fs::write(d.join(PIN_RECORD), "torn").unwrap();
        assert_eq!(old_chain_on(&d, new, None, None).as_deref(), Some(old), "a record that isn't one: the cache says");
        assert_eq!(old_chain_on(&d, "", None, None), None, "a network with no pin (mainnet, not yet)");
        // `remember` writes the network's own pin (beta's in tests).
        remember(&d);
        assert_eq!(std::fs::read_to_string(d.join(PIN_RECORD)).unwrap(), pin_hash());
        assert_eq!(old_chain(&d), None);
    }

    #[test]
    fn without_a_record_or_a_cache_the_blocks_times_say() {
        let new = "000000018a449eb24e2c55744a19ad02db5928fa89296f87cbb2059784b2b211";
        let d = folder(new);
        std::fs::remove_file(d.join("mainblockhash.dat")).unwrap();
        assert_eq!(old_chain_on(&d, new, Some(1_793_250_719), None), None, "no block file");
        blocks(&d, 1_792_139_812);
        assert_eq!(first_block_time(&d), Some(1_792_139_812), "the first block after the genesis");
        assert!(old_chain_on(&d, new, Some(1_793_250_719), None).is_some(), "made before the restart");
        assert_eq!(old_chain_on(&d, new, None, None), None, "a network that never restarted");
        blocks(&d, 1_793_260_000);
        assert_eq!(old_chain_on(&d, new, Some(1_793_250_719), None), None, "the new chain's");
        std::fs::write(d.join("blocks/blk00000.dat"), vec![0u8; 4096]).unwrap();
        assert_eq!(first_block_time(&d), None, "an empty block file");
    }

    /// The block file of a FreeBank chain with these network bytes.
    fn blocks_with(d: &Path, magic: [u8; 4]) {
        blocks(d, 1_793_760_000);
        let mut f = std::fs::read(d.join("blocks/blk00000.dat")).unwrap();
        f[..4].copy_from_slice(&magic);
        std::fs::write(d.join("blocks/blk00000.dat"), f).unwrap();
    }

    #[test]
    fn a_new_freebank_chain_on_the_same_ecash_chain_moves_the_old_one_aside() {
        let pin = "000000018a449eb24e2c55744a19ad02db5928fa89296f87cbb2059784b2b211";
        let new = Some([0xfb, 0x4b, 0xea, 0x05]);
        let d = folder(pin);
        std::fs::write(d.join(PIN_RECORD), pin).unwrap();
        assert_eq!(old_chain_on(&d, pin, None, new), None, "no block file yet");
        blocks_with(&d, [0xfb, 0x4b, 0xea, 0x03]);
        assert_eq!(old_chain_on(&d, pin, None, new).as_deref(), Some("FreeBank's chain fb4bea03"), "easynet's v1.0.x chain");
        assert_eq!(old_chain_on(&d, pin, None, None), None, "a network that doesn't say its bytes");
        assert_eq!(old_chain_on(&d, "", None, new).as_deref(), Some("FreeBank's chain fb4bea03"), "whatever the pin");
        // Once per chain (v0.7.8): after a move for fb4bea05, other bytes were written by the node since.
        std::fs::write(d.join(MOVED_FOR_RECORD), "fb4bea05").unwrap();
        assert_eq!(old_chain_on(&d, pin, None, new), None, "moved for this chain already");
        assert!(moved_for(&d, new) && !moved_for(&d, Some([0xfb, 0x4b, 0xea, 0x06])) && !moved_for(&d, None));
        std::fs::write(d.join(MOVED_FOR_RECORD), "fb4bea04").unwrap();
        assert_eq!(old_chain_on(&d, pin, None, new).as_deref(), Some("FreeBank's chain fb4bea03"), "moved for an earlier one");
        std::fs::remove_file(d.join(MOVED_FOR_RECORD)).unwrap();
        blocks_with(&d, [0xfb, 0x4b, 0xea, 0x05]);
        assert_eq!(old_chain_on(&d, pin, None, new), None, "the new chain's own blocks");
        std::fs::write(d.join("blocks/blk00000.dat"), vec![0u8; 4096]).unwrap();
        assert_eq!(old_chain_on(&d, pin, None, new), None, "a block file not written yet");
        std::fs::remove_file(d.join(DATADIR_MARK)).unwrap();
        blocks_with(&d, [0xfb, 0x4b, 0xea, 0x03]);
        assert_eq!(old_chain_on(&d, pin, None, new), None, "a folder that isn't FreeBank's");
    }

    #[test]
    fn only_the_old_chains_payments_are_left_out() {
        let r = 150;
        for (e, old) in [
            (json!({"txid": "a", "confirmations": 0, "timereceived": 100}), true),
            (json!({"txid": "b", "confirmations": -3, "time": 120}), true),
            (json!({"txid": "c", "confirmations": 2, "timereceived": 100}), false),
            (json!({"txid": "d", "confirmations": 0, "timereceived": 200}), false),
            (json!({"txid": "e", "confirmations": 0}), false),
            (json!({"category": "move", "time": 100}), false),
        ] {
            assert_eq!(old_payment(&e, r), old, "{e}");
        }
        assert!(!old_payment(&json!({"txid": "a", "confirmations": 0, "timereceived": 100}), 0), "never moved");
    }

    #[test]
    fn easynets_old_chain_payments_are_left_out_without_a_move() {
        // An older app's node update reindexes onto the new chain over the old block files: no move is recorded, so the
        // new chain's start dates the old chain's payments.
        let since = crate::network::EASYNET.fb_since.expect("easynet's new chain has a start");
        assert!(since > 1_793_750_400, "after the genesis time");
        assert!(old_payment(&json!({"txid": "a", "confirmations": 0, "timereceived": 1_793_811_012}), since), "old chain's block 985");
        assert!(!old_payment(&json!({"txid": "b", "confirmations": 0, "timereceived": since + 60}), since), "new chain");
        assert!(!old_payment(&json!({"txid": "c", "confirmations": 3, "timereceived": 1_793_811_012}), since), "in a block");
        assert_eq!(crate::network::MAINNET.fb_since, None);
    }
}
