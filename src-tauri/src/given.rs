//! Addresses given out (v0.6.8): every address Receive hands out, FreeBank's or eCash's, on this screen or to a paired
//! phone, with when, and a note of what it is for if the owner adds one. They are kept in `<app data>/wallet/addresses.json`
//! (this network's, readable by this user only). Settings › Wallet lists them, with the received amounts and the
//! addresses the wallets had given out before this list was kept (no date then), so a given-out address is never a
//! mystery.

use crate::commands::ClientState;
use crate::node::NodeManager;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::State;

/// The list stops growing here: the oldest without a note go first.
const MAX: usize = 5_000;
/// A note's length, in characters.
pub const NOTE_MAX: usize = 120;

static FILE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Given {
    pub address: String,
    /// "freebank" or "ecash".
    pub kind: String,
    /// The FreeBank wallet it came from, when not the main one.
    #[serde(default)]
    pub wallet: Option<String>,
    /// Unix seconds; 0 for one the wallet had given out before this list was kept.
    pub at: u64,
    /// "app", or the paired phone that asked for it ("phone: <its name>").
    #[serde(default)]
    pub via: String,
    #[serde(default)]
    pub note: String,
}

fn path(app_dir: &Path) -> PathBuf {
    crate::seed::seed_path(app_dir).with_file_name("addresses.json")
}

/// The list; empty when there is none yet. A file that can't be read or isn't the list is an error, so nothing writes
/// over it.
fn read(app_dir: &Path) -> Result<Vec<Given>, String> {
    match std::fs::read(path(app_dir)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{} isn't a list of addresses: {e}", path(app_dir).display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path(app_dir).display())),
    }
}

pub fn load(app_dir: &Path) -> Vec<Given> {
    read(app_dir).unwrap_or_default()
}

fn save(app_dir: &Path, list: &[Given]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    crate::seed::write_private(&path(app_dir), &bytes)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Note an address just handed out. Never fails the hand-out: a list that can't be written is only logged.
pub fn record(app_dir: &Path, kind: &str, address: &str, wallet: Option<&str>, via: &str) {
    let _one = FILE_LOCK.lock().unwrap();
    let mut list = match read(app_dir) {
        Ok(l) => l,
        Err(e) => {
            crate::activity::note(&format!("addresses: not kept, the list can't be read: {e}"));
            return;
        }
    };
    if list.iter().any(|g| g.address == address) {
        return;
    }
    list.push(Given {
        address: address.to_string(),
        kind: kind.to_string(),
        wallet: wallet.filter(|w| !w.is_empty()).map(String::from),
        at: unix_now(),
        via: via.to_string(),
        note: String::new(),
    });
    trim(&mut list, MAX);
    if let Err(e) = save(app_dir, &list) {
        crate::activity::note(&format!("addresses: couldn't keep the list: {e}"));
    }
}

/// The notes kept, by address (empty ones left out): the payment exports' note column.
pub fn notes(app_dir: &Path) -> std::collections::HashMap<String, String> {
    load(app_dir).into_iter().filter(|g| !g.note.is_empty()).map(|g| (g.address, g.note)).collect()
}

/// Past `max`, the oldest without a note go; noted ones stay.
fn trim(list: &mut Vec<Given>, max: usize) {
    let mut extra = list.len().saturating_sub(max);
    list.retain(|g| {
        let go = extra > 0 && g.note.is_empty();
        if go {
            extra -= 1;
        }
        !go
    });
}

/// A note, one line, without control characters.
fn clean_note(note: &str) -> String {
    note.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NOTE_MAX).collect()
}

/// Set (or, empty, clear) an address's note. One the list doesn't have yet (given out before it was kept) is added.
pub fn set_note(app_dir: &Path, address: &str, kind: &str, note: &str) -> Result<(), String> {
    let _one = FILE_LOCK.lock().unwrap();
    let mut list = read(app_dir)?;
    let note = clean_note(note);
    match list.iter_mut().find(|g| g.address == address) {
        Some(g) => g.note = note,
        None => list.push(Given { address: address.into(), kind: kind.into(), note, ..Default::default() }),
    }
    save(app_dir, &list)
}

/// One row of Settings › Wallet's list.
#[derive(Debug, Serialize, PartialEq)]
pub struct Row {
    #[serde(flatten)]
    pub given: Given,
    /// What it has received, in coins; None when its wallet wasn't asked (another FreeBank wallet, eCash not set up).
    pub received: Option<f64>,
}

/// The list, newest first: the kept rows, then those the wallets' address books add (`node`: address, kind, received,
/// label; a FreeBank one is the screens' wallet's, `fb_wallet`).
fn merge(kept: Vec<Given>, node: &[(String, &str, f64, String)], fb_wallet: Option<&str>) -> Vec<Row> {
    let mut rows: Vec<Row> = kept
        .into_iter()
        .map(|g| {
            let received = node.iter().find(|(a, k, ..)| *a == g.address && *k == g.kind).map(|(_, _, amount, _)| *amount);
            Row { given: g, received }
        })
        .collect();
    for (address, kind, amount, label) in node {
        if !rows.iter().any(|r| r.given.address == *address) {
            let wallet = if *kind == "freebank" { fb_wallet.map(String::from) } else { None };
            let given = Given { address: address.clone(), kind: kind.to_string(), wallet, note: clean_note(label), ..Default::default() };
            rows.push(Row { given, received: Some(*amount) });
        }
    }
    rows.sort_by(|a, b| b.given.at.cmp(&a.given.at));
    rows
}

