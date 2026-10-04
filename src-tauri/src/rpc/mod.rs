//! JSON-RPC client for freebankd (FreeBank drivechain, slot 130)

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Bitcoin Core's "still loading" code: the node is up but warming up (block index, eCash checks).
pub const RPC_IN_WARMUP: i64 = -28;

/// JSON-RPC "Method not found": the node is older than the call (the test node answers it).
#[cfg(test)]
pub const RPC_METHOD_NOT_FOUND: i64 = -32601;

/// Why a call failed, kept apart so callers can tell "no node" from "node busy" from "wrong password".
#[derive(Debug)]
pub enum RpcError {
    NotConfigured,
    /// Nothing answered at the address (connection refused, timeout).
    Unreachable(String),
    /// Took longer than the call's timeout (a node busy verifying blocks answers late).
    Busy,
    /// Answered with an HTTP error and no JSON-RPC body (401 = wrong credentials).
    Http(u16),
    /// The node answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
    Other(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::NotConfigured => write!(f, "RPC not configured"),
            RpcError::Unreachable(e) => write!(f, "Request failed: {}", e),
            RpcError::Busy => write!(f, "The node is busy; try again in a moment"),
            RpcError::Http(s) => write!(f, "HTTP error: {}", s),
            RpcError::Rpc { message, .. } => write!(f, "RPC error: {}", message),
            RpcError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl RpcError {
    /// The form the screens get. A node's error keeps its code, "RPC error -13: …", so the
    /// frontend can act on it (src/lib/errors.ts, `rpcCode`); anything else reads as Display.
    /// Display itself stays "RPC error: …" for the phone relay and the node screens.
    pub fn for_ui(&self) -> String {
        match self {
            RpcError::Rpc { code, message } => format!("RPC error {}: {}", code, message),
            e => e.to_string(),
        }
    }
}

/// Several wallets in one app (v0.2.6): once the local node has more than its main wallet open, every wallet call
/// must name its wallet (`/wallet/<name>`), or the node refuses it (-19). Then the app's clients for the local node
/// name the main wallet unless the screens chose another (`set_wallet`). None: only the main wallet is open, and
/// calls go to the node's root as before.
static MAIN_WALLET: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

pub fn set_main_wallet(name: Option<String>) {
    *MAIN_WALLET.write().unwrap() = name;
}

pub fn main_wallet() -> Option<String> {
    MAIN_WALLET.read().unwrap().clone()
}

/// Core's RPC_WALLET_NOT_FOUND: a wallet named in the path that isn't open (after the node restarted).
const WALLET_NOT_LOADED: i64 = -18;

/// A wallet name in a URL path: letters, digits and . _ - as they are, anything else %-escaped.
fn path_name(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => (b as char).to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

/// FreeBank JSON-RPC client
pub struct FreeBankClient {
    url: Option<String>,
    /// The wallet the screens chose (v0.2.6); None: the main wallet.
    wallet: Option<String>,
    auth: Option<String>,
    client: reqwest::Client,
    timeout: Option<Duration>,
    /// Set for the node on this computer: its datadir holds the cookie, which changes on every start.
    local_datadir: Option<PathBuf>,
}

impl Default for FreeBankClient {
    fn default() -> Self {
        Self::with_http(reqwest::Client::new())
    }
}

impl FreeBankClient {
    /// A client that shares an existing HTTP connection pool.
    pub fn with_http(client: reqwest::Client) -> Self {
        Self {
            url: None,
            wallet: None,
            auth: None,
            client,
            timeout: None,
            local_datadir: None,
        }
    }

    /// Which wallet the screens' calls go to (None: the main wallet). The phone's calls always go to the main one
    /// (`call_fresh_typed_main`).
    pub fn set_wallet(&mut self, wallet: Option<String>) {
        self.wallet = wallet;
    }

    #[cfg(test)]
    pub fn wallet(&self) -> Option<&str> {
        self.wallet.as_deref()
    }

    /// The same connection on the main wallet, whichever the screens chose: for what is about the main wallet and its
    /// words (Settings › Wallet, the security checks). Hold the shared client's lock while using it.
    pub fn for_main(&self) -> FreeBankClient {
        FreeBankClient {
            url: self.url.clone(),
            wallet: None,
            auth: self.auth.clone(),
            client: self.client.clone(),
            timeout: self.timeout,
            local_datadir: self.local_datadir.clone(),
        }
    }

    /// The wallet a call names: for the local node, the chosen one or (with other wallets open) the main one; for a
    /// node elsewhere, only one the screens chose.
    fn wallet_for(&self, main_only: bool) -> Option<String> {
        if self.local_datadir.is_none() {
            return if main_only { None } else { self.wallet.clone() };
        }
        if main_only {
            main_wallet()
        } else {
            self.wallet.clone().or_else(main_wallet)
        }
    }

    /// Give up on a call after `t` (for status polls; wallet calls keep no limit).
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    /// The address it calls (tests point other clients at the same stand-in).
    #[cfg(test)]
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// No limit on a call (an eCash wallet's import rescans the chain).
    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Configure the RPC connection
    pub fn configure(&mut self, url: &str, user: &str, password: &str) {
        self.url = Some(url.to_string());
        self.auth = Some(STANDARD.encode(format!("{}:{}", user, password)));
        self.local_datadir = None;
    }

    /// Configure for the node on this computer, whose credentials live in `datadir`.
    /// Without a cookie yet (the node isn't running), it still remembers where the node will be, and the first call
    /// after the node starts reads the cookie (`call_fresh_typed`); it says false and has no login until then.
    pub fn configure_local(&mut self, url: &str, datadir: PathBuf) -> bool {
        let Some((u, p)) = crate::node::detect::rpc_auth(&datadir) else {
            self.url = Some(url.to_string());
            self.auth = None;
            self.local_datadir = Some(datadir);
            return false;
        };
        self.configure(url, &u, &p);
        self.local_datadir = Some(datadir);
        true
    }

    /// Re-read the local node's cookie after a restart. False if this isn't the local node.
    pub fn refresh_local_auth(&mut self) -> bool {
        let Some(dir) = self.local_datadir.clone() else {
            return false;
        };
        let Some(url) = self.url.clone() else {
            return false;
        };
        self.configure_local(&url, dir)
    }

    /// Make a JSON-RPC call; if the local node refuses our (stale) cookie, or there was none yet, re-read it once and
    /// retry.
    pub async fn call_fresh(&mut self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        self.call_fresh_typed(method, params).await.map_err(|e| e.to_string())
    }

    /// `call_fresh` for the screens: a node's error keeps its code (`RpcError::for_ui`).
    pub async fn call_ui(&mut self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        self.call_fresh_typed(method, params).await.map_err(|e| e.for_ui())
    }

    /// `call_fresh`, keeping the kind of failure (the phone relay acts on Core's error codes).
    pub async fn call_fresh_typed(&mut self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        match self.call_typed(method, params.clone()).await {
            Err(RpcError::Http(401) | RpcError::NotConfigured) if self.refresh_local_auth() => {
                self.call_typed(method, params).await
            }
            r => r,
        }
    }

    /// `call_fresh_typed` on the main wallet, whichever the screens chose: the phone's (the operator chose "Main
    /// wallet only" for the phone, 2026-10-04).
    pub async fn call_fresh_typed_main(&mut self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        match self.call_with(true, method, params.clone()).await {
            Err(RpcError::Http(401) | RpcError::NotConfigured) if self.refresh_local_auth() => {
                self.call_with(true, method, params).await
            }
            r => r,
        }
    }

    /// Check if client is configured
    pub fn is_configured(&self) -> bool {
        self.url.is_some() && self.auth.is_some()
    }

    /// Make a JSON-RPC call
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        self.call_typed(method, params).await.map_err(|e| e.to_string())
    }

    /// Make a JSON-RPC call, keeping the kind of failure.
    pub async fn call_typed(&self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        self.call_with(false, method, params).await
    }

    /// A call to the node itself, naming no wallet (createwallet, loadwallet, listwallets).
    pub async fn call_root(&self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        self.post(None, method, params).await
    }

    /// A call to the wallet `wallet_for` names. A wallet that isn't open (the node restarted since) is opened and the
    /// call made again, once.
    async fn call_with(&self, main_only: bool, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        let Some(w) = self.wallet_for(main_only) else { return self.post(None, method, params).await };
        match self.post(Some(&w), method, params.clone()).await {
            Err(RpcError::Rpc { code: WALLET_NOT_LOADED, .. }) => {
                match self.post(None, "loadwallet", vec![json!(w)]).await {
                    // -35: opened meanwhile by another call.
                    Ok(_) | Err(RpcError::Rpc { code: -35, .. }) => {}
                    Err(e) => return Err(e),
                }
                self.post(Some(&w), method, params).await
            }
            r => r,
        }
    }

    async fn post(&self, wallet: Option<&str>, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        let base = self.url.as_ref().ok_or(RpcError::NotConfigured)?;
        let at;
        let url = match wallet {
            Some(w) => {
                at = format!("{}/wallet/{}", base.trim_end_matches('/'), path_name(w));
                &at
            }
            None => base,
        };
        let auth = self.auth.as_ref().ok_or(RpcError::NotConfigured)?;

        let body = json!({
            "jsonrpc": "1.0",
            "id": "freebank-app",
            "method": method,
            "params": params
        });

        let mut req = self
            .client
            .post(url)
            .header("Authorization", format!("Basic {}", auth))
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(t) = self.timeout {
            req = req.timeout(t);
        }
        let response = req
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    RpcError::Busy
                } else {
                    RpcError::Unreachable(e.to_string())
                }
            })?;

