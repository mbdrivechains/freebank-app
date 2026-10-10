//! Demo mode's relay (v0.4.4). A demo node runs without an eCash node: it asks an enforcer for every eCash fact
//! (`-mainchainrest=` empty, freebankd v0.2.25), and the enforcer it asks is FreeBank's read-only gateway. The node
//! speaks plain HTTP/1.1 (the Connect protocol, JSON) and its own TLS is too old for the internet, so it talks to this
//! relay on 127.0.0.1, and the relay forwards each call to the gateway over TLS, checking the gateway's certificate
//! against the system's roots.
//! - Only the six reads the node makes pass (ValidatorService, as at the gateway), and nothing a browser sends (Origin,
//!   Sec-Fetch-*). A refused first request hears 403 and never reaches the gateway; a refused later one closes the
//!   connection.
//! - Each request leaves with the gateway's name as its Host (the node writes 127.0.0.1:<port>) and only the headers
//!   in `HEADERS`. A body must give its length (Content-Length) and stay small.
//! - Replies come back as they are. One gateway connection per node connection, so the node's keep-alive carries over.
//!
//! The port is fixed, so a node left running finds the relay again when the app (or its background part) comes back.

use super::NodeManager;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// FreeBank's read-only enforcer gateway on the beta. Empty: no demo mode (set it so for mainnet).
pub const GATEWAY: &str = "enforcer.ecxfreebank.com";
const GATEWAY_PORT: u16 = 443;
/// Where the node finds the relay, beside the node's own 8454 (RPC) and 8455 (P2P).
pub const RELAY_PORT: u16 = 8453;
/// Why Keep running (and with it the phone's background part and start at login) is off in demo mode.
pub const DEMO_STOPS: &str =
    "In demo mode FreeBank's node stops when the app closes: it reaches the eCash chain only through the app.";
/// The first node release that runs without an eCash node.
const DEMO_NODE: (u32, u32, u32) = (0, 2, 25);

const SERVICE: &str = "/cusf.mainchain.v1.ValidatorService/";
/// The node's only reads (freebankd `src/l1client.cpp`).
const READS: [&str; 6] = [
    "GetChainTip",
    "GetBlockHeaderInfo",
    "GetBmmHStarCommitment",
    "GetCtip",
    "GetChainInfo",
    "GetTwoWayPegData",
];
/// The request headers passed on besides Host, in lower case (freebankd `src/enforcerconnect.cpp`).
const HEADERS: [&str; 5] = ["user-agent", "content-type", "connect-protocol-version", "connect-timeout-ms", "content-length"];
const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 64 * 1024;
/// The answer to a refused request.
const REFUSED: &str = "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: 74\r\nConnection: close\r\n\r\n\
                       {\"code\":\"permission_denied\",\"message\":\"not a read FreeBank's demo passes\"}";

/// The address the node is given as its enforcer in demo mode.
pub fn relay_addr() -> String {
    format!("127.0.0.1:{}", RELAY_PORT)
}

pub fn available() -> bool {
    !GATEWAY.is_empty()
}

/// The installed node release runs without an eCash node ("v0.2.25" and later).
pub fn node_can_demo(tag: &str) -> bool {
    let v: Vec<u32> = tag
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    v.len() == 3 && (v[0], v[1], v[2]) >= DEMO_NODE
}

/// Start the relay unless this process runs it already.
pub async fn ensure(mgr: &NodeManager) -> Result<(), String> {
    if !available() {
        return Err("Demo mode isn't available in this version of FreeBank.".into());
    }
    let _one = mgr.relay_start.lock().await;
    if mgr.relay_up.load(Ordering::SeqCst) {
        return Ok(());
    }
    // A background part that is stopping (the app took the phone link back) may still hold the port for a moment.
    let mut tries = 0;
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", RELAY_PORT)).await {
            Ok(l) => break l,
            Err(_) if tries < 10 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                return Err(format!(
                    "Demo mode needs port {} on this computer, and something else is using it ({}).",
                    RELAY_PORT, e
                ))
            }
        }
    };
    mgr.relay_up.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((s, _)) => {
                    tokio::spawn(async move {
                        let _ = serve(s, dial(GATEWAY, GATEWAY_PORT), GATEWAY).await;
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    });
    Ok(())
}

/// Setup's checklist in demo mode: the gateway answers through the relay.
pub async fn check(mgr: &NodeManager) -> super::detect::StackCheck {
    let tip = match ensure(mgr).await {
        Ok(()) => super::detect::enforcer_tip(&relay_addr()).await,
        Err(e) => Err(e),
    };
    let ok = tip.is_ok();
    super::detect::StackCheck {
        found: ok,
        rest_ok: ok,
        on_beta: ok,
        enforcer_ok: ok,
        l1_blocks: tip.as_ref().ok().map(|t| t.0),
        detail: tip.err().map(|e| format!("FreeBank's gateway didn't answer: {}", e)).unwrap_or_default(),
    }
}

