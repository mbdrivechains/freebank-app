//! Which parts of FreeBank are open on the node's network: the credit gate (notes, houses, bills,
//! pools) and, later, the gold unit. freebankd v0.2.17 is to report them through `getgateinfo`
//! (V0216_PLAN.md §7.1, operator's decisions D-2026-09-29-5 and -6), so opening credit or gold needs
//! no app release. A node without it (v0.2.16 and older) keeps today's behaviour: credit open, as on
//! beta now, and gold closed.

use crate::commands::ClientState;
use crate::rpc::{FreeBankClient, RpcError, RPC_METHOD_NOT_FOUND};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateInfo {
    /// Notes, houses, bills and pools may be used.
    pub credit_open: bool,
    /// The gold unit is on. No gold screen exists yet: this is a flag for later.
    pub gold_open: bool,
    /// Where the answer came from: "node" (getgateinfo), "node-unrecognised" (getgateinfo answered
    /// in a shape this app doesn't read, so the defaults apply), or "default" (the node has no
    /// getgateinfo).
    pub source: &'static str,
}

impl GateInfo {
    /// A node from before the gates: credit as today, gold closed.
    pub fn before_gates() -> Self {
        GateInfo { credit_open: true, gold_open: false, source: "default" }
    }
}

/// The gates, from the node. Any failure other than "Method not found" (a node still starting, say)
/// is an error, so the screen keeps what it had and asks again later.
pub async fn read(c: &mut FreeBankClient) -> Result<GateInfo, String> {
    match c.call_fresh_typed("getgateinfo", vec![]).await {
        Ok(v) => Ok(parse(&v)),
        Err(RpcError::Rpc { code: RPC_METHOD_NOT_FOUND, .. }) => Ok(GateInfo::before_gates()),
        Err(e) => Err(e.for_ui()),
    }
}

/// CONFIRM AGAINST freebankd v0.2.17. `getgateinfo` isn't written yet (checked 2026-09-29: only
/// V0216_PLAN.md mentions it, "an RPC that reports each gate's height and whether it is open (e.g.
/// getgateinfo: credit, later gold)"), so this reads the shapes it is likely to take:
///   {"credit": {"open": true, "height": 0}, "gold": {"open": false, "height": null}}
///   {"credit": true, "gold": false}                  {"credit": "open", "gold": "closed"}
///   {"credit_open": true, "gold_open": false}
///   {"gates": [{"name": "credit", "open": true}, …]} or "gates" as an object of the above
/// An entry with a height and no open flag is open once the answer's own tip height ("height",
/// "blocks" or "tip") reaches it; a null or negative height is "never". Credit that can't be read
/// keeps today's behaviour (open: the node still refuses what it doesn't allow) and says so in
/// `source`; gold that can't be read is closed.
pub fn parse(v: &Value) -> GateInfo {
    let v = match v.get("gates") {
        Some(g) if g.is_object() => g,
        _ => v,
    };
    let tip = ["tip", "tipheight", "tip_height", "height", "blocks"].iter().find_map(|k| v.get(*k)?.as_i64());
    let credit = gate(v, "credit", tip);
    GateInfo {
        credit_open: credit.unwrap_or(true),
        gold_open: gate(v, "gold", tip).unwrap_or(false),
        source: if credit.is_some() { "node" } else { "node-unrecognised" },
    }
}

