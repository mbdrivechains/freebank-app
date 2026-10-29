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
    old_chain_on(datadir, n.pin_hash, n.restarted)
}

fn old_chain_on(datadir: &Path, pin: &str, restarted: Option<u64>) -> Option<String> {
    if pin.is_empty() || !datadir.join(DATADIR_MARK).is_file() {
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

/// Before each start, after `settle`: the pin this start uses.
pub fn remember(datadir: &Path) {
    if !pin_hash().is_empty() && datadir.join(DATADIR_MARK).is_file() {
        let _ = std::fs::write(datadir.join(PIN_RECORD), pin_hash());
    }
}

/// Before a start, with nothing using the folder: an old chain's data moves aside. The folder it went to, if it did.
pub fn settle(mgr: &NodeManager, datadir: &Path) -> Result<Option<String>, String> {
    if old_chain(datadir).is_none() {
        return Ok(None);
    }
    let t = now();
    std::fs::write(datadir.join(MOVED_RECORD), t.to_string()).map_err(|e| e.to_string())?;
    MOVED_AT.store(t, Ordering::Relaxed);
    let away = move_aside(datadir)?;
    let said = format!(
        "{} started again from its first block, so FreeBank's node starts its chain again too. Your wallets keep their \
         keys and addresses; their payments on the old chain count for nothing now and aren't listed. The old chain's \
         data is in {}.",
        crate::network::current().l1,
        away.display()
    );
    crate::activity::note(&format!("node: the eCash chain started again; the old chain's data moved to {}", away.display()));
    *mgr.restarted.lock().unwrap() = Some(said);
    Ok(Some(away.to_string_lossy().into_owned()))
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
    old_payment(e, MOVED_AT.load(Ordering::Relaxed))
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
    if old_chain(Path::new(&s.datadir)).is_none() {
        return;
    }
    crate::activity::note("node: it runs on the eCash chain from before it started again; starting it again");
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
        assert_eq!(old_chain_on(&d, new, None).as_deref(), Some(old), "the cache, without a record");
        std::fs::write(d.join(PIN_RECORD), new).unwrap();
        assert_eq!(old_chain_on(&d, new, None), None, "the record says this start's pin");
        std::fs::write(d.join(PIN_RECORD), old).unwrap();
        assert_eq!(old_chain_on(&d, new, None).as_deref(), Some(old));
        std::fs::write(d.join(PIN_RECORD), "torn").unwrap();
        assert_eq!(old_chain_on(&d, new, None).as_deref(), Some(old), "a record that isn't one: the cache says");
        assert_eq!(old_chain_on(&d, "", None), None, "a network with no pin (mainnet, not yet)");
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
        assert_eq!(old_chain_on(&d, new, Some(1_793_250_719)), None, "no block file");
        blocks(&d, 1_792_139_812);
        assert_eq!(first_block_time(&d), Some(1_792_139_812), "the first block after the genesis");
        assert!(old_chain_on(&d, new, Some(1_793_250_719)).is_some(), "made before the restart");
        assert_eq!(old_chain_on(&d, new, None), None, "a network that never restarted");
        blocks(&d, 1_793_260_000);
        assert_eq!(old_chain_on(&d, new, Some(1_793_250_719)), None, "the new chain's");
        std::fs::write(d.join("blocks/blk00000.dat"), vec![0u8; 4096]).unwrap();
        assert_eq!(first_block_time(&d), None, "an empty block file");
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
}
