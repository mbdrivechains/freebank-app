//! The eCash node's JSON-RPC: where it answers and with which login.
//!
//! The node answers RPC on the port Setup already knows for its REST (BitWindow's eCash node serves both on 18302),
//! unless Settings gives another (`l1_rpc`). A login typed in Settings › Node & connection (a node on another computer)
//! is tried first: its user in settings.json, its password in `<app data>/wallet/ecash-login`, readable by this user
//! only. Then the login is looked for the way BitWindow's own `readMainchainConf` does, first match the node accepts:
//! 1. the node's cookie, then `rpcuser`/`rpcpassword` in its `bitcoin.conf`, in the data folder Settings names
//!    (`l1_datadir`), else the eCash default (`~/.ecash`, or `~/Library/Application Support/Ecash` on a Mac);
//! 2. `rpcuser`/`rpcpassword` in BitWindow's `bitwindow-bitcoin.conf`;
//! 3. BitWindow's default login, `user` / `password`. A node that takes it takes it from any program on this computer,
//!    so the screens say so.

use crate::node::Settings;
use crate::rpc::{FreeBankClient, RpcError};
use crate::seed::Chain;
use std::path::{Path, PathBuf};
use serde_json::json;
use std::time::Duration;

#[derive(Clone)]
pub struct Conn {
    /// "http://host:port"
    pub base: String,
    user: String,
    pass: String,
    http: reqwest::Client,
    pub chain: Chain,
    /// The node took BitWindow's default login.
    pub default_login: bool,
}

impl Conn {
    #[cfg(test)]
    pub fn for_test(base: &str, chain: Chain) -> Conn {
        Conn {
            base: base.trim_end_matches('/').to_string(),
            user: "user".into(),
            pass: "pass".into(),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            chain,
            default_login: false,
        }
    }

    /// The output script of an address on this node's network, worked out here.
    pub fn script_of(&self, address: &str) -> Result<bitcoin::ScriptBuf, String> {
        use std::str::FromStr;
        let net = match self.chain {
            Chain::Main => bitcoin::Network::Bitcoin,
            Chain::Regtest => bitcoin::Network::Regtest,
        };
        let a = bitcoin::Address::from_str(address.trim()).map_err(|_| "That isn't an eCash address.")?;
        Ok(a.require_network(net).map_err(|_| "That address is for another network.")?.script_pubkey())
    }

    /// The node itself (chain calls).
    pub fn node(&self) -> FreeBankClient {
        self.client(&self.base.clone())
    }

    /// One of its wallets, by name.
    pub fn wallet(&self, name: &str) -> FreeBankClient {
        self.client(&format!("{}/wallet/{}", self.base, name))
    }

    fn client(&self, url: &str) -> FreeBankClient {
        let mut c = FreeBankClient::with_http(self.http.clone()).with_timeout(Duration::from_secs(60));
        c.configure(url, &self.user, &self.pass);
        c
    }
}

/// The eCash node's own data folder: Settings', else the eCash default.
fn node_dir(s: &Settings) -> PathBuf {
    match &s.l1_datadir {
        Some(d) if !d.trim().is_empty() => PathBuf::from(d.trim()),
        _ if cfg!(target_os = "macos") => crate::node::home().join("Library/Application Support/Ecash"),
        _ => crate::node::home().join(".ecash"),
    }
}

fn bitwindow_conf() -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        crate::node::home().join("Library/Application Support/bitwindow")
    } else {
        crate::node::home().join(".local/share/bitwindow")
    };
    dir.join("bitwindow-bitcoin.conf")
}

/// `rpcuser` and `rpcpassword` from a Core config, in any section (the first of each).
pub fn conf_login(text: &str) -> Option<(String, String)> {
    let get = |k: &str| {
        text.lines().find_map(|l| {
            let l = l.trim();
            let (key, v) = l.split_once('=')?;
            // "rpcuser=…", or a section-prefixed "main.rpcuser=…".
            (key.trim() == k || key.trim().ends_with(&format!(".{}", k))).then(|| v.trim().to_string())
        })
    };
    let (u, p) = (get("rpcuser")?, get("rpcpassword")?);
    (!u.is_empty() && !p.is_empty()).then_some((u, p))
}

