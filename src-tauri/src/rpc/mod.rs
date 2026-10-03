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

/// FreeBank JSON-RPC client
pub struct FreeBankClient {
    url: Option<String>,
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
            auth: None,
            client,
            timeout: None,
            local_datadir: None,
        }
    }

    /// Give up on a call after `t` (for status polls; wallet calls keep no limit).
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    /// Configure the RPC connection
    pub fn configure(&mut self, url: &str, user: &str, password: &str) {
        self.url = Some(url.to_string());
        self.auth = Some(STANDARD.encode(format!("{}:{}", user, password)));
        self.local_datadir = None;
    }

    /// Configure for the node on this computer, whose credentials live in `datadir`.
    pub fn configure_local(&mut self, url: &str, datadir: PathBuf) -> bool {
        let Some((u, p)) = crate::node::detect::rpc_auth(&datadir) else {
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

    /// Make a JSON-RPC call; if the local node refuses our (stale) cookie, re-read it once and retry.
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
            Err(RpcError::Http(401)) if self.refresh_local_auth() => self.call_typed(method, params).await,
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
        let url = self.url.as_ref().ok_or(RpcError::NotConfigured)?;
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls: Calls = Arc::default();
        let log = calls.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let (mut line, mut len) = (String::new(), 0usize);
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                        break;
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
                log.lock().unwrap().push((method.clone(), req["params"].clone()));
                let (status, reply) = match answer(&method, &req["params"]) {
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