        // Core answers RPC errors (warm-up included) with HTTP 500 and a JSON body, so read the
        // body first and fall back to the status only when there is none.
        let status = response.status();
        let result: Value = match response.json().await {
            Ok(v) => v,
            Err(e) if status.is_success() => {
                return Err(RpcError::Other(format!("JSON parse error: {}", e)))
            }
            Err(_) => return Err(RpcError::Http(status.as_u16())),
        };

        // Check for RPC error
        if let Some(error) = result.get("error") {
            if !error.is_null() {
                return Err(RpcError::Rpc {
                    code: error["code"].as_i64().unwrap_or(0),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("Unknown RPC error")
                        .to_string(),
                });
            }
        }

        Ok(result["result"].clone())
    }
}

#[cfg(test)]
pub(crate) mod stub {
    //! A local stand-in for freebankd's JSON-RPC, for tests: it answers each call from `answer` and
    //! records (method, params). One request per connection.
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{Arc, Mutex};

    pub type Calls = Arc<Mutex<Vec<(String, Value)>>>;

    /// A client pointed at a new stub. `answer(method, params)` gives the result, or (code, message)
    /// for a JSON-RPC error, sent with HTTP 404 for -32601 and 500 otherwise, as Core does.
    pub fn serve<F>(answer: F) -> (super::FreeBankClient, Calls)
    where
        F: Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + 'static,
    {
        serve_paths(move |_, m, p| answer(m, p))
    }