fn received_list(v: &Value) -> Vec<(String, f64, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some((e["address"].as_str()?.to_string(), e["amount"].as_f64().unwrap_or(0.0), e["label"].as_str().unwrap_or("").to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Settings › Wallet's list: the screens' FreeBank wallet's addresses and the eCash wallet's, with what each received.
#[tauri::command]
pub async fn addresses_given(client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>) -> Result<Vec<Row>, String> {
    let mut node: Vec<(String, &str, f64, String)> = Vec::new();
    let wallet = {
        let mut c = client.lock().await;
        // Every address of its address book, paid or not (getnewaddress puts each there).
        if let Ok(v) = c.call_ui("listreceivedbyaddress", vec![json!(0), json!(true)]).await {
            node.extend(received_list(&v).into_iter().map(|(a, amt, l)| (a, "freebank", amt, l)));
        }
        c.wallet().map(String::from)
    };
    if let Ok(v) = crate::ecash::commands::received_by_address(&mgr).await {
        node.extend(received_list(&v).into_iter().map(|(a, amt, l)| (a, "ecash", amt, l)));
    }
    let kept = read(&mgr.app_dir)?;
    let mut rows = merge(kept, &node, wallet.as_deref());
    // Another FreeBank wallet's rows weren't asked about: no amount for them.
    for r in rows.iter_mut().filter(|r| r.given.kind == "freebank" && r.given.wallet != wallet) {
        r.received = None;
    }
    Ok(rows)
}

#[tauri::command]
pub fn addresses_note(mgr: State<'_, Arc<NodeManager>>, address: String, kind: String, note: String) -> Result<(), String> {
    if kind != "freebank" && kind != "ecash" {
        return Err("Which kind of address?".into());
    }
    set_note(&mgr.app_dir, address.trim(), &kind, &note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(what: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fb-given-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn kept_once_with_notes_and_private() {
        let d = dir("kept");
        record(&d, "freebank", "Xabc", None, "app");
        record(&d, "freebank", "Xabc", None, "app");
        record(&d, "ecash", "bc1qxyz", None, "phone: Ann's phone");
        let l = load(&d);
        assert_eq!(l.len(), 2, "once each");
        assert_eq!((l[1].kind.as_str(), l[1].via.as_str()), ("ecash", "phone: Ann's phone"));
        assert!(l[0].at > 0);
        set_note(&d, "bc1qxyz", "ecash", "  beta\nburn\tcoins  ").unwrap();
        assert_eq!(load(&d)[1].note, "beta burn coins", "one line");
        set_note(&d, "Xold", "freebank", &"x".repeat(500)).unwrap();
        assert_eq!(load(&d)[2].note.len(), NOTE_MAX, "an earlier one is added, its note cut");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path(&d)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_list_that_cant_be_read_is_never_written_over() {
        let d = dir("bad");
        crate::seed::write_private(&path(&d), b"not json").unwrap();
        record(&d, "freebank", "Xnew", None, "app");
        assert_eq!(std::fs::read(path(&d)).unwrap(), b"not json", "left as it was");
        assert!(set_note(&d, "Xnew", "freebank", "x").is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn trimming_keeps_the_noted_ones() {
        let g = |a: &str, note: &str| Given { address: a.into(), note: note.into(), ..Default::default() };
        let mut l = vec![g("a", "rent"), g("b", ""), g("c", ""), g("d", "wages"), g("e", "")];
        trim(&mut l, 3);
        assert_eq!(l.iter().map(|x| x.address.as_str()).collect::<Vec<_>>(), vec!["a", "d", "e"]);
        trim(&mut l, 1);
        assert_eq!(l.iter().map(|x| x.address.as_str()).collect::<Vec<_>>(), vec!["a", "d"], "notes stay even past the cap");
    }

    #[test]
    fn merged_with_the_address_books() {
        let kept = vec![
            Given { address: "Xa".into(), kind: "freebank".into(), at: 10, via: "app".into(), ..Default::default() },
            Given { address: "tb1b".into(), kind: "ecash".into(), at: 20, via: "app".into(), note: "faucet".into(), ..Default::default() },
        ];
        let node = vec![
            ("Xa".to_string(), "freebank", 1.5, String::new()),
            ("Xearlier".to_string(), "freebank", 0.0, "rent".to_string()),
            ("tb1b".to_string(), "ecash", 2.0, String::new()),
        ];
        let rows = merge(kept, &node, None);
        assert_eq!(rows.iter().map(|r| r.given.address.as_str()).collect::<Vec<_>>(), vec!["tb1b", "Xa", "Xearlier"], "newest first");
        assert_eq!(rows[0].received, Some(2.0));
        assert_eq!(rows[0].given.note, "faucet");
        assert_eq!(rows[1].received, Some(1.5));
        assert_eq!((rows[2].given.at, rows[2].given.note.as_str()), (0, "rent"), "given out before the list: the node's label");
    }
}
