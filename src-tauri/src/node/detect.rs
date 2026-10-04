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
    /// Warming only because the node answered too late (busy connecting blocks), not because it is starting.
    pub busy: bool,
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
}

pub async fn probe(http: &reqwest::Client, s: &Settings) -> Probe {
    let c = local_client(http, s);
    let r = c.call_typed("getblockchaininfo", vec![]).await;
    let busy = matches!(r, Err(RpcError::Busy));
    let (state, message, info) = match r {
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
        Err(RpcError::Refused(_)) | Err(RpcError::Unreachable(_)) | Err(RpcError::NotConfigured) => {
            (RpcState::Down, String::new(), None)
        }
        Err(e) => (RpcState::Locked, e.to_string(), None),
    };
    Probe {
        busy,
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
    /// "other": data this node can't use, which must be moved aside first;
    /// "earlier" (Setup only, `setup_check`): a folder FreeBank set up for an earlier install, which
    /// Setup offers to use or to move aside ("Start fresh").
    pub kind: &'static str,
    pub message: String,
    /// Where "other" or "earlier" data would be moved.
    pub away: Option<String>,
    pub has_wallet: bool,
    /// For "earlier": the name on its blocks (freebank.conf) and the last block its log shows.
    pub tag: Option<String>,
    pub height: Option<u64>,
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
        tag: None,
        height: None,
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
    mk(
        "other",
        "This folder holds data from an older FreeBank node that this version can't use. \
         It will be moved aside, and nothing is deleted."
            .into(),
        Some(away_path(datadir)),
    )
}

/// Where a folder is moved aside to: <folder>.old-<unix time>, or with -2, -3… if that is taken
/// (two moves in one second).
fn away_path(datadir: &Path) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = format!("{}.old-{}", datadir.to_string_lossy(), stamp);
    (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{}-{}", base, n) })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap_or(base)
}

/// Setup's look at the data folder. A folder with FreeBank's mark that this install didn't record
/// (`created`) was set up for an earlier one: v0.1.1's Obliterate kept it, or the app's own folder
/// went. Setup says so and asks: use it (its blocks, wallet and name), or start fresh (moved aside).
pub fn setup_check(datadir: &Path, created: &[String]) -> DatadirCheck {
    let mut c = check_datadir(datadir);
    let recorded = created.iter().any(|d| Path::new(d) == datadir);
    if c.kind == "ours" && datadir.join(DATADIR_MARK).exists() && !recorded {
        c.kind = "earlier";
        c.message = "FreeBank set up this folder for an earlier install.".into();
        c.away = Some(away_path(datadir));
        c.tag = super::conf_tag(datadir);
        c.height = last_height(datadir);
    }
    c
}

/// The height of the last "UpdateTip: new best=… height=N" line in the end of debug.log.
fn last_height(datadir: &Path) -> Option<u64> {
    let mut f = std::fs::File::open(datadir.join("debug.log")).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(256 * 1024))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("UpdateTip: new best=")).find_map(|l| {
        let rest = &l[l.find(" height=")? + 8..];
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    })
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
pub(crate) fn grouped(n: u64) -> String {
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

/// The enforcer's tip, read-only (ValidatorService/GetChainTip): (height, hash). Over the Connect protocol, as
/// freebankd (v0.2.17 on) asks it by default: a plain HTTP/1.1 POST of proto3 JSON to the enforcer's own port, so no
/// grpcurl is needed (v0.2.5). Its own client: no redirects (a 307 would send the POST elsewhere), and at most 64 KiB
/// read (security review L7).
async fn enforcer_tip(enforcer: &str) -> Result<(u64, String), String> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = http
        .post(format!("http://{}/cusf.mainchain.v1.ValidatorService/GetChainTip", enforcer))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .body("{}")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("no answer ({})", e.without_url()))?;
    let status = resp.status();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("no full answer ({})", e.without_url()))? {
        if body.len() + chunk.len() > 64 * 1024 {
            return Err("an answer too long to be the enforcer's".into());
        }
        body.extend_from_slice(&chunk);
    }
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| format!("an answer FreeBank couldn't read (HTTP {})", status.as_u16()))?;
    if !status.is_success() {
        // Connect's error body: {"code": "...", "message": "..."}.
        let msg = v["message"].as_str().or(v["code"].as_str()).unwrap_or("an error");
        return Err(msg.chars().take(160).collect());
    }
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
pub async fn test_connection(http: &reqwest::Client, rest: &str, enforcer: &str) -> Vec<ConnCheck> {
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
    match enforcer_tip(enforcer).await {
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

    /// Operator, 2026-09-29: Setup took back an earlier install's folder, name and all, without asking.
    #[test]
    fn an_earlier_installs_folder_is_asked_about() {
        let d = dir("earlier");
        std::fs::write(d.join(DATADIR_MARK), b"").unwrap();
        std::fs::write(d.join("wallet.dat"), b"w").unwrap();
        std::fs::write(d.join("freebank.conf"), b"coinbasetag=Old Name\n").unwrap();
        std::fs::write(
            d.join("debug.log"),
            "2026-09-29 10:48:47 UpdateTip: new best=19f4 height=402 version=0x20000000 tx=676549\n\
             2026-09-29 10:51:08 UpdateTip: new best=ec8e height=403 version=0x20000000 tx=681276\n\
             2026-09-29 10:53:38 Shutdown: done\n",
        )
        .unwrap();
        let c = setup_check(&d, &[]);
        assert_eq!((c.kind, c.tag.as_deref(), c.height, c.has_wallet), ("earlier", Some("Old Name"), Some(403), true));
        assert!(c.away.unwrap().starts_with(&format!("{}.old-", d.display())));
        // This install's own folder: used as it is, no question.
        let mine = [d.to_string_lossy().into_owned()];
        assert_eq!(setup_check(&d, &mine).kind, "ours");
        // Without the mark it is someone else's beta node (or data this node can't use): as before.
        std::fs::remove_file(d.join(DATADIR_MARK)).unwrap();
        assert_eq!(setup_check(&d, &[]).kind, check_datadir(&d).kind);
        std::fs::remove_dir_all(&d).unwrap();
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

    /// A one-request HTTP server: the request line and headers it got, and its answer.
    fn answer_once(status: &str, body: &str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let (status, body) = (status.to_string(), body.to_string());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            tx.send(String::from_utf8_lossy(&buf[..n]).into_owned()).unwrap();
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        (addr, rx)
    }

    /// v0.2.5: the enforcer's tip over the Connect protocol, as freebankd asks it, with no grpcurl.
    #[tokio::test]
    async fn the_enforcer_is_asked_over_connect() {
        let hash = "00".repeat(32);
        let (addr, got) = answer_once("200 OK", &format!(r#"{{"blockHeaderInfo":{{"blockHash":{{"hex":"{hash}"}},"height":"967812"}}}}"#));
        let tip = enforcer_tip(&addr).await;
        assert_eq!(tip, Ok((967_812, hash)));
        let req = got.recv().unwrap().to_ascii_lowercase();
        assert!(req.starts_with("post /cusf.mainchain.v1.validatorservice/getchaintip http/1.1"), "{req}");
        assert!(req.contains("connect-protocol-version: 1") && req.contains("content-type: application/json"));
        // An error comes back as Connect's {"code", "message"}.
        let (addr, _kept) = answer_once("503 Service Unavailable", r#"{"code":"unavailable","message":"not synced"}"#);
        assert_eq!(enforcer_tip(&addr).await, Err("not synced".to_string()));
        // A redirect isn't followed (security review L7), and an answer that isn't JSON says its HTTP status.
        let (addr, _kept) = answer_once("307 Temporary Redirect\r\nLocation: http://192.0.2.1/x", "");
        assert_eq!(enforcer_tip(&addr).await, Err("an answer FreeBank couldn't read (HTTP 307)".to_string()));
        let (addr, _kept) = answer_once("404 Not Found", "nothing here");
        assert_eq!(enforcer_tip(&addr).await, Err("an answer FreeBank couldn't read (HTTP 404)".to_string()));
        // Nor is an endless answer read.
        let (addr, _kept) = answer_once("200 OK", &" ".repeat(70 * 1024));
        assert_eq!(enforcer_tip(&addr).await, Err("an answer too long to be the enforcer's".to_string()));
    }
}