fn cookie(dir: &Path) -> Option<(String, String)> {
    let c = std::fs::read_to_string(dir.join(".cookie")).ok()?;
    let (u, p) = c.trim().split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

/// Where a typed eCash login's password is kept (owner-only).
pub fn password_path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("ecash-login")
}

/// Keep (or, with None, forget) the typed login's password, readable by this user only.
pub fn save_password(app_dir: &Path, password: Option<&str>) -> Result<(), String> {
    let p = password_path(app_dir);
    let _ = std::fs::remove_file(&p);
    let Some(pw) = password.filter(|x| !x.is_empty()) else { return Ok(()) };
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    use std::io::Write;
    let mut f = crate::node::install::private_file(&p).map_err(|e| e.to_string())?;
    f.write_all(pw.as_bytes()).map_err(|e| e.to_string())
}

fn typed_login(s: &Settings, app_dir: Option<&Path>) -> Option<(String, String)> {
    let user = s.l1_user.as_deref().map(str::trim).filter(|u| !u.is_empty())?;
    let pw = std::fs::read_to_string(password_path(app_dir?)).ok()?;
    Some((user.to_string(), pw))
}

/// The logins to try, in order, with whether each is BitWindow's default.
fn logins(s: &Settings, app_dir: Option<&Path>) -> Vec<(String, String, bool)> {
    let dir = node_dir(s);
    let mut v = Vec::new();
    if let Some((u, p)) = typed_login(s, app_dir) {
        v.push((u, p, false));
    }
    if let Some((u, p)) = cookie(&dir) {
        v.push((u, p, false));
    }
    for f in [dir.join("bitcoin.conf"), bitwindow_conf()] {
        if let Some((u, p)) = std::fs::read_to_string(&f).ok().as_deref().and_then(conf_login) {
            v.push((u, p, false));
        }
    }
    v.push(("user".into(), "password".into(), true));
    // Each login once. BitWindow's conf usually holds the default login itself: say it as such.
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for (u, p, _) in v {
        if !out.iter().any(|o| o.0 == u && o.1 == p) {
            let default_login = u == "user" && p == "password";
            out.push((u, p, default_login));
        }
    }
    out
}

/// The eCash node's RPC address, host:port. RPC is plain HTTP: an https:// address is refused rather than quietly
/// spoken to over http (security review H2).
pub fn rpc_endpoint(s: &Settings) -> Result<String, String> {
    let raw = match &s.l1_rpc {
        Some(r) if !r.trim().is_empty() => r.trim(),
        _ => s.rest.trim(),
    };
    if raw.to_ascii_lowercase().starts_with("https://") {
        return Err("The eCash node's RPC is plain HTTP: enter it as host:port (over a private network or VPN for a node \
                    on another computer), not https://."
            .into());
    }
    let at = crate::node::detect::normalize_endpoint(raw);
    // Read as a URL is (re-review L-D): "[::1]@evil.example" would otherwise look like this computer while the request
    // goes to evil.example. Only host and port, nothing else.
    let bad = || format!("\"{at}\" isn't an eCash node's address: enter host:port.");
    let url = reqwest::Url::parse(&format!("http://{at}")).map_err(|_| bad())?;
    if !url.username().is_empty() || url.password().is_some() || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(bad());
    }
    let host = url.host_str().ok_or_else(bad)?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Whether host:port is this computer.
pub fn is_local(at: &str) -> bool {
    let host = crate::security::host_part(at);
    crate::security::is_loopback_host(host)
}