/// One gate's state, or None when the answer doesn't say.
fn gate(v: &Value, name: &str, tip: Option<i64>) -> Option<bool> {
    if let Some(b) = v.get(format!("{}_open", name)).and_then(Value::as_bool) {
        return Some(b);
    }
    let entry = v.get(name).or_else(|| {
        v.get("gates")?.as_array()?.iter().find(|g| {
            ["name", "gate", "id"].iter().any(|k| g.get(*k).and_then(Value::as_str) == Some(name))
        })
    })?;
    match entry {
        Value::Bool(b) => Some(*b),
        Value::String(s) => word(s),
        Value::Object(o) => {
            for k in ["open", "is_open", "active", "enabled"] {
                if let Some(b) = o.get(k).and_then(Value::as_bool) {
                    return Some(b);
                }
            }
            for k in ["status", "state"] {
                if let Some(b) = o.get(k).and_then(Value::as_str).and_then(word) {
                    return Some(b);
                }
            }
            for k in ["height", "open_height", "opens_at", "activation_height", "activationheight"] {
                match o.get(k) {
                    Some(Value::Null) => return Some(false),
                    Some(Value::String(s)) if s.eq_ignore_ascii_case("never") => return Some(false),
                    Some(h) if h.is_i64() => {
                        let h = h.as_i64().unwrap_or(-1);
                        return if h < 0 { Some(false) } else { tip.map(|t| t >= h) };
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

fn word(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "open" | "active" | "enabled" | "on" => Some(true),
        "closed" | "inactive" | "disabled" | "off" | "never" | "pending" => Some(false),
        _ => None,
    }
}

/// `{credit_open, gold_open, source}` for the screens (src/lib/gates.ts).
#[tauri::command]
pub async fn gate_info(client: State<'_, ClientState>) -> Result<GateInfo, String> {
    let mut c = client.lock().await;
    read(&mut c).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::stub;
    use serde_json::json;

    fn open(credit: bool, gold: bool) -> GateInfo {
        GateInfo { credit_open: credit, gold_open: gold, source: "node" }
    }

    #[test]
    fn reads_the_likely_shapes() {
        let cases = [
            (json!({"credit": {"open": false, "height": null}, "gold": {"open": false, "height": null}}), open(false, false)),
            (json!({"credit": {"open": true, "height": 0}, "gold": {"open": false}}), open(true, false)),
            (json!({"credit": true, "gold": true}), open(true, true)),
            (json!({"credit": "open", "gold": "closed"}), open(true, false)),
            (json!({"credit_open": false, "gold_open": false}), open(false, false)),
            (json!({"gates": [{"name": "credit", "active": true}, {"name": "gold", "active": false}]}), open(true, false)),
            (json!({"gates": {"credit": {"status": "closed"}, "gold": {"status": "closed"}}}), open(false, false)),
            // Heights: open once the answer's tip reaches them; null or negative is "never".
            (json!({"height": 973_800, "credit": {"height": 973_728}, "gold": {"height": null}}), open(true, false)),
            (json!({"blocks": 100, "credit": {"height": 973_728}, "gold": {"height": -1}}), open(false, false)),
            (json!({"credit": {"activation_height": "never"}}), open(false, false)),
        ];
        for (answer, want) in cases {
            assert_eq!(parse(&answer), want, "{}", answer);
        }
    }

    #[test]
    fn an_unread_answer_keeps_credit_open_and_gold_closed() {
        for answer in [json!({}), json!({"something": 1}), json!({"credit": {"height": 5}}), json!(null), json!([1, 2])] {
            let g = parse(&answer);
            assert_eq!((g.credit_open, g.gold_open, g.source), (true, false, "node-unrecognised"), "{}", answer);
        }
        // Gold alone unread: closed, while credit is still read from the node.
        assert_eq!(parse(&json!({"credit": false})), open(false, false));
    }

    #[tokio::test]
    async fn a_node_without_getgateinfo_keeps_credit_open() {
        let (mut c, calls) = stub::serve(|m, _| match m {
            "getgateinfo" => Err((RPC_METHOD_NOT_FOUND, "Method not found".into())),
            _ => Ok(Value::Null),
        });
        assert_eq!(read(&mut c).await.unwrap(), GateInfo::before_gates());
        assert_eq!(calls.lock().unwrap()[0].0, "getgateinfo");
    }

    #[tokio::test]
    async fn a_node_with_getgateinfo_is_read_and_other_errors_are_errors() {
        let (mut c, _) = stub::serve(|_, _| Ok(json!({"credit": {"open": false}, "gold": {"open": false}})));
        assert_eq!(read(&mut c).await.unwrap(), open(false, false));

        let (mut c, _) = stub::serve(|_, _| Err((-28, "Loading block index...".into())));
        let e = read(&mut c).await.unwrap_err();
        assert_eq!(e, "RPC error -28: Loading block index...");
    }
}