    /// The same, with the request's path too ("/", "/wallet/<name>"). Calls are recorded as (method, params)
    /// with the method written "<path> <method>" when the path isn't "/".
    pub fn serve_paths<F>(answer: F) -> (super::FreeBankClient, Calls)
    where
        F: Fn(&str, &str, &Value) -> Result<Value, (i64, String)> + Send + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls: Calls = Arc::default();
        let log = calls.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let (mut line, mut len, mut path) = (String::new(), 0usize, String::new());
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                        break;
                    }
                    if path.is_empty() {
                        path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = req["method"].as_str().unwrap_or("").to_string();
                let shown = if path == "/" || path.is_empty() { method.clone() } else { format!("{} {}", path, method) };
                log.lock().unwrap().push((shown, req["params"].clone()));
                let (status, reply) = match answer(&path, &method, &req["params"]) {
                    Ok(v) => ("200 OK", json!({"result": v, "error": null, "id": req["id"]})),
                    Err((code, message)) => (
                        if code == super::RPC_METHOD_NOT_FOUND { "404 Not Found" } else { "500 Internal Server Error" },
                        json!({"result": null, "error": {"code": code, "message": message}, "id": req["id"]}),
                    ),
                };
                let body = reply.to_string();
                let head = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status,
                    body.len()
                );
                let _ = conn.write_all(head.as_bytes()).and_then(|_| conn.write_all(body.as_bytes()));
            }
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut client = super::FreeBankClient::with_http(http);
        client.configure(&url, "user", "pass");
        (client, calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_screens_get_the_nodes_error_code() {
        let (mut c, _) = stub::serve(|m, _| match m {
            "sendtoaddress" => Err((-13, "Error: Please enter the wallet passphrase with walletpassphrase first.".into())),
            _ => Ok(json!(7)),
        });
        let e = c.call_ui("sendtoaddress", vec![json!("X"), json!(1)]).await.unwrap_err();
        assert_eq!(e, "RPC error -13: Error: Please enter the wallet passphrase with walletpassphrase first.");
        // Everyone else still reads "RPC error: …" (the phone relay passes it on to the phone).
        let e = c.call_fresh("sendtoaddress", vec![]).await.unwrap_err();
        assert_eq!(e, "RPC error: Error: Please enter the wallet passphrase with walletpassphrase first.");
        assert_eq!(c.call_ui("getblockcount", vec![]).await.unwrap(), json!(7));
        // Failures that aren't the node's answer read as before.
        assert_eq!(RpcError::Busy.for_ui(), RpcError::Busy.to_string());
    }
}
