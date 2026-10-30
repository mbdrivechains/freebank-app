//! Blocks marked invalid while the eCash view was behind (v0.6.8). After an unclean stop (a power cut) the FreeBank node
//! replays its last blocks as it starts, against what the eCash node knows then. A block whose BMM names an L1 block
//! that node hasn't reached yet is refused ("bad-mc-prev, L1 block … is not on the L1's main chain") and marked invalid,
//! and from then on the node refuses it from every peer: on 2026-10-25 it redialled the seed for eleven hours. Its start
//! waits for the eCash node (`process::start`), but not for ever. So while the app runs: once this run's debug.log says
//! a block failed bad-mc-prev and the eCash view has caught up, each such block (and only those) is checked again
//! (`reconsiderblock`), once each, and the Node tab says so. A block that really is bad is refused again.
//!
//! Blocks an earlier release refused (v0.6.11, `after_release`). A newer release of the node can take a block an older
//! one refused, and the node keeps a refused block marked invalid through an update: on 2026-10-30 easynet's block 219
//! (the first one FreeBank v1.0.1 makes, which credits deposits) was refused by v0.2.29, and after the update to
//! v1.0.1 the node still stopped at 218. So once for each release the app runs its node with (an update, or this
//! version's first start), the blocks its log says an earlier release refused, and the branches past its chain that
//! it doesn't follow, are checked again. (A refused block is a chain tip only until a peer's headers build on it, so
//! `getchaintips` alone doesn't name it; checking a branch's tip again clears the blocks under it too.) The peers it
//! banned for sending them are let back in: easynet's FreeBank has a single peer, banned for a day.

use super::{detect, process, NodeManager};
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// `listbanned`'s reason for a ban the node set itself (a peer sent a block it refused, or headers on one).
const MISBEHAVING: &str = "node misbehaving";
/// freebankd's line for a block it refused, with the block's hash and height after it (validation.cpp).
const INVALID: &str = "InvalidChainFound: invalid block=";
/// What the node's log says when its eCash view didn't have a block's L1 block (freebankd's ConnectTip).
const BAD_L1: &str = "bad-mc-prev";
/// freebankd's line for a block it couldn't connect, with the block's hash after it (validation.cpp, ConnectTip).
const CONNECT_FAILED: &str = "ConnectBlock ";
/// The first line freebankd writes as it starts (init.cpp): where a run's part of its log begins.
const STARTED: &str = "FreeBank version v";

#[derive(Debug, Default)]
pub struct Recheck {
    /// Where the node's log ended when it was started (`process::LogMark`), and how far it has been read since.
    base: Option<u64>,
    scanned: u64,
    /// Blocks the log says failed bad-mc-prev as the node connected them, not yet checked again.
    pending: Vec<String>,
    /// Blocks checked again in this run of the app.
    done: Vec<String>,
    /// What the Node tab says once one was.
    pub said: Option<String>,
}

/// Every 30 seconds while the app runs.
pub fn spawn(mgr: Arc<NodeManager>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            super::restart::restart_old(&mgr).await;
            look(&mgr).await;
        }
    });
}

