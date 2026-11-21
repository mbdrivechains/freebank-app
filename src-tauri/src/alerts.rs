//! Update alerts (v0.7.6). Forks will happen, as few as possible, and a node that misses one doesn't fail loudly: it
//! marks the new blocks invalid and stays where it was, while its peers move on. Each node already says so in
//! getblockchaininfo's `warnings` (FreeBank's: "We do not appear to fully agree with our peers! You may need to
//! upgrade…", once an invalid chain has six blocks' more work; also unknown new rules, disk space and a wrong clock).
//! The app never showed that field. Now a banner on every screen does, for FreeBank's node and for the eCash node the
//! app uses, for as long as the reason holds; it has no dismiss button.
//!
//! - **Warnings:** the node's own text, as it gives it. Left out: the pre-release notice that every build not marked a
//!   release carries (the app shows its own notice), and "testsafemode enabled".
//! - **Stalled:** a node out of initial sync whose headers stay ahead of its blocks for 30 minutes isn't adding blocks
//!   it already knows of.
//! - **Quiet:** a node with nothing left to download whose newest block is two hours old hears no new ones: its
//!   network has paused, or it can't reach it (a Mac on 2026-11-11: "In sync" at 284 for hours while easynet was
//!   down, its one "peer" a relay with nothing behind it). With how many peers it has that finished the handshake.
//!
//! Only the user's own nodes are asked: no explorer or public tip, so the check works the same with no service built
//! in.

use crate::commands::ClientState;
use crate::node::NodeManager;
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;

/// Headers ahead of blocks for this long, out of initial sync: stalled.
pub const STALL: Duration = Duration::from_secs(30 * 60);
/// No new block for this long, with nothing left to download: quiet. eCash's blocks aim at ten minutes, and FreeBank's
/// ride on them, so two hours without one is a pause or a lost connection, not chance (by luck, about one in 160,000).
pub const QUIET: u64 = 2 * 60 * 60;
/// The banner waits this long before saying so: a node just started hears of newer blocks within a minute or two.
pub const QUIET_HOLD: Duration = Duration::from_secs(5 * 60);
/// Each node gets this long to answer (an eCash node on another computer may be unreachable).
const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    /// "freebank" or "ecash": whose node.
    pub node: &'static str,
    /// "warning" (the node's own text), "stalled" or "quiet".
    pub kind: &'static str,
    /// The node's warning; otherwise empty.
    pub text: String,
    /// For "stalled": the node's blocks and headers. For "quiet": its blocks.
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
    /// For "quiet": its newest block's time (unix seconds), whole hours since then, and its peers.
    pub tip_time: Option<u64>,
    pub hours: Option<u64>,
    pub peers: Option<u64>,
}

impl Alert {
    fn new(node: &'static str, kind: &'static str) -> Self {
        Alert { node, kind, text: String::new(), blocks: None, headers: None, tip_time: None, hours: None, peers: None }
    }
}

/// What one node said: getblockchaininfo, its newest block's time and its peers that finished the handshake.
#[derive(Debug, Default)]
pub struct Reading {
    pub info: Value,
    pub tip_time: Option<u64>,
    pub peers: Option<u64>,
}

/// Peers that finished the handshake (v0.7.6). getpeerinfo also lists a connection that never answered, as a relay
/// with nothing behind it does (version 0, no user agent): counted, it read "1 peer" with no one to sync from.
pub fn handshaken(peerinfo: &Value) -> Vec<&Value> {
    peerinfo.as_array().map_or_else(Vec::new, |a| a.iter().filter(|p| p["version"].as_i64().unwrap_or(0) > 0).collect())
}

/// Whole hours since the node's newest block, once that is `QUIET` old with nothing left to download. None at block 0
/// (a new node still finding peers: the Node tab says that) and while headers run ahead (downloading, or stalled).
pub fn quiet_hours(blocks: u64, headers: u64, tip_time: Option<u64>, now: u64) -> Option<u64> {
    let age = now.saturating_sub(tip_time?);
    (blocks > 0 && headers <= blocks && age >= QUIET).then_some(age / 3600)
}

