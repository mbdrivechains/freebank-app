//! The outbound WebSocket to the relay: `host` + `proof` (ECDSA with D over the relay's
//! challenge), then frames both ways, pings every 30 s, reconnect with backoff (1 s doubling to
//! 60 s). It runs only while `Phone::wanted()`: a phone is paired or a pairing is open.

use super::Phone;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(30);
const BACKOFF_MAX: u64 = 60;
/// While off, look again this often (a pairing code can expire without a wake).
const OFF_RECHECK: Duration = Duration::from_secs(30);

enum End {
    /// The URL changed or there is nothing to do: go round again at once.
    Again,
    /// Close code 4001: another desktop proved the same room. Back off long; don't fight it.
    Replaced,
    /// Failed; `online` says whether it got as far as proving the room.
    Failed { online: bool, why: String },
}

pub async fn run(phone: Arc<Phone>, mut out: mpsc::UnboundedReceiver<Value>) {
    let mut backoff = 1u64;
    loop {
        if !phone.wanted() {
            phone.set_status("off", "No phone is paired.");
            while out.try_recv().is_ok() {}
            let _ = timeout(OFF_RECHECK, phone.wake.notified()).await;
            continue;
        }
        let url = phone.relay_url();
        phone.set_status("connecting", &url);
        let end = connect(&phone, &url, &mut out).await;
        phone.clear_channels();
        match end {
            End::Again => backoff = 1,
            End::Replaced => {
                backoff = BACKOFF_MAX;
                eprintln!("phone relay: replaced by another desktop for room {}", phone.room);
                phone.set_status(
                    "retrying",
                    &format!("Another FreeBank desktop with this key took over the relay. Trying again in {BACKOFF_MAX} s."),
                );
                let _ = timeout(Duration::from_secs(BACKOFF_MAX), phone.wake.notified()).await;
            }
            End::Failed { online, why } => {
                if online {
                    backoff = 1;
                }
                phone.set_status("retrying", &format!("{why} Trying again in {backoff} s."));
                let _ = timeout(Duration::from_secs(backoff), phone.wake.notified()).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

fn fail(why: impl std::fmt::Display, online: bool) -> End {
    End::Failed { online, why: format!("{why}.") }
}

type WsRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

fn closed(c: Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>, online: bool) -> End {
    match c {
        Some(c) if u16::from(c.code) == 4001 => End::Replaced,
        Some(c) if !c.reason.is_empty() => fail(format_args!("The relay closed the connection ({})", c.reason), online),
        Some(c) => fail(format_args!("The relay closed the connection (code {})", u16::from(c.code)), online),
        None => fail("The relay closed the connection", online),
    }
}

/// Read until a frame of type `want`, ignoring others, within the connect timeout.
async fn await_frame(rx: &mut WsRead, want: &str) -> Result<Value, End> {
    let r = timeout(CONNECT_TIMEOUT, async {
        while let Some(m) = rx.next().await {
            match m {
                Ok(Message::Text(t)) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                    if v["t"] == want {
                        return Ok(v);
                    }
                }
                Ok(Message::Close(c)) => return Err(closed(c, false)),
                Ok(_) => {}
                Err(e) => return Err(fail(e, false)),
            }
        }
        Err(fail("The relay closed the connection", false))
    })
    .await;
    match r {
        Err(_) => Err(fail(format_args!("The relay sent no \"{want}\""), false)),
        Ok(r) => r,
    }
}

async fn connect(phone: &Arc<Phone>, url: &str, out: &mut mpsc::UnboundedReceiver<Value>) -> End {
    let ws = match timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url)).await {
        Err(_) => return fail("The relay didn't answer", false),
        Ok(Err(e)) => return fail(format_args!("Can't reach the relay ({e})"), false),
        Ok(Ok((ws, _))) => ws,
    };
    let (mut tx, mut rx) = ws.split();
    if let Err(e) = tx.send(Message::text(phone.host_frame().to_string())).await {
        return fail(e, false);
    }

    // The challenge, then our proof, then "ready".
    let n = match await_frame(&mut rx, "challenge").await {
        Ok(v) => v["n"].as_str().unwrap_or("").to_string(),
        Err(e) => return e,
    };
    let proof = match phone.proof_frame(&n) {
        Ok(p) => p,
        Err(e) => return fail(format_args!("Bad challenge from the relay ({e})"), false),
    };
    if let Err(e) = tx.send(Message::text(proof.to_string())).await {
        return fail(e, false);
    }
    if let Err(e) = await_frame(&mut rx, "ready").await {
        return e;
    }
    // Anything queued for channels of an earlier connection is meaningless now.
    while out.try_recv().is_ok() {}
    phone.set_status("online", url);

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            m = rx.next() => match m {
                None => return fail("The relay closed the connection", true),
                Some(Ok(Message::Close(c))) => return closed(c, true),
                Some(Err(e)) => return fail(e, true),
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        phone.handle_frame(v);
                    }
                }
                Some(Ok(_)) => {}
            },
            f = out.recv() => {
                let Some(f) = f else { return End::Again };
                if let Err(e) = tx.send(Message::text(f.to_string())).await {
                    return fail(e, true);
                }
            }
            _ = phone.wake.notified() => {
                if phone.relay_url() != url || !phone.wanted() {
                    // Nothing left to do here, but what is queued goes first: revoking the last
                    // phone queues its "denied" and the channel's close, then wakes us.
                    if phone.relay_url() == url {
                        while let Ok(f) = out.try_recv() {
                            if tx.send(Message::text(f.to_string())).await.is_err() {
                                break;
                            }
                        }
                    }
                    let _ = tx.send(Message::Close(None)).await;
                    return End::Again;
                }
            }
            _ = ping.tick() => {
                if let Err(e) = tx.send(Message::Ping(Vec::new().into())).await {
                    return fail(e, true);
                }
            }
        }
    }
}

/// Wait for the link to report `state` (for tests and callers that need it up).
#[cfg(test)]
pub async fn wait_for(phone: &Phone, state: &str, within: Duration) -> bool {
    let t0 = tokio::time::Instant::now();
    while t0.elapsed() < within {
        if phone.status().state == state {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}