/// Connect: the first login the node accepts, and which network it is on. `app_dir` holds a typed login's password.
/// A node on another computer gets only the typed login, never this computer's cookie, confs or BitWindow's (review
/// L3). On eCash's own network, the node must have eCash's pinned block (it might be Bitcoin, or something else).
pub async fn connect(_http: &reqwest::Client, s: &Settings, app_dir: Option<&Path>) -> Result<Conn, String> {
    let at = rpc_endpoint(s)?;
    let base = format!("http://{}", at);
    // Its own client: never through an http_proxy (review H2), whatever the environment says.
    let http = reqwest::Client::builder().no_proxy().build().map_err(|e| e.to_string())?;
    let local = is_local(&at);
    let candidates: Vec<(String, String, bool)> = if local {
        logins(s, app_dir)
    } else {
        typed_login(s, app_dir).map(|(u, p)| vec![(u, p, false)]).unwrap_or_default()
    };
    if candidates.is_empty() {
        return Err(format!(
            "The eCash node at {} is on another computer: enter its login in Settings › Node & connection.",
            at
        ));
    }
    for (user, pass, default_login) in candidates {
        let conn = Conn { base: base.clone(), user, pass, http: http.clone(), chain: Chain::Main, default_login };
        match conn.node().call_typed("getblockchaininfo", vec![]).await {
            Ok(info) => {
                let chain = Chain::from_name(info["chain"].as_str().unwrap_or(""))
                    .map_err(|_| format!("The eCash node at {} is on \"{}\", not eCash.", at, info["chain"].as_str().unwrap_or("?")))?;
                if chain == Chain::Main {
                    let pinned = conn.node().call_typed("getblockhash", vec![json!(crate::node::PIN_HEIGHT)]).await;
                    if pinned.ok().and_then(|h| h.as_str().map(String::from)).as_deref() != Some(crate::node::PIN_HASH) {
                        return Err(format!("The node at {} isn't on eCash beta (it lacks eCash's pinned block).", at));
                    }
                }
                return Ok(Conn { chain, ..conn });
            }
            Err(RpcError::Http(401)) | Err(RpcError::Http(403)) => continue,
            Err(RpcError::Unreachable(_)) | Err(RpcError::Busy) => {
                return Err(format!("The eCash node isn't answering at {}. Is BitWindow (or your eCash node) running?", at))
            }
            Err(RpcError::Rpc { code: -28, .. }) => return Err("The eCash node is still starting. Try again in a minute.".into()),
            Err(e) => return Err(format!("The eCash node at {} answered: {}", at, e)),
        }
    }
    Err(format!(
        "The eCash node at {} refused every login FreeBank knows. Enter its login in Settings › Node & connection.",
        at
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conf_gives_its_login_from_any_section() {
        assert_eq!(conf_login("server=1\nrpcuser=alice\nrpcpassword=s3cret\n"), Some(("alice".into(), "s3cret".into())));
        assert_eq!(conf_login("[main]\nrpcuser = bob\nrpcpassword = pw\n"), Some(("bob".into(), "pw".into())));
        assert_eq!(conf_login("main.rpcuser=c\nmain.rpcpassword=d"), Some(("c".into(), "d".into())));
        // rpcauth alone holds only a hash: nothing to log in with.
        assert_eq!(conf_login("rpcauth=user:abc$def\n"), None);
        assert_eq!(conf_login("rpcuser=x\n"), None);
    }

    #[test]
    fn the_logins_come_in_bitwindows_order() {
        let dir = std::env::temp_dir().join(format!("fb-ecash-logins-{}-{}", std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".cookie"), "__cookie__:abc\n").unwrap();
        std::fs::write(dir.join("bitcoin.conf"), "rpcuser=user\nrpcpassword=password\n").unwrap();
        let s = Settings { l1_datadir: Some(dir.to_string_lossy().into_owned()), ..Settings::default() };
        let l = logins(&s, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(l[0], ("__cookie__".into(), "abc".into(), false));
        // The conf's login is BitWindow's default, said as such, and not tried twice.
        assert_eq!(l[1], ("user".into(), "password".into(), true));
        assert_eq!(l.iter().filter(|x| x.0 == "user").count(), 1);
    }

    #[test]
    fn a_typed_login_comes_first_and_its_password_is_this_users_only() {
        let app = std::env::temp_dir().join(format!("fb-ecash-typed-{}-{}", std::process::id(), rand::random::<u32>()));
        save_password(&app, Some("s3cret")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(password_path(&app)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let s = Settings { l1_user: Some("alice".into()), ..Settings::default() };
        assert_eq!(logins(&s, Some(&app))[0], ("alice".into(), "s3cret".into(), false));
        // Forgotten: back to the usual order.
        save_password(&app, Some("")).unwrap();
        assert!(!password_path(&app).exists());
        assert_ne!(logins(&s, Some(&app))[0].0, "alice");
        let _ = std::fs::remove_dir_all(&app);
    }
}