/// The gateway's eCash height for the Node tab's status poll: asked at most every 30 seconds and for 3 seconds at most,
/// so polling never holds a status call or opens a TLS connection to the gateway each time. Restarts the relay if
/// it isn't running (the start-up attempt can give up while the background part still holds the port).
pub async fn tip(mgr: &NodeManager) -> Option<u64> {
    if let Some((at, h)) = *mgr.relay_tip.lock().unwrap() {
        if at.elapsed() < Duration::from_secs(30) {
            return h;
        }
    }
    let h = match ensure(mgr).await {
        Ok(()) => tokio::time::timeout(Duration::from_secs(3), super::detect::enforcer_tip(&relay_addr()))
            .await
            .ok()
            .and_then(|r| r.ok())
            .map(|t| t.0),
        Err(_) => None,
    };
    *mgr.relay_tip.lock().unwrap() = Some((std::time::Instant::now(), h));
    h
}

/// A TLS connection to the gateway, its certificate checked for `host`.
async fn dial(host: &'static str, port: u16) -> std::io::Result<tokio_native_tls::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect((host, port)).await?;
    let tls = native_tls::TlsConnector::builder()
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()
        .map_err(std::io::Error::other)?;
    tokio_native_tls::TlsConnector::from(tls).connect(host, tcp).await.map_err(std::io::Error::other)
}

/// One node connection: each request checked and rewritten on its way up, the replies copied back.
async fn serve<D, U, F>(down: D, connect: F, host: &str) -> Result<(), String>
where
    D: AsyncRead + AsyncWrite + Send + 'static,
    U: AsyncRead + AsyncWrite + Send + 'static,
    F: Future<Output = std::io::Result<U>>,
{
    let (mut dr, mut dw) = tokio::io::split(down);
    let mut buf = Vec::new();
    // The first request decides whether the gateway is dialled at all. Refused, it hears 403 in Connect's words (the
    // gateway's answer too). A refused request later on just closes the connection: the node then tries once more on
    // a fresh one, where it is the first.
    let first = match read_request(&mut dr, &mut buf, host).await {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = dw.write_all(REFUSED.as_bytes()).await;
            let _ = dw.shutdown().await;
            // Read on briefly, so unread bytes don't turn the close into a reset that loses the answer.
            let mut sink = [0u8; 8192];
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                let mut left = MAX_HEAD + MAX_BODY;
                while left > 0 {
                    match dr.read(&mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => left = left.saturating_sub(n),
                    }
                }
            })
            .await;
            return Err(e);
        }
    };
    let up = tokio::time::timeout(Duration::from_secs(10), connect)
        .await
        .map_err(|_| "the gateway didn't answer in time".to_string())?
        .map_err(|e| format!("couldn't reach the gateway: {}", e))?;
    let (mut ur, mut uw) = tokio::io::split(up);
    let back = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut ur, &mut dw).await;
        let _ = dw.shutdown().await;
    });
    let mut next = Some(first);
    while let Some(req) = next {
        if uw.write_all(&req).await.is_err() {
            break;
        }
        // The node waits for each reply before it asks again, so nothing is in flight when a request is refused.
        next = read_request(&mut dr, &mut buf, host).await.ok().flatten();
    }
    back.abort();
    Ok(())
}