async fn look(mgr: &NodeManager) {
    let s = mgr.settings.lock().await.clone();
    // Only the node this app runs (or took over from its last launch).
    if s.installed_tag.is_none() || !process::child_alive(mgr).await {
        return;
    }
    let datadir = PathBuf::from(&s.datadir);
    let mark = mgr.log_mark.lock().unwrap().map(|m| m.debug_log);
    let pending = scan(mgr, &datadir, mark);
    let new_release = s.tips_checked != s.installed_tag;
    if pending.is_empty() && !new_release {
        return;
    }
    let caught_up = match crate::stack::caught_up(mgr).await {
        Some(c) => c,
        // Another program's eCash node (BitWindow's): what its REST says.
        None => detect::l1_caught_up(&mgr.http, &s.rest).await,
    };
    if !caught_up {
        return;
    }
    let c = detect::local_client(&mgr.http, &s);
    if new_release {
        after_release(mgr, &c, &datadir, s.installed_tag.as_deref().unwrap_or_default()).await;
    }
    let mut checked = Vec::new();
    for hash in pending {
        {
            let mut rc = mgr.recheck.lock().unwrap();
            rc.pending.retain(|h| *h != hash);
            rc.done.push(hash.clone());
        }
        let height = c.call("getblockheader", vec![json!(hash)]).await.ok().and_then(|h| h["height"].as_u64());
        let r = c.call("reconsiderblock", vec![json!(hash)]).await;
        crate::activity::note(&format!("node: block {hash} failed bad-mc-prev while the eCash view was behind: checked again ({})", match &r {
            Ok(_) => "done".to_string(),
            Err(e) => e.to_string(),
        }));
        if let Some(h) = height {
            checked.push(h);
        }
    }
    if !checked.is_empty() {
        checked.sort_unstable();
        say(mgr, words(&checked));
    }
}

/// Checked again once with this release (`tag`): the blocks an earlier release refused (its log) that aren't on the
/// node's chain, and the branches past its chain (`past_tip`). The Node tab names the refused blocks it then took.
async fn after_release(mgr: &NodeManager, c: &crate::rpc::FreeBankClient, datadir: &Path, tag: &str) {
    // Still loading its blocks: next time.
    let Ok(tips) = c.call("getchaintips", vec![]).await else { return };
    let mut refused = Vec::new();
    for (hash, height) in earlier_refusals(&read_log(datadir), tag) {
        if !on_chain(c, &hash).await {
            refused.push((hash, height));
        }
    }
    let mut again: Vec<String> = refused.iter().map(|(h, _)| h.clone()).collect();
    for hash in past_tip(&tips) {
        if !again.contains(&hash) {
            again.push(hash);
        }
    }
    for hash in &again {
        let r = c.call("reconsiderblock", vec![json!(hash)]).await;
        crate::activity::note(&format!("node: block {hash}, refused by an earlier release or under a branch it didn't follow, checked again by {tag} ({})", match &r {
            Ok(_) => "done".to_string(),
            Err(e) => e.to_string(),
        }));
    }
    if !again.is_empty() {
        unban(c, tag).await;
    }
    let mut taken = Vec::new();
    for (hash, height) in &refused {
        if on_chain(c, hash).await {
            taken.push(*height);
        }
    }
    let mut s = mgr.settings.lock().await.clone();
    s.tips_checked = Some(tag.to_string());
    let _ = mgr.save_settings(s).await;
    if !taken.is_empty() {
        taken.sort_unstable();
        say(mgr, words_release(&taken, tag));
    }
}

/// The peers the node banned itself: a peer it banned for a block it refused may well have been right. Bans set by
/// hand are kept. After the blocks are checked again, so a peer let back in isn't banned again for the same block.
async fn unban(c: &crate::rpc::FreeBankClient, tag: &str) {
    let Ok(list) = c.call("listbanned", vec![]).await else { return };
    for b in list.as_array().into_iter().flatten().filter(|b| b["ban_reason"] == MISBEHAVING) {
        let Some(addr) = b["address"].as_str() else { continue };
        let r = c.call("setban", vec![json!(addr), json!("remove")]).await;
        crate::activity::note(&format!("node: peer {addr}, banned by the node, let back in with {tag} ({})", match &r {
            Ok(_) => "done".to_string(),
            Err(e) => e.to_string(),
        }));
    }
}

async fn on_chain(c: &crate::rpc::FreeBankClient, hash: &str) -> bool {
    let h = c.call("getblockheader", vec![json!(hash)]).await;
    h.ok().and_then(|h| h["confirmations"].as_i64()).is_some_and(|n| n > 0)
}

