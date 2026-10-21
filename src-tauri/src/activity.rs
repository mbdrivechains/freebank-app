//! Recent activity, for "Include recent activity" in a report (`feedback.rs`): a small log of what the app did (it
//! started, the node started and stopped, the phone link's state, failed or held sends, pairing, errors shown) in
//! `<app data>/logs/app.log`, kept to two files of 256 KiB. Lines are masked as they are written, and again when a
//! report asks for them, with chosen lines of the node's `debug.log`. Nothing leaves the computer unless the user
//! ticks the box, sees the text, and sends the report.
//!
//! What is masked: transaction ids, hashes and other hex, coin addresses of any prefix (base58check, bech32), IP
//! addresses (v4 and v6), amounts given with a unit, pairing links, the user name and passwords in URLs, long
//! tokens, and the home folder and the user's name. The node's log is read from a short list of message types (chain
//! progress, start and stop, errors, its caches): wallet lines, such as a sent transaction with its amounts and
//! scripts, never go. Times go to the minute. Successful sends are not noted.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// app.log turns into app.log.1 past this.
const MAX_FILE: u64 = 256 * 1024;
const MAX_LINE: usize = 300;
/// What a report takes: the app's last lines and the node's chosen ones, within the relay's 40,000 characters.
const APP_LINES: usize = 150;
const NODE_LINES: usize = 80;
/// The app's own eCash node's chosen lines and the enforcer's warnings and errors (v0.6.1).
const ECASH_LINES: usize = 40;
const ENFORCER_LINES: usize = 20;
pub const MAX_ACTIVITY: usize = 38_000;

/// The node's log lines a report may carry, by how their message starts. Anything else is left out and counted.
const NODE_KEEP: &[&str] = &[
    "UpdateTip:",
    "ConnectBlock:",
    "init message:",
    "Shutdown:",
    "ERROR",
    "Error",
    "Warning",
    "WARNING",
    "UpdateMainBlockCache:",
    "LoadMainBlockCache:",
    "DumpMainBlockCache:",
    "LoadBMMCache:",
    "DumpBMMCache:",
    "Leaving InitialBlockDownload",
    "FreeBank version",
    "Loaded best chain:",
    "connect() to",
];

/// The app's own eCash node's log lines a report may carry (v0.6.1), as `NODE_KEEP`: chain progress, start and stop,
/// errors. Its wallet lines never go. eCash 31 marks levels "[warning]", "[error]".
const ECASH_KEEP: &[&str] = &[
    "UpdateTip:",
    "init message:",
    "Shutdown",
    "ERROR",
    "Error",
    "[error]",
    "Warning",
    "WARNING",
    "[warning]",
    "Leaving InitialBlockDownload",
    "eCash version",
    "Bitcoin Core version",
    "Loaded best chain:",
];

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Set by Obliterate: from then on nothing is written, so the app's folder never comes back.
static STOPPED: AtomicBool = AtomicBool::new(false);
static WRITING: Mutex<()> = Mutex::new(());

/// Where the log goes: `<app dir>/logs/app.log`.
pub fn init(app_dir: &Path) {
    let _ = APP_DIR.set(app_dir.to_path_buf());
}

/// No more notes: Obliterate is removing the app's folder.
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
}

fn log_path(app_dir: &Path) -> PathBuf {
    app_dir.join("logs").join("app.log")
}

/// Note one event, masked, on one line, with the time (UTC).
pub fn note(event: &str) {
    let Some(dir) = APP_DIR.get() else { return };
    note_in(dir, event, std::time::SystemTime::now());
}

