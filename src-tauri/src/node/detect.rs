//! Looking around: is there an eCash beta node + enforcer, is a FreeBank node already running,
//! and what is in the datadir.

use super::{Settings, DATADIR_MARK, PIN_HASH, PIN_HEIGHT};
use crate::rpc::{FreeBankClient, RpcError, RPC_IN_WARMUP};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// "http://host:port/" -> "host:port"
pub fn normalize_endpoint(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    s.trim_end_matches('/').to_string()
}

#[derive(Debug, Serialize)]
pub struct StackCheck {
    pub found: bool,
    pub rest_ok: bool,
    pub on_beta: bool,
    pub enforcer_ok: bool,
    pub l1_blocks: Option<u64>,
    /// What is missing, in plain words (empty when found).
    pub detail: String,
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<serde_json::Value> {
    http.get(url)
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

pub async fn l1_blocks(http: &reqwest::Client, rest: &str) -> Option<u64> {
    get_json(http, &format!("http://{}/rest/chaininfo.json", rest))
        .await?
        .get("blocks")?
        .as_u64()
}

pub async fn check_stack(http: &reqwest::Client, rest: &str, enforcer: &str) -> StackCheck {
    let rest_ok_blocks = l1_blocks(http, rest).await;
    let rest_ok = rest_ok_blocks.is_some();
    let on_beta = rest_ok
        && get_json(http, &format!("http://{}/rest/blockhashbyheight/{}.json", rest, PIN_HEIGHT))
            .await
            .and_then(|v| v.get("blockhash")?.as_str().map(|s| s == PIN_HASH))
            .unwrap_or(false);
    // v0: a TCP connect is enough to say the enforcer is there.
    let enforcer_ok = matches!(
        tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(enforcer)).await,
        Ok(Ok(_))
    );
    let detail = if !rest_ok {
        format!("No eCash node answered at {}.", rest)
    } else if !on_beta {
        format!(
            "The eCash node at {} is not on eCash beta, or has not synced past block {} yet.",
            rest, PIN_HEIGHT
        )
    } else if !enforcer_ok {
        format!("No enforcer answered at {}.", enforcer)
    } else {
        String::new()
    };
    StackCheck {
        found: rest_ok && on_beta && enforcer_ok,
        rest_ok,
        on_beta,
        enforcer_ok,
        l1_blocks: rest_ok_blocks,
        detail,
    }
}

/// RPC credentials for the node in `datadir`: the cookie, else rpcuser/rpcpassword in freebank.conf.
pub fn rpc_auth(datadir: &Path) -> Option<(String, String)> {
    if let Ok(c) = std::fs::read_to_string(datadir.join(".cookie")) {
        if let Some((u, p)) = c.trim().split_once(':') {
            return Some((u.to_string(), p.to_string()));
        }
    }
    let conf = std::fs::read_to_string(datadir.join("freebank.conf")).ok()?;
    let get = |k: &str| {
        conf.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .map(|v| v.trim().to_string())
    };
    Some((get("rpcuser=")?, get("rpcpassword=")?))
}

/// An RPC client for the local node, with whatever credentials its datadir holds right now
/// (the cookie changes on every start).
pub fn local_client(http: &reqwest::Client, s: &Settings) -> FreeBankClient {
    let mut c = FreeBankClient::with_http(http.clone()).with_timeout(Duration::from_secs(20));
    let (u, p) = rpc_auth(Path::new(&s.datadir)).unwrap_or_default();
    c.configure(&format!("http://127.0.0.1:{}", s.rpc_port), &u, &p);
    c
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RpcState {
    /// Nothing answers on the RPC port.
    Down,
    /// freebankd answers but is still loading (block index, eCash chain checks).
    Warming,
    Up,
    /// Something answers but our credentials don't work (another user's node, or another datadir).
    Locked,
    /// The app is stopping, restarting or updating the node (Node tab only).
    Busy,
}

#[derive(Debug, Serialize)]
pub struct Probe {
    pub state: RpcState,
    pub message: String,
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
}

pub async fn probe(http: &reqwest::Client, s: &Settings) -> Probe {
    let c = local_client(http, s);
    let (state, message, info) = match c.call_typed("getblockchaininfo", vec![]).await {
        Ok(v) => (RpcState::Up, String::new(), Some(v)),
        Err(RpcError::Rpc { code, message }) if code == RPC_IN_WARMUP => {
            (RpcState::Warming, message, None)
        }
        Err(RpcError::Busy) => (RpcState::Warming, "Checking blocks…".to_string(), None),
        Err(RpcError::Http(401)) | Err(RpcError::Http(403)) => (
            RpcState::Locked,
            format!(
                "A node answers on port {}, but not with the password in {}.",
                s.rpc_port, s.datadir
            ),
            None,
        ),
        Err(RpcError::Unreachable(_)) | Err(RpcError::NotConfigured) => {
            (RpcState::Down, String::new(), None)
        }
        Err(e) => (RpcState::Locked, e.to_string(), None),
    };
    Probe {
        state,
        message,
        blocks: info.as_ref().and_then(|v| v["blocks"].as_u64()),
        headers: info.as_ref().and_then(|v| v["headers"].as_u64()),
    }
}

/// The eCash block-hash cache entry at the pin height, in display order.
fn pin_entry(mainblockhash: &Path) -> Option<String> {
    let mut f = std::fs::File::open(mainblockhash).ok()?;
    f.seek(SeekFrom::Start(12 + PIN_HEIGHT * 32)).ok()?;
    let mut b = [0u8; 32];
    f.read_exact(&mut b).ok()?;
    b.reverse();
    Some(hex::encode(b))
}

#[derive(Debug, Serialize)]
pub struct DatadirCheck {
    /// "new": empty or missing; "ours": a FreeBank beta datadir, used as is;
    /// "other": data this node can't use, which must be moved aside first.
    pub kind: &'static str,
    pub message: String,
    /// Where "other" data would be moved.
    pub away: Option<String>,
    pub has_wallet: bool,
}

pub fn check_datadir(datadir: &Path) -> DatadirCheck {
    let entries: Vec<String> = std::fs::read_dir(datadir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let has_wallet = !super::wallet_files(datadir).is_empty();
    let mk = |kind, message: String, away| DatadirCheck {
        kind,
        message,
        away,
        has_wallet,
    };
    if entries.iter().all(|e| e == "freebank.conf") {
        return mk("new", String::new(), None);
    }
    if !datadir.join(DATADIR_MARK).exists() && nearly_empty(datadir) {
        return mk(
            "new",
            "This folder holds only leftovers from an earlier start (no blocks, no wallet). \
             FreeBank will use it as it is."
                .into(),
            None,
        );
    }
    if datadir.join(DATADIR_MARK).exists() {
        return mk("ours", "FreeBank will keep using the data already in this folder.".into(), None);
    }
    if datadir.join("blocks").is_dir()
        && pin_entry(&datadir.join("mainblockhash.dat")).as_deref() == Some(PIN_HASH)
    {
        return mk(
            "ours",
            "This folder already holds a FreeBank beta node's data; FreeBank will use it.".into(),
            None,
        );
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let away = format!("{}.old-{}", datadir.to_string_lossy(), stamp);
    mk(
        "other",
        "This folder holds data from an older FreeBank node that this version can't use. \
         It will be moved aside, and nothing is deleted."
            .into(),
        Some(away),
    )
}

/// Files a node leaves behind even when it never got going. Nothing here is worth keeping.
const LEFTOVERS: &[&str] = &[
    "freebank.conf", "debug.log", "db.log", ".lock", ".cookie", "peers.dat", "banlist.dat",
    "banlist.json", "anchors.dat", "fee_estimates.dat", "mempool.dat", "settings.json", ".walletlock",
    "onion_private_key", "onion_v3_private_key",
];

/// A folder with no wallet and no chain: only LEFTOVERS files, and subfolders with no files in them.
fn nearly_empty(datadir: &Path) -> bool {
    fn no_files(dir: &Path) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        rd.filter_map(|e| e.ok()).all(|e| match e.file_type() {
            Ok(t) if t.is_dir() => no_files(&e.path()),
            _ => false,
        })
    }
    let Ok(rd) = std::fs::read_dir(datadir) else {
        return false;
    };
    rd.filter_map(|e| e.ok()).all(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        match e.file_type() {
            Ok(t) if t.is_dir() => no_files(&e.path()),
            Ok(t) if t.is_file() => LEFTOVERS.contains(&name.as_str()),
            _ => false,
        }
    })
}

/// 970387 -> "970,387"
fn grouped(n: u64) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One line of "Test connection".
#[derive(Debug, Serialize)]
pub struct ConnCheck {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

/// The enforcer's tip through grpcurl, read-only (ValidatorService/GetChainTip): (height, hash).
async fn enforcer_tip(grpcurl: &Path, enforcer: &str) -> Result<(u64, String), String> {
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(grpcurl)
            .args(["-plaintext", "-max-time", "10", "-d", "{}", enforcer])
            .arg("cusf.mainchain.v1.ValidatorService/GetChainTip")
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "no answer within 20 seconds".to_string())?
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().next().unwrap_or("no answer").trim();
        return Err(line.chars().take(160).collect());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "an answer FreeBank couldn't read".to_string())?;
    let info = &v["blockHeaderInfo"];
    let height = info["height"]
        .as_u64()
        .or_else(|| info["height"].as_str()?.parse().ok())
        .ok_or("an answer without a height")?;
    let hash = info["blockHash"]["hex"]
        .as_str()
        .ok_or("an answer without a block hash")?
        .to_string();
    Ok((height, hash))
}

/// Is `hash` on the REST node's active chain? freebankd asks the same way at startup:
/// /rest/headers/1/<hash> returns that header only when it is on the active chain.
async fn on_rest_chain(http: &reqwest::Client, rest: &str, hash: &str) -> Option<bool> {
    let body = http
        .get(format!("http://{}/rest/headers/1/{}.hex", rest, hash))
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    let body = body.trim();
    if body.is_empty() {
        return Some(false);
    }
    let raw = hex::decode(body).ok().filter(|r| r.len() == 80)?;
    let mut h: [u8; 32] = Sha256::digest(Sha256::digest(&raw)).into();
    h.reverse();
    Some(hex::encode(h) == hash)
}

/// "Test connection" under Advanced: the checks freebankd makes at startup, in plain words.
pub async fn test_connection(
    http: &reqwest::Client,
    grpcurl: Option<&Path>,
    rest: &str,
    enforcer: &str,
) -> Vec<ConnCheck> {
    let mut checks = Vec::new();
    let mut add = |label: &str, ok: bool, detail: String| {
        checks.push(ConnCheck {
            label: label.to_string(),
            ok,
            detail,
        })
    };

    let info = get_json(http, &format!("http://{}/rest/chaininfo.json", rest)).await;
    let blocks = info.as_ref().and_then(|v| v["blocks"].as_u64());
    match blocks {
        Some(b) => add("eCash node answers", true, format!("block {}", grouped(b))),
        None => add("eCash node answers", false, format!("nothing at {}", rest)),
    }
    if blocks.is_some() {
        let pin = get_json(http, &format!("http://{}/rest/blockhashbyheight/{}.json", rest, PIN_HEIGHT))
            .await
            .and_then(|v| v.get("blockhash")?.as_str().map(String::from));
        match pin {
            Some(h) if h == PIN_HASH => add("On eCash beta", true, format!("block {} matches", grouped(PIN_HEIGHT))),
            Some(_) => add("On eCash beta", false, format!("block {} is a different block", grouped(PIN_HEIGHT))),
            None => add("On eCash beta", false, format!("not synced to block {} yet", grouped(PIN_HEIGHT))),
        }
    }

    let tcp = matches!(
        tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(enforcer)).await,
        Ok(Ok(_))
    );
    if !tcp {
        add("Enforcer answers", false, format!("nothing at {}", enforcer));
        return checks;
    }
    let Some(grpcurl) = grpcurl else {
        add("Enforcer answers", true, "port open; the full check needs grpcurl, which comes with the install".into());
        return checks;
    };
    match enforcer_tip(grpcurl, enforcer).await {
        Err(e) => add("Enforcer answers", false, e),
        Ok((height, hash)) => {
            add("Enforcer answers", true, format!("block {}", grouped(height)));
            if blocks.is_some() {
                match on_rest_chain(http, rest, &hash).await {
                    Some(true) => add("Enforcer and eCash node agree", true, "same chain".into()),
                    Some(false) => add(
                        "Enforcer and eCash node agree",
                        false,
                        "the enforcer's tip isn't on this eCash node's chain (one may still be catching up)".into(),
                    ),
                    None => add("Enforcer and eCash node agree", false, "couldn't check".into()),
                }
            }
        }
    }
    checks
}

