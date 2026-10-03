//! "Report a problem or suggest something" (operator, 2026-09-30: "We should also have an issue/s
//! suggestion button. Ideally going to github"; then, for people without a GitHub account, a form to
//! our server). "Send to FreeBank" posts the report to the relay's `POST /feedback` at
//! app.ecxfreebank.com (distribution `relay/src/feedback.rs`), which keeps it privately. The page only
//! ever sees `feedback_details` and a reference back: the request goes from here.
//!
//! What goes: the kind, the text, a contact if given, and, if the user leaves the box ticked, the
//! app's version, the system and the node's version (`feedback_details`, shown to them first).
//! If they tick "Include recent activity", also the app's and the node's latest log lines, masked
//! (`activity.rs`: no hashes, addresses, IP addresses, long tokens or home folder) and shown to them
//! first (`feedback_activity`). Nothing else: no balances or amounts.

use crate::node::{NodeManager, APP_VERSION};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tauri::State;

/// The relay's report desk. `FREEBANK_FEEDBACK_URL` points it elsewhere for testing.
const FEEDBACK_URL: &str = "https://app.ecxfreebank.com/feedback";
/// The relay's limits (`relay/src/feedback.rs`): the text and the contact, in characters.
const MAX_TEXT: usize = 8000;
/// The text's share of the server's 64 KiB body (the recent activity has its own).
const MAX_TEXT_BYTES: usize = 14_000;
const MAX_CONTACT: usize = 200;

/// What the report may carry about this computer, shown to the user before it goes.
#[derive(Debug, Serialize, Clone)]
pub struct Details {
    pub app: String,
    pub os: String,
    pub node: Option<String>,
}

#[derive(Serialize)]
struct Outgoing<'a> {
    kind: &'a str,
    text: &'a str,
    contact: &'a str,
    app: &'a str,
    os: &'a str,
    node: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    activity: &'a str,
    from: &'static str,
}

/// "macOS 26.0 (aarch64)", "Ubuntu 24.04.1 LTS (x86_64)".
fn system() -> String {
    let arch = std::env::consts::ARCH;
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|v| format!("macOS {}", v.trim()));
    #[cfg(not(target_os = "macos"))]
    let name = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
    });
    let s = format!("{} ({})", name.unwrap_or_else(|| std::env::consts::OS.to_string()), arch);
    clean(&s, false).chars().take(80).collect()
}

/// Invisible characters the relay refuses (distribution relay/src/feedback.rs `invisible`, security
/// review M2): they could hide text from a person while a program reading the report still sees it.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        | '\u{00AD}' | '\u{034F}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}' | '\u{180B}'..='\u{180F}'
        | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2065}' | '\u{206A}'..='\u{206F}' | '\u{2028}' | '\u{2029}'
        | '\u{3164}' | '\u{E000}'..='\u{F8FF}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{EFFFF}' | '\u{F0000}'..='\u{10FFFF}')
}

/// Text as the relay takes it: new lines as \n, no other control characters, nothing invisible (the
/// relay refuses those, so a pasted one would otherwise fail the whole report), and the home folder
/// as "~", so a pasted error doesn't carry the user's name (security review I8).
fn clean(s: &str, multiline: bool) -> String {
    let s = match crate::node::home().to_str().filter(|h| h.len() > 1) {
        Some(h) => s.replace(h, "~"),
        None => s.to_string(),
    };
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&c| !((c.is_control() && !(multiline && (c == '\n' || c == '\t'))) || invisible(c)))
        .collect()
}

#[tauri::command]
pub async fn feedback_details(mgr: State<'_, Arc<NodeManager>>) -> Result<Details, String> {
    let node = mgr.settings.lock().await.installed_tag.clone();
    Ok(Details { app: APP_VERSION.to_string(), os: system(), node })
}

/// "Include recent activity": what the report would carry, for the user to read first (`activity.rs`).
#[tauri::command]
pub async fn feedback_activity(mgr: State<'_, Arc<NodeManager>>) -> Result<String, String> {
    let datadir = mgr.settings.lock().await.datadir.clone();
    let node = (!datadir.is_empty()).then(|| std::path::PathBuf::from(datadir));
    Ok(crate::activity::recent(&mgr.app_dir, node.as_deref()))
}

/// An error the screens showed, for the recent activity. Error texts can carry amounts without a unit, so every
/// longer number goes too (`mask_numbers`).
#[tauri::command]
pub async fn activity_note(text: String) {
    let text: String = text.chars().take(2000).collect();
    crate::activity::note(&format!("shown: {}", crate::activity::mask_numbers(&text)));
}