fn note_in(app_dir: &Path, event: &str, at: std::time::SystemTime) {
    if STOPPED.load(Ordering::SeqCst) || !app_dir.is_dir() {
        return;
    }
    let _w = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let logs = app_dir.join("logs");
    // Never the app's folder itself: only logs/ inside it.
    if !logs.is_dir() && std::fs::create_dir(&logs).is_err() {
        return;
    }
    let path = log_path(app_dir);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let flat = event.split_whitespace().collect::<Vec<_>>().join(" ");
    let secs = at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // One write, so lines from two app processes never interleave.
    let line = format!("{} {}\n", utc(secs), cut(&mask(&flat), MAX_LINE));
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    if let Ok(mut f) = o.open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// "2026-10-02T07:01:02Z".
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days to a civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// A line's leading time to the minute: "2026-10-02T07:01:02Z x" and "2026-10-02 07:01:02 x" lose their seconds.
fn to_minute(line: &str) -> String {
    let b = line.as_bytes();
    let stamp = b.len() >= 19
        && b[4] == b'-'
        && b[7] == b'-'
        && (b[10] == b'T' || b[10] == b' ')
        && b[13] == b':'
        && b[16] == b':'
        && b[..19].iter().enumerate().all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16) || c.is_ascii_digit());
    if stamp {
        // The enforcer's stamps carry a fraction of a second too ("20:09:31.287887Z"): it goes with the seconds.
        let frac = if b.get(19) == Some(&b'.') { 1 + b[20..].iter().take_while(|c| c.is_ascii_digit()).count() } else { 0 };
        format!("{}{}", &line[..16], &line[19 + frac..])
    } else {
        line.to_string()
    }
}

/// Mask what could identify the user or their coins (see the top of this file).
pub fn mask(s: &str) -> String {
    let home = crate::node::home();
    let mut s = match home.to_str().filter(|h| h.len() > 1) {
        Some(h) => s.replace(h, "~"),
        None => s.to_string(),
    };
    if let Some(user) = home.file_name().and_then(|n| n.to_str()).filter(|u| u.len() >= 3) {
        s = replace_word(&s, user, "<user>");
    }
    s = mask_url_users(&s);
    s = mask_pair_links(&s);
    s = mask_ipv6(&s);
    s = mask_amounts(&s);
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.') {
            word.push(c);
        } else {
            out.push_str(&mask_word(&word));
            word.clear();
            out.push(c);
        }
    }
    out.push_str(&mask_word(&word));
    out
}

/// Error texts may carry amounts without a unit ("500000 of the 1000000 units", "1500000 > 1000000"): for those,
/// every number of four or more digits goes too.
pub fn mask_numbers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let digits = run.chars().filter(|c| c.is_ascii_digit()).count();
        out.push_str(if digits >= 4 { "<n>" } else { run });
        run.clear();
    };
    for c in s.chars() {
        if c.is_ascii_digit() || (c == '.' && !run.is_empty()) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `word` replaced where it stands alone.
fn replace_word(s: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(word) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + word.len()..].chars().next();
        out.push_str(&rest[..i]);
        if before.is_none_or(|c| !is_word_char(c)) && after.is_none_or(|c| !is_word_char(c)) {
            out.push_str(with);
        } else {
            out.push_str(word);
        }
        rest = &rest[i + word.len()..];
    }
    out.push_str(rest);
    out
}