/// The node's next whole request, ready for the gateway. None: the node closed the connection.
async fn read_request<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, host: &str) -> Result<Option<Vec<u8>>, String> {
    loop {
        if let Some((req, used)) = next_request(buf, host)? {
            buf.drain(..used);
            return Ok(Some(req));
        }
        let mut chunk = [0u8; 8192];
        let n = r.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The request at the front of `buf` as it leaves for the gateway, and how many bytes of `buf` it took. None: not
/// all of it has arrived. An error: it may not pass.
fn next_request(buf: &[u8], host: &str) -> Result<Option<(Vec<u8>, usize)>, String> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > MAX_HEAD { Err("a request head too long".into()) } else { Ok(None) };
    };
    if end > MAX_HEAD {
        return Err("a request head too long".into());
    }
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| "a request head that isn't text".to_string())?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (verb, path, version) = (first.next(), first.next(), first.next());
    if first.next().is_some() || verb != Some("POST") || version != Some("HTTP/1.1") {
        return Err("not a POST in HTTP/1.1".into());
    }
    let method = path
        .and_then(|p| p.strip_prefix(SERVICE))
        .filter(|m| READS.contains(m))
        .ok_or("not one of the node's reads")?;
    let mut out = format!("POST {}{} HTTP/1.1\r\nHost: {}\r\n", SERVICE, method, host);
    let mut len = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("a broken header")?;
        let value = value.trim_matches([' ', '\t']);
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err("a broken header".into());
        }
        let lower = name.to_ascii_lowercase();
        if lower == "transfer-encoding" {
            return Err("a body without a length".into());
        }
        // A web page's request: only the node may use the relay.
        if lower == "origin" || lower.starts_with("sec-fetch-") {
            return Err("a browser's request".into());
        }
        if lower == "content-length" {
            if len.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("a broken length".into());
            }
            let n: usize = value.parse().map_err(|_| "a broken length".to_string())?;
            if n > MAX_BODY {
                return Err("a request too long".into());
            }
            len = Some(n);
        }
        if HEADERS.contains(&lower.as_str()) {
            out.push_str(&format!("{}: {}\r\n", name, value));
        }
    }
    let len = len.ok_or("a body without a length")?;
    out.push_str("\r\n");
    let total = end + 4 + len;
    if buf.len() < total {
        return Ok(None);
    }
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&buf[end + 4..total]);
    Ok(Some((bytes, total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request as freebankd writes it (enforcerconnect.cpp `BuildRequest`).
    fn node_request(method: &str, body: &str) -> String {
        format!(
            "POST /cusf.mainchain.v1.ValidatorService/{} HTTP/1.1\r\nHost: 127.0.0.1:8453\r\nUser-Agent: freebankd\r\n\
             Content-Type: application/json\r\nConnect-Protocol-Version: 1\r\nConnect-Timeout-Ms: 15000\r\n\
             Content-Length: {}\r\n\r\n{}",
            method,
            body.len(),
            body
        )
    }

    fn pass(req: &str) -> String {
        let (out, used) = next_request(req.as_bytes(), "gw.example").unwrap().unwrap();
        assert_eq!(used, req.len());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_node_reads_pass_with_the_gateway_as_host() {
        for m in READS {
            let out = pass(&node_request(m, "{\"height\":5}"));
            assert!(out.starts_with(&format!("POST {}{} HTTP/1.1\r\nHost: gw.example\r\n", SERVICE, m)), "{}", out);
            assert!(!out.contains("127.0.0.1"), "{}", out);
            assert!(out.contains("Connect-Timeout-Ms: 15000\r\n") && out.ends_with("\r\n\r\n{\"height\":5}"), "{}", out);
        }
    }

    #[test]
    fn only_the_listed_headers_go_on() {
        let req = node_request("GetChainTip", "{}").replace("User-Agent", "Cookie: a=b\r\nX-Forwarded-For: 1.2.3.4\r\nUser-Agent");
        let out = pass(&req);
        assert!(!out.contains("Cookie") && !out.contains("X-Forwarded-For"), "{}", out);
        assert_eq!(out.matches("Host:").count(), 1, "{}", out);
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            node_request("GetChainTip", "{}").replace("POST", "GET"),
            node_request("GetChainTip", "{}").replace("HTTP/1.1", "HTTP/1.0"),
            node_request("CreateDepositTransaction", "{}"),
            node_request("GetChainTip", "{}").replace("ValidatorService", "WalletService"),
            node_request("GetChainTip", "{}").replace("cusf.mainchain.v1.ValidatorService/GetChainTip", "cusf.mainchain.v1.BlockProducerService/GetChainTip"),
            node_request("GetChainTip", "{}").replace("/GetChainTip", "/GetChainTip?x=1"),
            node_request("GetChainTip", "{}").replace("/GetChainTip", "/../WalletService/GetChainTip"),
            node_request("GetChainTip", "{}").replace("Content-Length: 2", "Transfer-Encoding: chunked"),
            node_request("GetChainTip", "{}").replace("Content-Length: 2\r\n", ""),
            node_request("GetChainTip", "{}").replace("Content-Length: 2", "Content-Length: 2\r\nContent-Length: 2"),
            node_request("GetChainTip", "{}").replace("Content-Length: 2", "Content-Length: +2"),
            node_request("GetChainTip", "{}").replace("User-Agent: freebankd", "User Agent: freebankd"),
            node_request("GetChainTip", "{}").replace("User-Agent: freebankd", "User-Agent: a\nHost: b"),
            node_request("GetChainTip", &"x".repeat(MAX_BODY + 1)),
            node_request("GetChainTip", "{}").replace("User-Agent", "Origin: https://example.com\r\nUser-Agent"),
            node_request("GetChainTip", "{}").replace("User-Agent", "Sec-Fetch-Mode: no-cors\r\nUser-Agent"),
        ] {
            assert!(next_request(bad.as_bytes(), "gw.example").is_err(), "{:?}", bad);
        }
        assert!(next_request(&vec![b'a'; MAX_HEAD + 1], "gw.example").is_err());
    }

    #[test]
    fn a_partial_request_waits_and_a_second_one_stays() {
        let one = node_request("GetCtip", "{\"sidechain_number\":130}");
        for cut in [10, one.len() - 5] {
            assert!(next_request(&one.as_bytes()[..cut], "gw.example").unwrap().is_none());
        }
        let two = format!("{}{}", one, node_request("GetChainTip", "{}"));
        let (_, used) = next_request(two.as_bytes(), "gw.example").unwrap().unwrap();
        assert_eq!(used, one.len());
        assert!(pass(&two[used..]).contains("/GetChainTip HTTP/1.1"));
    }

    #[test]
    fn the_refusal_says_its_length() {
        let (head, body) = REFUSED.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())), "{}", REFUSED);
        assert!(serde_json::from_str::<serde_json::Value>(body).is_ok());
    }

    #[test]
    fn demo_needs_node_v0_2_25() {
        for (tag, ok) in [("v0.2.25", true), ("v0.2.26", true), ("v0.3.0", true), ("v1.0.0", true), ("v0.2.25-rc1", true),
            ("v0.2.24", false), ("v0.2.9", false), ("v0.2", false), ("", false), ("kestrel", false)]
        {
            assert_eq!(node_can_demo(tag), ok, "{}", tag);
        }
    }

    /// The relay end to end with a fake gateway: two calls on one connection, then a refused one that never arrives.
    #[tokio::test]
    async fn serve_relays_the_reads_and_drops_the_rest() {
        let (node, relay_side) = tokio::io::duplex(1 << 16);
        let (gw_side, mut gateway) = tokio::io::duplex(1 << 16);
        let relay = tokio::spawn(serve(relay_side, async move { Ok(gw_side) }, "gw.example"));
        let (mut nr, mut nw) = tokio::io::split(node);

        let mut got = Vec::new();
        for (m, reply) in [("GetChainTip", "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"), ("GetCtip", "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n{\"a\"")] {
            let req = node_request(m, "{}");
            nw.write_all(req.as_bytes()).await.unwrap();
            let mut seen = vec![0u8; req.len() - "127.0.0.1:8453".len() + "gw.example".len()];
            gateway.read_exact(&mut seen).await.unwrap();
            let seen = String::from_utf8(seen).unwrap();
            assert!(seen.contains(&format!("/{} HTTP/1.1\r\nHost: gw.example\r\n", m)), "{}", seen);
            gateway.write_all(reply.as_bytes()).await.unwrap();
            let mut back = vec![0u8; reply.len()];
            nr.read_exact(&mut back).await.unwrap();
            got.push(String::from_utf8(back).unwrap());
        }
        assert!(got[1].ends_with("{\"a\""));

        nw.write_all(node_request("GetChainTip", "{}").replace("ValidatorService", "WalletService").as_bytes()).await.unwrap();
        relay.await.unwrap().unwrap();
        let mut rest = Vec::new();
        gateway.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "the refused call reached the gateway: {:?}", String::from_utf8_lossy(&rest));
    }

    /// A refused first request: the gateway is never dialled.
    #[tokio::test]
    async fn a_refused_first_request_dials_nothing() {
        let (mut node, relay_side) = tokio::io::duplex(1 << 16);
        let dialled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d = dialled.clone();
        let relay = tokio::spawn(serve(
            relay_side,
            async move {
                d.store(true, Ordering::SeqCst);
                Ok(tokio::io::duplex(64).0)
            },
            "gw.example",
        ));
        node.write_all(node_request("GetChainTip", "{}").replace("POST", "GET").as_bytes()).await.unwrap();
        let mut back = Vec::new();
        node.read_to_end(&mut back).await.unwrap();
        assert_eq!(String::from_utf8(back).unwrap(), REFUSED);
        assert!(relay.await.unwrap().is_err());
        assert!(!dialled.load(Ordering::SeqCst));
    }

    /// TLS to a real host with its certificate checked: `cargo test -- --ignored relay_tls`. Any HTTP answer will do.
    #[tokio::test]
    #[ignore]
    async fn relay_tls_reaches_a_real_host() {
        let mut s = dial("explorer.ecxfreebank.com", 443).await.unwrap();
        s.write_all(node_request("GetChainTip", "{}").replace("127.0.0.1:8453", "explorer.ecxfreebank.com").as_bytes())
            .await
            .unwrap();
        let mut head = [0u8; 12];
        s.read_exact(&mut head).await.unwrap();
        assert!(head.starts_with(b"HTTP/1.1 "), "{:?}", String::from_utf8_lossy(&head));
    }
}