/// The newest block's time: getblockchaininfo's `time` (eCash's node has it), else from getblockheader.
pub fn tip_time_of(info: &Value, header: Option<&Value>) -> Option<u64> {
    info["time"].as_u64().or_else(|| header.and_then(|h| h["time"].as_u64()))
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Warnings that aren't news: the pre-release notice (FreeBank's "This is a pre-release build of experimental
/// software…", Core's "This is a pre-release test build…") and Core's test switch.
fn ignored(w: &str) -> bool {
    w.starts_with("This is a pre-release") || w == "testsafemode enabled"
}

/// getblockchaininfo's (or getnetworkinfo's) `warnings`: one string on FreeBank's node, a list on eCash's (Core 28 on).
pub fn warnings_of(info: &Value) -> Vec<String> {
    let all: Vec<&str> = match &info["warnings"] {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(|w| w.as_str()).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = Vec::new();
    for w in all.into_iter().map(str::trim).filter(|w| !w.is_empty() && !ignored(w)) {
        if !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Whether the node has been stalled for `STALL`, given when it was first seen behind (`since`, kept between calls).
pub fn stalled(info: &Value, since: &mut Option<Instant>, now: Instant) -> Option<(u64, u64)> {
    let blocks = info["blocks"].as_u64();
    let headers = info["headers"].as_u64();
    let ibd = info["initialblockdownload"].as_bool().unwrap_or(false);
    match (blocks, headers) {
        (Some(b), Some(h)) if !ibd && h > b => {
            let first = *since.get_or_insert(now);
            (now.duration_since(first) >= STALL).then_some((b, h))
        }
        _ => {
            *since = None;
            None
        }
    }
}

/// Whether the node has been quiet for `QUIET_HOLD` on end, given when it was first seen quiet (`since`): its hours.
pub fn quiet(r: &Reading, since: &mut Option<Instant>, now: Instant, unix: u64) -> Option<u64> {
    let (b, h) = (r.info["blocks"].as_u64(), r.info["headers"].as_u64());
    match (b, h) {
        (Some(b), Some(h)) => match quiet_hours(b, h, r.tip_time, unix) {
            Some(hours) => {
                let first = *since.get_or_insert(now);
                (now.duration_since(first) >= QUIET_HOLD).then_some(hours)
            }
            None => {
                *since = None;
                None
            }
        },
        _ => {
            *since = None;
            None
        }
    }
}

/// When a node was first seen behind (headers ahead of blocks) and first seen quiet.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Seen {
    pub behind: Option<Instant>,
    pub quiet: Option<Instant>,
}

/// The alerts for one node's reading.
pub fn of(node: &'static str, r: &Reading, seen: &mut Seen, now: Instant, unix: u64) -> Vec<Alert> {
    let mut out: Vec<Alert> =
        warnings_of(&r.info).into_iter().map(|text| Alert { text, ..Alert::new(node, "warning") }).collect();
    if let Some((b, h)) = stalled(&r.info, &mut seen.behind, now) {
        out.push(Alert { blocks: Some(b), headers: Some(h), ..Alert::new(node, "stalled") });
    }
    if let Some(hours) = quiet(r, &mut seen.quiet, now, unix) {
        out.push(Alert {
            blocks: r.info["blocks"].as_u64(),
            tip_time: r.tip_time,
            hours: Some(hours),
            peers: r.peers,
            ..Alert::new(node, "quiet")
        });
    }
    out
}

/// What each node was last seen as: [FreeBank's, eCash's].
#[derive(Default)]
pub struct Watch(Mutex<[Seen; 2]>);
pub type WatchState = Arc<Watch>;

/// The banner's alerts, for FreeBank's node (when the app talks to one) and the eCash node (when one answers).
#[tauri::command]
pub async fn node_alerts(
    client: State<'_, ClientState>,
    mgr: State<'_, Arc<NodeManager>>,
    watch: State<'_, WatchState>,
) -> Result<Vec<Alert>, String> {
    let fb = {
        let mut c = client.lock().await;
        if c.is_configured() {
            match c.call_fresh("getblockchaininfo", vec![]).await {
                Ok(info) => {
                    let best = info["bestblockhash"].as_str().map(|h| Value::String(h.to_string()));
                    let header = match best {
                        Some(h) => c.call_fresh("getblockheader", vec![h]).await.ok(),
                        None => None,
                    };
                    let peers = c.call_fresh("getpeerinfo", vec![]).await.ok();
                    Some(reading(info, header, peers))
                }
                Err(_) => None,
            }
        } else {
            None
        }
    };
    // Before the screens connect, the app's own node: Setup's "Catching up" is where a node left behind by a fork
    // stays, so the banner shows there too.
    let fb = match fb {
        Some(i) => Some(i),
        None if crate::network::current().freebank_on => {
            let s = mgr.settings.lock().await.clone();
            match s.installed_tag {
                Some(_) => {
                    let c = crate::node::detect::local_client(&mgr.http, &s);
                    match tokio::time::timeout(WAIT, c.call("getblockchaininfo", vec![])).await {
                        Ok(Ok(info)) => {
                            let best = info["bestblockhash"].as_str().map(|h| Value::String(h.to_string()));
                            let header = match best {
                                Some(h) => c.call("getblockheader", vec![h]).await.ok(),
                                None => None,
                            };
                            let peers = c.call("getpeerinfo", vec![]).await.ok();
                            Some(reading(info, header, peers))
                        }
                        _ => None,
                    }
                }
                None => None,
            }
        }
        None => None,
    };
    let ec = match tokio::time::timeout(WAIT, crate::ecash::commands::connect(&mgr)).await {
        Ok(Ok(conn)) => {
            let n = conn.node();
            match tokio::time::timeout(WAIT, n.call_typed("getblockchaininfo", vec![])).await {
                Ok(Ok(info)) => {
                    let header = match (info["time"].as_u64(), info["bestblockhash"].as_str()) {
                        (None, Some(h)) => n.call_typed("getblockheader", vec![Value::String(h.to_string())]).await.ok(),
                        _ => None,
                    };
                    let peers = n.call_typed("getpeerinfo", vec![]).await.ok();
                    Some(reading(info, header, peers))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let (now, unix) = (Instant::now(), unix_now());
    let mut seen = watch.0.lock().unwrap();
    let mut out = Vec::new();
    // A node that doesn't answer keeps no clock: its screens say it's down, and the count starts again when it's back.
    match fb {
        Some(r) => out.extend(of("freebank", &r, &mut seen[0], now, unix)),
        None => seen[0] = Seen::default(),
    }
    match ec {
        Some(r) => out.extend(of("ecash", &r, &mut seen[1], now, unix)),
        None => seen[1] = Seen::default(),
    }
    Ok(out)
}

fn reading(info: Value, header: Option<Value>, peers: Option<Value>) -> Reading {
    Reading {
        tip_time: tip_time_of(&info, header.as_ref()),
        peers: peers.as_ref().map(|p| handshaken(p).len() as u64),
        info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_pre_release_notice_and_empty_warnings_are_left_out() {
        let notice = "This is a pre-release build of experimental software: use only funds you can afford to lose. See NOTICE.md.";
        assert!(warnings_of(&json!({ "warnings": notice })).is_empty());
        assert!(warnings_of(&json!({ "warnings": "" })).is_empty());
        assert!(warnings_of(&json!({})).is_empty());
        assert!(warnings_of(&json!({ "warnings": ["This is a pre-release test build - use at your own risk"] })).is_empty());
        assert!(warnings_of(&json!({ "warnings": "testsafemode enabled" })).is_empty());
    }

    #[test]
    fn a_fork_warning_shows_as_the_node_gives_it_from_a_string_or_a_list() {
        let fork = "Warning: We do not appear to fully agree with our peers! You may need to upgrade, or other nodes may need to upgrade.";
        assert_eq!(warnings_of(&json!({ "warnings": fork })), vec![fork.to_string()]);
        let rules = "Unknown new rules activated (versionbit 3)";
        assert_eq!(
            warnings_of(&json!({ "warnings": [rules, " ", rules, "This is a pre-release test build"] })),
            vec![rules.to_string()]
        );
    }

    #[test]
    fn stalled_only_after_thirty_minutes_behind_out_of_initial_sync() {
        let t0 = Instant::now();
        let behind = json!({ "blocks": 100, "headers": 104, "initialblockdownload": false });
        let mut since = None;
        assert_eq!(stalled(&behind, &mut since, t0), None);
        assert_eq!(stalled(&behind, &mut since, t0 + STALL - Duration::from_secs(1)), None);
        assert_eq!(stalled(&behind, &mut since, t0 + STALL), Some((100, 104)));
        // Caught up: the clock starts again.
        let level = json!({ "blocks": 104, "headers": 104, "initialblockdownload": false });
        assert_eq!(stalled(&level, &mut since, t0 + STALL), None);
        assert_eq!(since, None);
        assert_eq!(stalled(&behind, &mut since, t0 + STALL * 2), None);
    }

    #[test]
    fn a_node_in_initial_sync_or_without_heights_is_never_stalled() {
        let t0 = Instant::now();
        let mut since = Some(t0);
        let syncing = json!({ "blocks": 10, "headers": 5000, "initialblockdownload": true });
        assert_eq!(stalled(&syncing, &mut since, t0 + STALL * 4), None);
        assert_eq!(since, None);
        assert_eq!(stalled(&json!({ "warnings": "" }), &mut since, t0), None);
    }

    #[test]
    fn of_tags_each_alert_with_its_node() {
        let t0 = Instant::now();
        let mut seen = Seen { behind: Some(t0), quiet: None };
        let info = json!({ "blocks": 1, "headers": 2, "initialblockdownload": false, "warnings": ["Disk space is low!"] });
        let r = Reading { info, tip_time: Some(1_000), peers: Some(1) };
        let a = of("ecash", &r, &mut seen, t0 + STALL, 1_000 + QUIET * 10);
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].node, a[0].kind, a[0].text.as_str()), ("ecash", "warning", "Disk space is low!"));
        assert_eq!((a[1].kind, a[1].blocks, a[1].headers), ("stalled", Some(1), Some(2)));
    }

    #[test]
    fn only_peers_that_finished_the_handshake_count() {
        // A Mac on 2026-11-11: a public relay took the connection with nothing behind it.
        let peers = json!([
            { "addr": "198.51.100.7:28455", "version": 0, "subver": "" },
            { "addr": "192.0.2.10:28455", "version": 70016, "subver": "/FreeBank:1.2.2/" },
        ]);
        assert_eq!(handshaken(&peers).len(), 1);
        assert_eq!(handshaken(&peers)[0]["subver"], "/FreeBank:1.2.2/");
        assert!(handshaken(&json!([])).is_empty());
        assert!(handshaken(&json!(null)).is_empty());
    }

    #[test]
    fn quiet_after_two_hours_with_nothing_left_to_download() {
        let t = 1_794_726_359;
        assert_eq!(quiet_hours(284, 284, Some(t), t + QUIET - 1), None);
        assert_eq!(quiet_hours(284, 284, Some(t), t + QUIET), Some(2));
        assert_eq!(quiet_hours(284, 284, Some(t), t + 5 * 3600 + 59 * 60), Some(5));
        // Downloading (or stalled: that alert says so), a new node at block 0, no tip time, a tip from the future.
        assert_eq!(quiet_hours(284, 290, Some(t), t + QUIET * 3), None);
        assert_eq!(quiet_hours(0, 0, Some(t), t + QUIET * 3), None);
        assert_eq!(quiet_hours(284, 284, None, t + QUIET * 3), None);
        assert_eq!(quiet_hours(284, 284, Some(t + 600), t), None);
    }

    #[test]
    fn the_tip_time_comes_from_the_chain_info_or_the_header() {
        assert_eq!(tip_time_of(&json!({ "time": 5 }), Some(&json!({ "time": 9 }))), Some(5));
        assert_eq!(tip_time_of(&json!({ "blocks": 1 }), Some(&json!({ "time": 9 }))), Some(9));
        assert_eq!(tip_time_of(&json!({ "blocks": 1 }), None), None);
    }

    #[test]
    fn the_banner_says_quiet_only_after_five_minutes_of_it() {
        let t0 = Instant::now();
        let tip = 1_794_726_359;
        let unix = tip + 3 * 3600;
        let r = Reading { info: json!({ "blocks": 284, "headers": 284 }), tip_time: Some(tip), peers: Some(0) };
        let mut seen = Seen::default();
        assert!(of("freebank", &r, &mut seen, t0, unix).is_empty());
        assert!(of("freebank", &r, &mut seen, t0 + QUIET_HOLD - Duration::from_secs(1), unix).is_empty());
        let a = of("freebank", &r, &mut seen, t0 + QUIET_HOLD, unix);
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].kind, a[0].blocks, a[0].hours, a[0].peers, a[0].tip_time), ("quiet", Some(284), Some(3), Some(0), Some(tip)));
        // A new block: the clock starts again.
        let fresh = Reading { info: json!({ "blocks": 285, "headers": 285 }), tip_time: Some(unix - 60), peers: Some(1) };
        assert!(of("freebank", &fresh, &mut seen, t0 + QUIET_HOLD * 2, unix).is_empty());
        assert_eq!(seen.quiet, None);
    }
}