/// Is something listening on this local port?
pub fn port_busy(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(400),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fbdd-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn near_empty_is_reused() {
        let d = dir("near");
        std::fs::write(d.join("debug.log"), b"old log").unwrap();
        std::fs::write(d.join("peers.dat"), b"x").unwrap();
        std::fs::create_dir_all(d.join("blocks/index")).unwrap();
        std::fs::create_dir_all(d.join("wallets")).unwrap();
        let c = check_datadir(&d);
        assert_eq!(c.kind, "new");
        assert!(!c.has_wallet);

        // A wallet anywhere, a block file, or an unknown file makes it real data.
        std::fs::write(d.join("wallets/wallet.dat"), b"w").unwrap();
        let c = check_datadir(&d);
        assert_eq!(c.kind, "other");
        assert!(c.has_wallet);
        std::fs::remove_file(d.join("wallets/wallet.dat")).unwrap();
        std::fs::write(d.join("blocks/blk00000.dat"), b"b").unwrap();
        assert_eq!(check_datadir(&d).kind, "other");
        std::fs::remove_file(d.join("blocks/blk00000.dat")).unwrap();
        std::fs::write(d.join("wallet.dat"), b"w").unwrap();
        let c = check_datadir(&d);
        assert_eq!(c.kind, "other");
        assert!(c.has_wallet);
        std::fs::remove_file(d.join("wallet.dat")).unwrap();
        std::fs::write(d.join("mainblockhash.dat"), b"m").unwrap();
        assert_eq!(check_datadir(&d).kind, "other");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(967680), "967,680");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    #[test]
    fn missing_or_conf_only_is_new() {
        let d = dir("conf");
        assert_eq!(check_datadir(&d.join("nope")).kind, "new");
        std::fs::write(d.join("freebank.conf"), b"coinbasetag=x\n").unwrap();
        let c = check_datadir(&d);
        assert_eq!(c.kind, "new");
        assert!(c.message.is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