/// The node's log, its last 64 MiB.
fn read_log(datadir: &Path) -> String {
    let Ok(mut f) = std::fs::File::open(datadir.join("debug.log")) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(64 << 20))).is_ok() {
        let _ = f.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The blocks the log says a release other than `tag` refused, each once: hash and height. Lines before the log's
/// first start line (a log freebankd shrank) are left out: which release wrote them isn't known.
fn earlier_refusals(log: &str, tag: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    let mut other: Option<bool> = None;
    for line in log.lines() {
        if let Some(after) = line.split(STARTED).nth(1) {
            // "FreeBank version v0.2.29.0-gdf0e250… (release build)": v0.2.29.
            let v = format!("v{}", after.split_whitespace().next().unwrap_or(""));
            other = Some(!(v == tag || v.starts_with(&format!("{tag}.")) || v.starts_with(&format!("{tag}-"))));
        } else if other == Some(true) {
            let Some(after) = line.split(INVALID).nth(1) else { continue };
            let hash: String = after.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            let height = after.split("height=").nth(1).and_then(|h| h.split_whitespace().next()).and_then(|h| h.parse().ok());
            if let (64, Some(height)) = (hash.len(), height) {
                if !out.iter().any(|(h, _)| *h == hash) {
                    out.push((hash, height));
                }
            }
        }
    }
    out
}

/// The branches past the node's chain it doesn't follow, from `getchaintips`: each one's tip. A branch there whose
/// blocks it has refused, or that build on one it refused, is shown as invalid or as headers only.
fn past_tip(tips: &serde_json::Value) -> Vec<String> {
    let list = tips.as_array().map(Vec::as_slice).unwrap_or_default();
    let Some(active) = list.iter().find(|t| t["status"] == "active").and_then(|t| t["height"].as_u64()) else { return Vec::new() };
    list.iter()
        .filter(|t| ["invalid", "headers-only", "valid-headers"].iter().any(|s| t["status"] == *s))
        .filter(|t| t["height"].as_u64().is_some_and(|h| h > active))
        .filter_map(|t| t["hash"].as_str().map(String::from))
        .collect()
}

/// What the Node tab says, after anything it already said in this run.
fn say(mgr: &NodeManager, text: String) {
    let mut rc = mgr.recheck.lock().unwrap();
    rc.said = Some(match rc.said.take() {
        Some(before) if before != text => format!("{before} {text}"),
        _ => text,
    });
}

/// Read what the node's log added since the last look: from its start (`mark`), or for a node taken over from an
/// earlier launch, from its last 4 MiB. A log freebankd shrank as it started (over 11 MB, it keeps the last 10) is
/// read from that start's first line. The blocks waiting for a second look.
fn scan(mgr: &NodeManager, datadir: &Path, mark: Option<u64>) -> Vec<String> {
    let mut rc = mgr.recheck.lock().unwrap();
    let Ok(mut f) = std::fs::File::open(datadir.join("debug.log")) else { return rc.pending.clone() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let base = mark.unwrap_or_else(|| rc.base.unwrap_or(len.saturating_sub(4 << 20)));
    if rc.base != Some(base) || len < rc.scanned {
        // A new start of the node (or a log it shrank): this run's part, from its start.
        rc.base = Some(base);
        rc.scanned = if len >= base { base } else { last_start(&mut f).unwrap_or(0) };
    }
    // Whole lines only, 4 MiB at most at a time: a line still being written is read whole next time.
    let from = rc.scanned;
    let until = len.min(from + (4 << 20));
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_ok() && f.take(until - from).read_to_end(&mut buf).is_ok() {
        let whole = match buf.iter().rposition(|&b| b == b'\n') {
            Some(i) => i + 1,
            None if buf.len() >= 4 << 20 => buf.len(),
            None => 0,
        };
        rc.scanned = from + whole as u64;
        for hash in failed_bad_l1(&String::from_utf8_lossy(&buf[..whole])) {
            if !rc.done.contains(&hash) && !rc.pending.contains(&hash) {
                rc.pending.push(hash);
            }
        }
    }
    rc.pending.clone()
}

/// Where the last run's first line starts in a log.
fn last_start(f: &mut std::fs::File) -> Option<u64> {
    let mut text = Vec::new();
    f.seek(SeekFrom::Start(0)).ok()?;
    f.take(64 << 20).read_to_end(&mut text).ok()?;
    let at = String::from_utf8_lossy(&text).rfind(STARTED)?;
    // The start of that line.
    Some(text[..at].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1) as u64)
}

/// The blocks freebankd couldn't connect for bad-mc-prev: `ConnectBlock <hash> failed. Reason: bad-mc-prev, …`.
fn failed_bad_l1(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines().filter(|l| l.contains(BAD_L1)) {
        let Some(after) = line.split(CONNECT_FAILED).nth(1) else { continue };
        let hash: String = after.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if hash.len() == 64 && !out.contains(&hash) {
            out.push(hash);
        }
    }
    out
}

fn words(heights: &[u64]) -> String {
    let list = heights.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(", ");
    let (which, it) = if heights.len() == 1 { (format!("block {list}"), "it") } else { (format!("blocks {list}"), "them") };
    format!(
        "FreeBank's node had marked {which} as bad while its eCash node was behind (after an unclean stop, such as a \
         power cut). Once the eCash node caught up, FreeBank checked {it} again."
    )
}

fn words_release(heights: &[u64], tag: &str) -> String {
    let list = heights.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(", ");
    let (which, it) = if heights.len() == 1 { (format!("block {list}"), "it") } else { (format!("blocks {list}"), "them") };
    format!("An earlier release of FreeBank's node had refused {which}, and the node kept refusing {it} after the update. FreeBank had {tag} check {it} again, and {tag} took {it}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "ae9bd53d00000000000000000000000000000000000000000000000000001228";

    fn failed(hash: &str) -> String {
        format!("2026-10-25T10:35:43Z ERROR: ConnectTip(): ConnectBlock {hash} failed. Reason: bad-mc-prev, L1 block 000000006f20 is not on the L1's main chain (code 16)\n")
    }

    #[test]
    fn the_log_lines_and_the_words() {
        assert_eq!(failed_bad_l1(&failed(H)), vec![H.to_string()]);
        // A peer's block refused for bad-mc-prev isn't one the node marked while connecting: not taken.
        assert!(failed_bad_l1("ERROR: AcceptBlock: bad-mc-prev, invalid mainchin prevBlock commit (code 16)\n").is_empty());
        assert!(failed_bad_l1("ConnectTip(): ConnectBlock abc failed. Reason: bad-mc-prev\n").is_empty(), "a whole hash only");
        assert!(failed_bad_l1(&format!("ConnectTip(): ConnectBlock {H} failed. Reason: bad-txns\n")).is_empty());
        assert!(words(&[1228]).contains("marked block 1228 as bad") && words(&[1228]).ends_with("checked it again."));
        assert!(words(&[1228, 1229]).contains("blocks 1228, 1229") && words(&[1228, 1229]).ends_with("checked them again."));
    }

    #[test]
    fn the_branches_past_the_chain() {
        let tips = json!([
            {"height": 241, "hash": "e".repeat(64), "branchlen": 23, "status": "headers-only"},
            {"height": 218, "hash": "a".repeat(64), "branchlen": 0, "status": "active"},
            {"height": 219, "hash": "b".repeat(64), "branchlen": 1, "status": "invalid"},
            {"height": 230, "hash": "c".repeat(64), "branchlen": 4, "status": "valid-fork"},
            {"height": 212, "hash": "d".repeat(64), "branchlen": 1, "status": "invalid"},
        ]);
        assert_eq!(past_tip(&tips), vec!["e".repeat(64), "b".repeat(64)], "past 218, and not a branch it followed before");
        assert!(past_tip(&json!({"code": -28})).is_empty(), "not a list: nothing");
    }

    #[test]
    fn the_blocks_an_earlier_release_refused() {
        let b = "75e94655fb0caafd170278fa9892929b9470961df46fff1d91856af70ee0ced8";
        let refused = |hash: &str, h: u32| format!("2026-10-30 07:36:16 InvalidChainFound: invalid block={hash}  height={h}  date=2026-10-30 03:28:08\n");
        let log = format!(
            "{}2026-10-30 07:36:06 FreeBank version v0.2.29.0-gdf0e2502fbdbe8706c1901968e053fcebc63916b (release build)\n{}{}\
             2026-10-30 08:00:00 FreeBank version v1.0.1.0-g1234567 (release build)\n{}",
            refused(&"1".repeat(64), 100),
            refused(b, 219),
            refused(b, 219),
            refused(&"2".repeat(64), 230)
        );
        assert_eq!(earlier_refusals(&log, "v1.0.1"), vec![(b.to_string(), 219)], "v0.2.29's, once; none from before the first start line, or from v1.0.1");
        assert_eq!(earlier_refusals(&log, "v0.2.29"), vec![("2".repeat(64), 230)]);
        assert!(earlier_refusals(&log, "v0.2.2").len() == 2, "v0.2.2 isn't v0.2.29");
    }

    #[test]
    fn the_words_after_a_release() {
        let w = words_release(&[219], "v1.0.1");
        assert!(w.contains("had refused block 219,") && w.ends_with("v1.0.1 took it."), "{w}");
        assert!(words_release(&[219, 227], "v1.0.1").ends_with("took them."));
    }

    #[test]
    fn what_the_node_tab_says_adds_up() {
        let d = std::env::temp_dir().join(format!("fb-recheck-say-{}", std::process::id()));
        let mgr = NodeManager::new(d);
        say(&mgr, "One.".into());
        say(&mgr, "One.".into());
        say(&mgr, "Two.".into());
        assert_eq!(mgr.recheck.lock().unwrap().said.as_deref(), Some("One. Two."));
    }

    #[test]
    fn a_pid_file_left_behind_means_an_unclean_stop() {
        let d = std::env::temp_dir().join(format!("fb-recheck-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!process::stopped_uncleanly(&d), "no pid file: a clean stop, or never started");
        std::fs::write(d.join("freebankd.pid"), format!("{}\n", std::process::id())).unwrap();
        assert!(process::stopped_uncleanly(&d), "left behind, whatever process has that number now: no node holds the folder");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_log_is_read_from_the_nodes_start_once() {
        let d = std::env::temp_dir().join(format!("fb-recheck-log-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let old = failed(&"1".repeat(64));
        std::fs::write(d.join("debug.log"), &old).unwrap();
        let mgr = NodeManager::new(d.clone());
        let mark = Some(old.len() as u64);
        assert!(scan(&mgr, &d, mark).is_empty(), "an earlier run's lines don't count");
        std::fs::write(d.join("debug.log"), format!("{old}{}", failed(H))).unwrap();
        assert_eq!(scan(&mgr, &d, mark), vec![H.to_string()]);
        mgr.recheck.lock().unwrap().pending.clear();
        mgr.recheck.lock().unwrap().done.push(H.into());
        std::fs::write(d.join("debug.log"), format!("{old}{}{}", failed(H), failed(H))).unwrap();
        assert!(scan(&mgr, &d, mark).is_empty(), "read once, and a block checked again isn't taken again");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_log_shrunk_at_the_start_is_read_from_that_start() {
        let d = std::env::temp_dir().join(format!("fb-recheck-shrunk-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mgr = NodeManager::new(d.clone());
        // The app marked the end of a 20 MB log; the node then kept only its tail, and started.
        let mark = Some(20 << 20);
        let earlier = format!("FreeBank version v0.2.29\n{}", failed(&"2".repeat(64)));
        std::fs::write(d.join("debug.log"), format!("{earlier}2026-10-25T10:35:40Z FreeBank version v0.2.29\n{}", failed(H))).unwrap();
        assert_eq!(scan(&mgr, &d, mark), vec![H.to_string()], "this run's lines only, from its first");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