/// Send a report to FreeBank. Returns its reference.
#[tauri::command]
pub async fn feedback_send(
    mgr: State<'_, Arc<NodeManager>>,
    kind: String,
    text: String,
    contact: String,
    with_details: bool,
    activity: Option<String>,
) -> Result<String, String> {
    if !matches!(kind.as_str(), "problem" | "idea" | "security") {
        return Err("Choose a problem, an idea or a security problem.".into());
    }
    let text = clean(text.trim(), true);
    let contact = clean(contact.trim(), false);
    if text.is_empty() {
        return Err("Write something first.".into());
    }
    // The characters, and the bytes: the server takes 64 KiB in all, with the activity, and the text
    // alone reaches 14,000 bytes before 8,000 characters in some scripts (code review 7).
    if text.chars().count() > MAX_TEXT || text.len() > MAX_TEXT_BYTES {
        return Err("That is longer than FreeBank takes; shorten it, please.".into());
    }
    if contact.chars().count() > MAX_CONTACT {
        return Err("The contact is too long.".into());
    }
    let d = if with_details { Some(feedback_details(mgr.clone()).await?) } else { None };
    let (app, os, node) = match &d {
        Some(d) => (d.app.as_str(), d.os.as_str(), d.node.as_deref().unwrap_or("")),
        None => ("", "", ""),
    };
    let url = std::env::var("FREEBANK_FEEDBACK_URL").unwrap_or_else(|_| FEEDBACK_URL.to_string());
    let activity: String = clean(activity.as_deref().unwrap_or(""), true).chars().take(crate::activity::MAX_ACTIVITY).collect();
    let body = Outgoing { kind: &kind, text: &text, contact: &contact, app, os, node, activity: &activity, from: "app" };
    post(&mgr.http, &url, &body).await.map_err(|e| {
        if activity.is_empty() { e } else { format!("{e} If it keeps failing, untick Include recent activity and send again.") }
    })
}

/// Post a report; the reference, or why not.
async fn post(http: &reqwest::Client, url: &str, body: &Outgoing<'_>) -> Result<String, String> {
    let res = http
        .post(url)
        .json(body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| "FreeBank's server couldn't be reached. Check your connection, or use GitHub.".to_string())?;
    let status = res.status();
    let reply: serde_json::Value = res.json().await.unwrap_or_default();
    if status.is_success() {
        Ok(reply["id"].as_str().unwrap_or("").chars().filter(|c| c.is_ascii_hexdigit()).take(16).collect())
    } else {
        let why: String = reply["error"].as_str().unwrap_or("").chars().take(200).collect();
        Err(if why.is_empty() {
            format!("FreeBank's server didn't take the report ({}).", status)
        } else {
            format!("FreeBank's server didn't take the report: {}.", why)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a running relay: `FREEBANK_FEEDBACK_URL=http://127.0.0.1:<port>/feedback cargo test --lib
    /// feedback -- --ignored` (the relay run with `--feedback-dir`).
    #[tokio::test]
    #[ignore]
    async fn a_running_relay_takes_the_report() {
        let url = std::env::var("FREEBANK_FEEDBACK_URL").expect("FREEBANK_FEEDBACK_URL");
        let http = reqwest::Client::new();
        let os = system();
        let body = Outgoing {
            kind: "problem",
            text: "Setup stopped at the download.\nTwice.",
            contact: "",
            app: APP_VERSION,
            os: &os,
            node: "v0.2.17",
            activity: "2026-10-02T07:01:02Z node: started\n2026-10-02T07:01:09Z phone link: online: wss://app.ecxfreebank.com/ws",
            from: "app",
        };
        let id = post(&http, &url, &body).await.unwrap();
        assert_eq!(id.len(), 8, "{}", id);
        let bad = Outgoing { kind: "spam", ..body };
        let err = post(&http, &url, &bad).await.unwrap_err();
        assert!(err.contains("kind is problem, idea or security"), "{}", err);
    }

    #[test]
    fn text_is_made_acceptable() {
        assert_eq!(clean("a\r\nb\rc\td\u{1b}[2J\u{202e}e", true), "a\nb\nc\td[2Je");
        assert_eq!(clean("ok\u{E0049} a\u{200B}b \u{2764}\u{FE0F}", true), "ok ab \u{2764}");
        let home = crate::node::home();
        assert_eq!(clean(&format!("Couldn't write {}/x.conf", home.display()), true), "Couldn't write ~/x.conf");
        assert_eq!(clean("me\n@x", false), "me@x");
        assert!(!system().is_empty());
    }
}