/// "scheme://user:password@host" keeps only "scheme://<user>@host".
fn mask_url_users(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let start = i + 3;
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find(|c: char| c == '/' || c.is_whitespace()).unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) => {
                out.push_str("<user>");
                rest = &tail[at..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

/// A pairing link's secret: everything after #pair= up to the end of the token.
fn mask_pair_links(s: &str) -> String {
    let mut s = s.to_string();
    let mut from = 0;
    while let Some(at) = s[from..].find("#pair=").map(|i| i + from + "#pair=".len()) {
        let end = s[at..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).map_or(s.len(), |i| at + i);
        s.replace_range(at..end, "<link>");
        from = at + "<link>".len();
    }
    s
}

/// IPv6 addresses (and MAC addresses): runs of hex digits, colons and dots with "::" or three or more colons.
/// Times ("07:01:02") have two colons and stay.
fn mask_ipv6(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let colons = run.matches(':').count();
        let hex = run.chars().filter(|c| c.is_ascii_hexdigit()).count();
        if (run.contains("::") || colons >= 3) && hex >= 1 {
            out.push_str("<ip>");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    let mut prev: Option<char> = None;
    for c in s.chars() {
        // A run starts only at a word boundary, so "deadbeef" in "a_deadbeef::" isn't half-taken.
        let starts = run.is_empty() && prev.is_some_and(|p| p.is_ascii_alphanumeric() || p == '_');
        if (c.is_ascii_hexdigit() || c == ':' || c == '.') && !starts {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
        prev = Some(c);
    }
    flush(&mut run, &mut out);
    out
}

const UNITS: &[&str] = &["sat", "sats", "satoshi", "satoshis", "unit", "units", "ecx", "secx", "btc", "coin", "coins"];

/// Amounts: a number followed by a unit, "nValue=…" and "Fee:…".
fn mask_amounts(s: &str) -> String {
    let parts: Vec<&str> = s.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        let unit = parts.get(i + 1).is_some_and(|n| {
            let n = n.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_lowercase();
            UNITS.contains(&n.as_str())
        });
        if let Some(i) = ["nValue=", "Fee:"].iter().find_map(|k| p.find(k).map(|i| i + k.len())) {
            out.push(format!("{}{}", &p[..i], mask_number(&p[i..])));
        } else if unit && number_core(p).is_some() {
            out.push(mask_number(p));
        } else {
            out.push(p.to_string());
        }
    }
    out.join(" ")
}

/// The number in a token with punctuation around it ("(100000000", "12.5,"): its byte range.
fn number_core(p: &str) -> Option<(usize, usize)> {
    let start = p.find(|c: char| c.is_ascii_digit())?;
    let end = p[start..].find(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(p.len(), |i| start + i);
    let core = &p[start..end];
    let lead_ok = p[..start].chars().all(|c| matches!(c, '(' | '[' | '+' | '-'));
    let trail_ok = p[end..].chars().all(|c| matches!(c, ',' | ';' | ')' | ']' | '.' | ':' | '!'));
    (lead_ok && trail_ok && core.matches('.').count() <= 1).then_some((start, end))
}

fn mask_number(p: &str) -> String {
    match number_core(p) {
        Some((a, b)) => format!("{}<amount>{}", &p[..a], &p[b..]),
        None => p.to_string(),
    }
}

/// A run of word characters: split at '/' and '=' (paths, key=value), then each piece is an IP address, or is split
/// at its dots and masked part by part.
fn mask_word(w: &str) -> String {
    let mut out = String::new();
    let mut piece = String::new();
    for c in w.chars() {
        if c == '/' || c == '=' {
            out.push_str(&mask_piece(&piece));
            piece.clear();
            out.push(c);
        } else {
            piece.push(c);
        }
    }
    out.push_str(&mask_piece(&piece));
    out
}

fn mask_piece(p: &str) -> String {
    let core = p.trim_end_matches('.');
    if is_ipv4(core) {
        return format!("<ip>{}", &p[core.len()..]);
    }
    p.split('.').map(mask_token).collect::<Vec<_>>().join(".")
}

fn mask_token(t: &str) -> String {
    let n = t.len();
    let hex = t.bytes().all(|b| b.is_ascii_hexdigit());
    if n == 64 && hex {
        return "<hash>".into();
    }
    // Shortened ids and scripts ("c2004004e2", "76a914d42f…"): hex with both digits and letters.
    if n >= 8 && hex && t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().any(|b| b.is_ascii_alphabetic()) {
        return "<hex>".into();
    }
    // Coin addresses of any prefix: base58 with a valid checksum, or the old prefixes even without one.
    let base58 = |b: u8| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l');
    if (25..=40).contains(&n)
        && t.bytes().all(base58)
        && (bitcoin::base58::decode_check(t).is_ok() || matches!(t.as_bytes()[0], b'1' | b'3' | b'X'))
    {
        return "<address>".into();
    }
    let lower = t.to_ascii_lowercase();
    if ["btx1", "bc1", "tb1", "bcrt1", "ecash1", "fbk1"].iter().any(|p| lower.starts_with(p) && n >= p.len() + 6) {
        return "<address>".into();
    }
    if n >= 40 {
        return "<id>".into();
    }
    t.into()
}

fn is_ipv4(w: &str) -> bool {
    let parts: Vec<&str> = w.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The last `n` lines of a file, reading at most its last `max_bytes`.
fn tail(path: &Path, n: usize, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else { return vec![] };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return vec![];
    }
    let mut buf = Vec::new();
    let _ = f.take(max_bytes).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0); // cut mid-line
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| l.to_string()).collect()
}

/// A node's lines a report may carry (`keep`: `NODE_KEEP`, `ECASH_KEEP`), masked; the others are counted as
/// "(n lines left out)".
fn node_lines(raw: &[String], keep: &[&str]) -> Vec<String> {
    chosen(raw, |line| node_message(line).is_some_and(|m| keep.iter().any(|k| m.starts_with(k))))
}

/// A node's log line after its time stamp: "2026-10-21 05:45:20 " (FreeBank's node, older eCash builds) or
/// "2026-10-21T05:45:20Z " (eCash 31). None for a line without one.
fn node_message(line: &str) -> Option<&str> {
    let b = line.as_bytes();
    if b.len() > 21 && b[4] == b'-' && b[7] == b'-' && b[13] == b':' && b[16] == b':' {
        if b[10] == b' ' && b[19] == b' ' {
            return Some(&line[20..]);
        }
        if b[10] == b'T' && b[19] == b'Z' && b[20] == b' ' {
            return Some(&line[21..]);
        }
    }
    None
}

/// The enforcer's warnings and errors (its lines are "<time>  WARN <target>: …"), masked, without the source file each
/// names; the others are counted.
fn enforcer_lines(raw: &[String]) -> Vec<String> {
    let raw: Vec<String> = raw.iter().map(|l| without_source(l)).collect();
    chosen(&raw, |line| line.split_whitespace().take(3).any(|w| matches!(w, "ERROR" | "WARN")))
}

/// "… WARN zeromq::reconnect: /usr/local/cargo/registry/…/reconnect.rs:180: Reconnection attempt 1" → "… WARN
/// zeromq::reconnect: Reconnection attempt 1": the enforcer's lines name the Rust file and line they came from.
fn without_source(line: &str) -> String {
    if let Some(i) = line.find(".rs:") {
        let start = line[..i].rfind(' ').map_or(0, |s| s + 1);
        let rest = &line[i + 4..];
        let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 && rest[digits..].starts_with(": ") {
            return format!("{}{}", &line[..start], &rest[digits + 2..]);
        }
    }
    line.to_string()
}

/// The lines `keep` takes, masked and to the minute, with the runs it leaves out counted.
fn chosen(raw: &[String], keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut left_out = 0;
    for line in raw {
        if keep(line) {
            if left_out > 0 {
                out.push(format!("({left_out} lines left out)"));
                left_out = 0;
            }
            out.push(cut(&to_minute(&mask(line)), MAX_LINE));
        } else {
            left_out += 1;
        }
    }
    if left_out > 0 {
        out.push(format!("({left_out} lines left out)"));
    }
    out
}

/// What "Include recent activity" sends: the app's last lines and the node's chosen ones, masked, at most
/// `MAX_ACTIVITY` characters (the oldest lines go first). With the app's own eCash node (`stack`: its debug.log, and
/// the enforcer's newest log), theirs too (v0.6.1): on a Mac's first easynet run its eCash node stopped and nobody
/// could tell why.
pub fn recent(app_dir: &Path, node_datadir: Option<&Path>, stack: Option<(&Path, Option<&Path>)>) -> String {
    let path = log_path(app_dir);
    let mut app = tail(&path.with_extension("log.1"), APP_LINES, 64 * 1024);
    app.extend(tail(&path, APP_LINES, 64 * 1024));
    // Masked again: lines written before a masking rule existed get it too.
    let mut app: Vec<String> =
        app[app.len().saturating_sub(APP_LINES)..].iter().map(|l| cut(&to_minute(&mask(l)), MAX_LINE)).collect();
    let raw = node_datadir.map(|d| tail(&d.join("debug.log"), 600, 96 * 1024)).unwrap_or_default();
    let mut node = node_lines(&raw, NODE_KEEP);
    let skip = node.len().saturating_sub(NODE_LINES);
    node.drain(..skip);
    let last = |mut v: Vec<String>, n: usize| {
        let skip = v.len().saturating_sub(n);
        v.drain(..skip);
        v
    };
    let mut ecash = stack.map(|(log, _)| last(node_lines(&tail(log, 600, 96 * 1024), ECASH_KEEP), ECASH_LINES));
    let mut enforcer = stack.map(|(_, log)| {
        last(log.map(|l| enforcer_lines(&tail(l, 600, 96 * 1024))).unwrap_or_default(), ENFORCER_LINES)
    });
    let or_none = |v: &[String]| if v.is_empty() { "(none)".to_string() } else { v.join("\n") };
    loop {
        let mut text = format!(
            "== FreeBank app ==\n{}\n== FreeBank node (debug.log, chosen lines) ==\n{}\n",
            app.join("\n"),
            or_none(&node)
        );
        if let (Some(e), Some(f)) = (&ecash, &enforcer) {
            text.push_str(&format!(
                "== The app's eCash node (debug.log, chosen lines) ==\n{}\n== Its enforcer (warnings and errors) ==\n{}\n",
                or_none(e),
                or_none(f)
            ));
        }
        let n = |v: &Option<Vec<String>>| v.as_ref().map_or(0, Vec::len);
        if text.chars().count() <= MAX_ACTIVITY || app.len() + node.len() + n(&ecash) + n(&enforcer) == 0 {
            return text;
        }
        // The oldest line of the longest part goes.
        let longest = [app.len(), node.len(), n(&ecash), n(&enforcer)].into_iter().enumerate().max_by_key(|(_, l)| *l).map_or(0, |(i, _)| i);
        match longest {
            0 => drop(app.remove(0)),
            1 => drop(node.remove(0)),
            2 => drop(ecash.as_mut().map(|v| v.remove(0))),
            _ => drop(enforcer.as_mut().map(|v| v.remove(0))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_what_could_identify_the_user() {
        let h = "a".repeat(64);
        assert_eq!(mask(&format!("UpdateTip: new best={h} height=307")), "UpdateTip: new best=<hash> height=307");
        assert_eq!(mask("to XkPvjfFq8Hj9wy9Wq3pRr1sBdXJxZ5nT2n."), "to <address>.");
        assert_eq!(mask("btx1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080 paid"), "<address> paid");
        assert_eq!(mask("addr=163.47.9.132:8455 peer=3"), "addr=<ip>:8455 peer=3");
        assert_eq!(mask("https://app.ecxfreebank.com/#pair=eyJ2IjoxLCJyZWxheSI6"), "https://app.ecxfreebank.com/#pair=<link>");
        assert_eq!(mask(&format!("cookie __cookie__:{}", "Zm9v".repeat(12))), "cookie __cookie__:<id>");
        // IPv6, with brackets, ports, scopes and mapped IPv4; MAC addresses; times stay.
        assert_eq!(
            mask("connect() to [2002:c0a8:15b:e472:3720:50ce:93e8:1736]:8455 failed at 07:01:02"),
            "connect() to [<ip>]:8455 failed at 07:01:02"
        );
        // "::" (any address) names nobody and stays; C++ names ("CBlockPolicyEstimator::Read") stay too.
        assert_eq!(mask("bound [::]:8455, fe80::1%wlp2s0 and ::ffff:1.2.3.4"), "bound [::]:8455, <ip>%wlp2s0 and <ip>");
        assert_eq!(mask("ERROR: CBlockPolicyEstimator::Read(): up-version"), "ERROR: CBlockPolicyEstimator::Read(): up-version");
        assert_eq!(mask("ether 3c:22:fb:01:9a:7e"), "ether <ip>");
        // Short ids and scripts; any address prefix with a valid checksum (a regtest/testnet one here).
        assert_eq!(
            mask("CTransaction(hash=c2004004e2, ver=3) scriptPubKey=76a914d42fe63db24429922b882ddd"),
            "CTransaction(hash=<hex>, ver=3) scriptPubKey=<hex>"
        );
        assert_eq!(mask("to mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn now"), "to <address> now");
        // Amounts with a unit, nValue= and Fee:.
        assert_eq!(
            mask("CTxOut(nValue=12.50000000, x) Fee:22600 face (100000000 sats) Residual 12345 sats, 0.5 sECX"),
            "CTxOut(nValue=<amount>, x) Fee:<amount> face (<amount> sats) Residual <amount> sats, <amount> sECX"
        );
        assert_eq!(mask_numbers("absurdly-high-fee, 1500000 > 1000000 at height 307"), "absurdly-high-fee, <n> > <n> at height 307");
        // Passwords in URLs.
        assert_eq!(mask("phone link: online: wss://bob:hunter2@relay.example/ws"), "phone link: online: wss://<user>@relay.example/ws");
        // What helps find a problem stays.
        let plain = "phone link: retrying (Connection refused). Trying again in 30 s. node v0.2.17 at block 307";
        assert_eq!(mask(plain), plain);
        let home = crate::node::home();
        assert_eq!(mask(&format!("{}/x/debug.log", home.display())), "~/x/debug.log");
        if let Some(user) = home.file_name().and_then(|n| n.to_str()).filter(|u| u.len() >= 3) {
            assert_eq!(mask(&format!("/media/{user}/disk")), "/media/<user>/disk");
        }
    }

    #[test]
    fn the_node_log_gives_only_chosen_lines() {
        let tx = "2026-10-02 07:01:02 CommitTransaction:\nCTransaction(hash=c2004004e2, ver=3, vin.size=1, vout.size=2, nLockTime=0)\n    CTxIn(COutPoint(d8748400bd, 3), scriptSig=483045022100ef35c2ea48d3, nSequence=4294967293)\n    CTxOut(nValue=12.50000000, scriptPubKey=76a914d42fe63db24429922b882ddd)";
        let raw: Vec<String> = format!(
            "2026-10-02 07:01:00 UpdateTip: new best={} height=307 version=0x20000000 tx=678826\n\
             2026-10-02 07:01:01 Fee Calculation: Fee:22600 Bytes:226 Needed:22600 Tgt:6\n{tx}\n\
             2026-10-02 07:01:03 AddToWallet {}  new\n\
             2026-10-02 07:01:04 connect() to [2002:c0a8:15b:e472:3720:50ce:93e8:1736]:8455 failed: Network is unreachable (101)\n\
             2026-10-02 07:01:05 Using enforcer: alice-desktop.example.ts.net:50051",
            "b".repeat(64),
            "c".repeat(64)
        )
        .lines()
        .map(String::from)
        .collect();
        let got = node_lines(&raw, NODE_KEEP);
        assert_eq!(
            got,
            vec![
                "2026-10-02 07:01 UpdateTip: new best=<hash> height=307 version=0x20000000 tx=678826".to_string(),
                "(6 lines left out)".into(),
                "2026-10-02 07:01 connect() to [<ip>]:8455 failed: Network is unreachable (101)".into(),
                "(1 lines left out)".into(),
            ]
        );
    }

    #[test]
    fn dates_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(to_minute("2026-10-02T07:01:02Z node: started"), "2026-10-02T07:01Z node: started");
        assert_eq!(to_minute("2026-10-02 07:01:02 UpdateTip: x"), "2026-10-02 07:01 UpdateTip: x");
        assert_eq!(to_minute("2026-10-21T20:09:31.287887Z  WARN x: y"), "2026-10-21T20:09Z  WARN x: y");
        assert_eq!(to_minute("(3 lines left out)"), "(3 lines left out)");
    }

    #[test]
    fn notes_rotate_and_reports_take_both_logs() {
        let dir = std::env::temp_dir().join(format!("fb-activity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No app folder: nothing is written, and it isn't made.
        note_in(&dir, "started", std::time::SystemTime::now());
        assert!(!dir.exists());
        std::fs::create_dir_all(dir.join("node")).unwrap();
        for i in 0..4000 {
            let words = "the phone link went online and the node kept up with the explorer tip";
            note_in(&dir, &format!("event {i} sent {} {words} {words}", "f".repeat(64)), std::time::SystemTime::now());
        }
        assert!(log_path(&dir).with_extension("log.1").exists(), "rotated");
        assert!(std::fs::metadata(log_path(&dir)).unwrap().len() <= MAX_FILE + 400);
        std::fs::write(dir.join("node/debug.log"), "boot\n2026-10-02 07:09:01 UpdateTip: new best=".to_string() + &"b".repeat(64) + " height=9\n").unwrap();
        let r = recent(&dir, Some(&dir.join("node")), None);
        assert!(r.starts_with("== FreeBank app ==\n"));
        assert!(r.contains(" event 3999 sent <hash> the phone link"));
        assert!(r.contains("2026-10-02 07:09 UpdateTip: new best=<hash> height=9"));
        assert!(r.contains("(1 lines left out)"), "the line without a time");
        assert!(!r.contains(&"f".repeat(64)) && !r.contains(&"b".repeat(64)));
        assert_eq!(r.lines().filter(|l| l.contains(" event ")).count(), APP_LINES);
        assert!(r.chars().count() <= MAX_ACTIVITY);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_report_carries_the_apps_own_ecash_node_and_enforcer() {
        let dir = std::env::temp_dir().join(format!("fb-activity-stack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let node = dir.join("debug.log");
        let enf = dir.join("enforcer.log");
        std::fs::write(
            &node,
            "2026-10-21T06:40:01Z eCash version v31.1.0 (release build)\n\
             2026-10-21T06:45:40Z UpdateTip: new best=".to_string() + &"c".repeat(64) + " height=2905\n\
             2026-10-21T06:45:41Z AddToWallet " + &"d".repeat(64) + " new\n\
             2026-10-21T06:45:49Z [error] Disk full\n2026-10-21T06:45:49Z Shutdown in progress...\n",
        )
        .unwrap();
        std::fs::write(
            &enf,
            "2026-10-21T06:45:20.1Z  INFO bip300301_enforcer: synced to 2905\n\
             2026-10-21T06:45:49.9Z ERROR bip300301_enforcer: lost the node at 100.64.0.7:28532\n\
             2026-10-21T06:45:50.287887Z  WARN zeromq::reconnect: /usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/zeromq-0.6.0/src/reconnect.rs:180: Reconnection attempt 1\n",
        )
        .unwrap();
        let r = recent(&dir, None, Some((&node, Some(&enf))));
        let at = r.find("== The app's eCash node (debug.log, chosen lines) ==").expect("its section");
        let mine = &r[at..];
        assert!(mine.contains("eCash version v31.1.0"), "{mine}");
        assert!(mine.contains("UpdateTip: new best=<hash> height=2905"));
        assert!(mine.contains("[error] Disk full") && mine.contains("Shutdown in progress"));
        assert!(!mine.contains("AddToWallet") && mine.contains("(1 lines left out)"), "its wallet lines never go");
        assert!(mine.contains("== Its enforcer (warnings and errors) ==\n(1 lines left out)\n2026-10-21T06:45Z ERROR"), "{mine}");
        assert!(mine.contains("\n2026-10-21T06:45Z  WARN zeromq::reconnect: Reconnection attempt 1"), "{mine}");
        assert!(mine.contains("ERROR bip300301_enforcer: lost the node at <ip>"), "{mine}");
        assert!(!r.contains("100.64.0.7"));
        // Without the app's own stack, no such sections.
        assert!(!recent(&dir, None, None).contains("eCash node"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
