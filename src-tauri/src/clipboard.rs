//! "Copy words": people copy their words whatever they are told, so the app makes it safe to (v0.2.0; hardened in
//! v0.4.10 with the Truthcoin app's reviewed version, which was ported from this one: `truthcoinapp/reviews/
//! SECURITY-REVIEW-f9cc742-copy-words.md` and the re-review and checks after it, in the project notes). The clipboard
//! is written from Rust, not the page, so the page never gets clipboard access.

use std::time::Duration;

// The words go on the clipboard in two forms at once, so each app takes the one it
// understands: HTML, a numbered table in the words' order (1 2 3 on the first row), for Notes, Pages or mail; and plain
// text, the words space-separated, for a password manager.
// - Linux (arboard, X11): marked `x-kde-passwordManagerHint: secret` for clipboard managers to leave out of their
//   history.
// - macOS (AppKit directly, because arboard can't set the option): one pasteboard item marked concealed and transient
//   (nspasteboard.org), written "current host only", so Universal Clipboard doesn't pass it to the user's other Apple
//   devices (review L1).
// After 60 seconds, or when the app ends (a quit, a signal, a panic), the clipboard is cleared if it still holds the
// app's copy. On a Mac that is told by the pasteboard's change count, so the app never reads another app's copy (N7).
// Only 24 recovery words are copied (`copy_words` checks them), so this copies nothing else.
// Nothing here is logged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use zeroize::Zeroizing;

const CLEAR_AFTER: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 60 });
/// The newest copy that reached the clipboard. Only its clearer acts, so a second copy gets its own full minute.
static COPIES: AtomicU64 = AtomicU64::new(0);
/// The copy whose minute isn't up.
static PENDING: Mutex<Option<Copied>> = Mutex::new(None);
/// Held from a copy's write until it is the pending one, so two copies at once can't leave the older one pending
/// (re-review R5).
static WRITING: Mutex<()> = Mutex::new(());
/// The longest the app waits for a clear at quit or after a panic (the clipboard's owner may never answer).
const LAST_CLEAR: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct Copied {
    gen: u64,
    plain: Zeroizing<String>,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    html: Zeroizing<String>,
    /// The pasteboard's change count just after the app wrote (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    mark: isize,
}

/// Whether the clipboard still holds the app's copy: exactly (both forms, or on a Mac nothing written since), or only
/// its plain words (Linux: another app, a password manager or a clipboard manager, copied the same words).
/// What a clear after a paste of words could tell (re-review R1; final check, Note 3).
#[derive(serde::Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Cleared {
    /// Checked: the words are gone.
    Gone,
    /// Sent, but this desktop gives no way to check (Wayland).
    Unchecked,
    /// Not sent, or the words are back (a clipboard manager), or the check didn't finish.
    NotGone,
}

#[derive(PartialEq)]
enum Holds {
    Not,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Plain,
    Exact,
}

#[cfg(not(target_os = "macos"))]
mod os {
    use super::{Copied, Holds};
    use zeroize::Zeroizing;

    /// Keeps the copy served: on X11 the app serves the clipboard only while a clipboard object lives.
    pub struct Held(arboard::Clipboard);

    pub fn write(html: &str, plain: &str) -> Result<(Held, isize), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| format!("FreeBank couldn't reach the clipboard ({e})"))?;
        #[cfg(target_os = "linux")]
        use arboard::SetExtLinux;
        let set = cb.set();
        #[cfg(target_os = "linux")]
        let set = set.exclude_from_history();
        set.html(html, Some(plain)).map_err(|e| format!("FreeBank couldn't copy the words ({e})"))?;
        Ok((Held(cb), 0))
    }

    /// Reads the HTML, and the text too if `plain_too` (each read can wait seconds for an owner that doesn't answer).
    pub fn holds(held: Option<&mut Held>, c: &Copied, plain_too: bool) -> Holds {
        let mut fresh;
        let cb = match held {
            Some(h) => &mut h.0,
            None => match arboard::Clipboard::new() {
                Ok(cb) => {
                    fresh = cb;
                    &mut fresh
                }
                Err(_) => return Holds::Not,
            },
        };
        if cb.get().html().is_ok_and(|h| Zeroizing::new(h).as_str() == c.html.as_str()) {
            Holds::Exact
        } else if plain_too && cb.get_text().is_ok_and(|t| Zeroizing::new(t).as_str() == c.plain.as_str()) {
            Holds::Plain
        } else {
            Holds::Not
        }
    }

    /// Clear the clipboard, whoever holds it (X11 lets any client give up the selection's owner). Whether the clear
    /// was sent.
    pub fn clear(held: Option<&mut Held>, _c: Option<&Copied>) -> bool {
        match held {
            Some(h) => h.0.clear().is_ok(),
            None => arboard::Clipboard::new().and_then(|mut cb| cb.clear()).is_ok(),
        }
    }

    /// After a paste of words: clear the clipboard, the selection for middle-click paste (PRIMARY, which holds the
    /// words if they were selected to be copied; check C3) and any X11 cut buffer that holds words (xterm puts its
    /// selections there too; final check, Note 1). Whether the clears were sent.
    pub fn clear_both() -> bool {
        use arboard::{ClearExtLinux, LinuxClipboardKind};
        let Ok(mut cb) = arboard::Clipboard::new() else { return false };
        let clip = cb.clear().is_ok();
        let primary = cb.clear_with().clipboard(LinuxClipboardKind::Primary).is_ok();
        let cut = cut_buffers_with_words(true).is_some();
        clip && primary && cut
    }

    /// Whether any of the root window's cut buffers (CUT_BUFFER0-7) holds words, deleting those that do if `delete`.
    /// None if the X server can't be reached.
    pub(super) fn cut_buffers_with_words(delete: bool) -> Option<bool> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let mut found = false;
        for atom in 9u32..=16 {
            let Some(reply) = conn.get_property(false, root, atom, AtomEnum::ANY, 0, 1 << 16).ok().and_then(|c| c.reply().ok())
            else {
                continue;
            };
            let value = Zeroizing::new(reply.value);
            if super::holds_words(&Zeroizing::new(String::from_utf8_lossy(&value).into_owned())) {
                found = true;
                if delete {
                    let _ = conn.delete_property(root, atom);
                }
            }
        }
        conn.flush().ok()?;
        Some(found)
    }

    /// After `clear_both`: whether the app can say the words are gone (re-review R1, check C1-C2). Not on a Wayland
    /// desktop, where the clear and the check both go through XWayland and can't see a Wayland app's copy. Otherwise
    /// the clipboard is read twice, at a quarter second and at a second (a clipboard manager that "prevents an empty
    /// clipboard" puts its last entry back): it must be empty, and the selection must hold no words. A read that
    /// doesn't finish within a second counts as not gone.
    pub fn confirm_gone() -> super::Cleared {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"));
        if wayland {
            return super::Cleared::Unchecked;
        }
        let gone = [250, 750].into_iter().all(|ms| {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            super::within(super::LAST_CLEAR, read_both)
                .is_some_and(|(clip_empty, other_words)| clip_empty && !other_words)
        });
        if gone {
            super::Cleared::Gone
        } else {
            super::Cleared::NotGone
        }
    }

    /// (the clipboard is empty, the selection or a cut buffer holds words)
    fn read_both() -> (bool, bool) {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let Ok(mut cb) = arboard::Clipboard::new() else { return (false, true) };
        let clip_empty = match cb.get_text() {
            Ok(t) => Zeroizing::new(t).trim().is_empty(),
            Err(arboard::Error::ContentNotAvailable) => true,
            Err(_) => false,
        };
        let primary_words = cb
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .is_ok_and(|t| super::holds_words(&Zeroizing::new(t)));
        (clip_empty, primary_words || cut_buffers_with_words(false) != Some(false))
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{Copied, Holds};
    use objc2::rc::autoreleasepool;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypeHTML, NSPasteboardTypeString,
    };
    use objc2_foundation::{ns_string, NSArray, NSString};
    use zeroize::Zeroizing;

    /// The pasteboard keeps the copy by itself.
    pub struct Held;

    /// One item: the markers, the HTML (wrapped as arboard does, so it reads as UTF-8) and the plain words, written
    /// at once, for this Mac only. Returns the change count after the write.
    pub fn write(html: &str, plain: &str) -> Result<(Held, isize), String> {
        autoreleasepool(|_| {
            const HEAD: &str =
                r#"<html><head><meta http-equiv="content-type" content="text/html; charset=utf-8"></head><body>"#;
            let mut doc = Zeroizing::new(String::with_capacity(HEAD.len() + html.len() + 16));
            doc.push_str(HEAD);
            doc.push_str(html);
            doc.push_str("</body></html>");
            let item = NSPasteboardItem::new();
            let ok = item.setString_forType(ns_string!(""), ns_string!("org.nspasteboard.ConcealedType"))
                && item.setString_forType(ns_string!(""), ns_string!("org.nspasteboard.TransientType"))
                && unsafe { item.setString_forType(&NSString::from_str(&doc), NSPasteboardTypeHTML) }
                && unsafe { item.setString_forType(&NSString::from_str(plain), NSPasteboardTypeString) };
            if !ok {
                return Err("FreeBank couldn't copy the words".to_string());
            }
            let pb = NSPasteboard::generalPasteboard();
            pb.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
            let items = NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
            if !pb.writeObjects(&items) {
                return Err("FreeBank couldn't copy the words".to_string());
            }
            Ok((Held, pb.changeCount()))
        })
    }

    /// Nothing has been written since the app's copy. Reads no contents, so no paste alert.
    pub fn holds(_held: Option<&mut Held>, c: &Copied, _plain_too: bool) -> Holds {
        if autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount()) == c.mark {
            Holds::Exact
        } else {
            Holds::Not
        }
    }

    /// With `c`, clear only if the app's copy is still there; without, clear (right after the words were pasted).
    pub fn clear(_held: Option<&mut Held>, c: Option<&Copied>) -> bool {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            if c.is_none_or(|c| pb.changeCount() == c.mark) {
                pb.clearContents();
            }
        });
        true
    }

    /// After a paste of words: the pasteboard (a Mac has no selection for middle-click paste).
    pub fn clear_both() -> bool {
        clear(None, None)
    }

    /// `clearContents` empties the pasteboard itself; reading it back would raise a paste alert (review N7).
    pub fn confirm_gone() -> super::Cleared {
        super::Cleared::Gone
    }
}

/// Clear the clipboard now if it still holds the app's copy: at quit, on a signal, and after a panic (a `try_lock`
/// there, and the caller waits at most a second).
fn clear_pending(wait: bool) {
    let mut g = if wait {
        PENDING.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        match PENDING.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        }
    };
    let Some(c) = g.take() else { return };
    drop(g);
    if os::holds(None, &c, true) != Holds::Not {
        os::clear(None, Some(&c));
    }
}

/// Run `f` on a thread of its own and wait for it at most `limit`: the clipboard's owner may never answer (the app's
/// own window, if the main thread is the one waiting; re-review R2). None if it didn't finish in time.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(limit).ok()
}

fn within_a_second(f: impl FnOnce() + Send + 'static) {
    within(LAST_CLEAR, f);
}

/// At quit (lib.rs, `RunEvent::Exit`, and a second signal).
pub fn clear_at_exit() {
    within_a_second(|| clear_pending(true));
}

/// From the panic hook (lib.rs): the release build aborts after it, so this is the last chance. A `try_lock`, in case
/// the panic came while the lock was held.
pub fn clear_after_panic() {
    within_a_second(|| clear_pending(false));
}

/// Whether text may hold recovery words: 12 or more runs of two or more letters, whatever separates them (`holdsWords`
/// in `src/lib/walletSeed.ts`). Broad on purpose: it decides only that the clipboard is cleared, or not said to be clear.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn holds_words(text: &str) -> bool {
    text.split(|c: char| !c.is_alphabetic()).filter(|t| t.chars().count() >= 2).count() >= 12
}

/// The two forms: the HTML table and the plain words. The words are checked as recovery words before (`copy_words`),
/// and escaped here all the same.
fn forms(words: &[&str]) -> (Zeroizing<String>, Zeroizing<String>) {
    use std::fmt::Write;
    let len: usize = words.iter().map(|w| w.len() * 6 + 20).sum();
    let mut html = Zeroizing::new(String::with_capacity(len + 64));
    let mut plain = Zeroizing::new(String::with_capacity(len));
    html.push_str("<table>");
    for (r, row) in words.chunks(3).enumerate() {
        html.push_str("<tr>");
        for (c, w) in row.iter().enumerate() {
            let _ = write!(html, "<td>{}. ", r * 3 + c + 1);
            escape_into(&mut html, w);
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            plain.push(' ');
        }
        plain.push_str(w);
    }
    (html, plain)
}

fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Copy `words`, then clear the clipboard after `CLEAR_AFTER` if it still holds them. Answers once the words are on
/// the clipboard, or not.
fn start_copy(words: &str) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
    let list: Vec<&str> = words.split_whitespace().collect();
    let (html, plain) = forms(&list);
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let (mut held, mark) = match os::write(&html, &plain) {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        // Counted only once on the clipboard: a copy that failed doesn't cancel an earlier one's clear (review N3).
        let gen = COPIES.fetch_add(1, Ordering::SeqCst) + 1;
        *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Copied { gen, plain, html, mark });
        drop(writing);
        let _ = tx.send(Ok(()));
        std::thread::sleep(CLEAR_AFTER);
        let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_none_or(|c| c.gen != gen) {
            return;
        }
        let c = g.take().expect("checked above");
        drop(g);
        if os::holds(Some(&mut held), &c, true) != Holds::Not {
            os::clear(Some(&mut held), Some(&c));
        }
    });
    rx
}

/// Put the recovery words on the clipboard, and clear it after a minute if it still holds them. Only 24 known words are
/// taken (the ones the screen shows, new or revealed), so this is no general clipboard writer.
#[tauri::command]
pub async fn copy_words(words: Vec<String>) -> Result<(), String> {
    let words = Zeroizing::new(words);
    let english = bip39::Language::English;
    if words.len() != crate::seed::WORD_COUNT || words.iter().any(|w| english.find_word(w).is_none()) {
        return Err("Only the recovery words can be copied here.".into());
    }
    let joined = Zeroizing::new(words.join(" "));
    start_copy(&joined).await.map_err(|_| "FreeBank couldn't copy the words.".to_string())?
}

/// Whether the clipboard holds the app's own copy of the words, untouched: the check step refuses it, since a copy the
/// app clears in a minute is no backup (review L2). A copy the user made from their notes or password manager is not
/// the app's own, even with the same words.
/// Off the main thread, and the clipboard read outside the lock (re-review R2).
#[tauri::command]
pub async fn words_own_copy() -> bool {
    tauri::async_runtime::spawn_blocking(own_copy_now).await.unwrap_or(false)
}

fn own_copy_now() -> bool {
    let copied = PENDING.lock().unwrap_or_else(|e| e.into_inner()).clone();
    copied.is_some_and(|c| os::holds(None, &c, false) == Holds::Exact)
}

/// Called right after recovery words were pasted from the clipboard into the app (the check step, a restore): the
/// clipboard is cleared, since the copy there was made by another app and has no protection of its own (review M1).
/// Answers what it could tell, so the screen says the words are gone only when they are (re-review R1). Clearing can't undo
/// what a clipboard history has already kept; the screen says that too.
#[tauri::command]
pub async fn clipboard_clear_words() -> Cleared {
    tauri::async_runtime::spawn_blocking(clear_words_now).await.unwrap_or(Cleared::NotGone)
}

fn clear_words_now() -> Cleared {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if os::clear_both() {
        os::confirm_gone()
    } else {
        Cleared::NotGone
    }
}

/// Drop the app's own copy, if the clipboard still holds it: the new words were given up (Back) (review L2). On a
/// thread of its own, since the clipboard's owner may be slow to answer.
#[tauri::command]
pub fn clipboard_forget_words() {
    std::thread::spawn(|| clear_pending(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORDS: &str = "abandon ability able about above absent absorb abstract absurd abuse access accident";

    #[test]
    fn both_forms_keep_the_order() {
        let owned: Vec<String> = (1..=12).map(|i| format!("w{i}")).collect();
        let words: Vec<&str> = owned.iter().map(String::as_str).collect();
        let (html, plain) = forms(&words);
        assert!(html.starts_with("<table><tr><td>1. w1</td><td>2. w2</td><td>3. w3</td></tr><tr><td>4. w4</td>"));
        assert!(html.ends_with("<td>10. w10</td><td>11. w11</td><td>12. w12</td></tr></table>"));
        assert_eq!(html.matches("<tr>").count(), 4);
        assert_eq!(plain.as_str(), words.join(" "));
    }

    #[test]
    fn markup_is_escaped() {
        let (html, _) = forms(&["<b>a&b</b>", "\"x'"]);
        assert_eq!(html.as_str(), "<table><tr><td>1. &lt;b&gt;a&amp;b&lt;/b&gt;</td><td>2. &quot;x&#39;</td></tr></table>");
    }

    #[test]
    fn words_are_told_in_text() {
        assert!(holds_words(WORDS));
        assert!(holds_words("1. abandon\t2. ability\t3. able\n4. about 5. above 6. absent 7. absorb 8. abstract 9. absurd 10. abuse 11. access 12. accident"));
        assert!(!holds_words("abandon ability able"));
        assert!(holds_words("abandon;ability;able;about;above;absent;absorb;abstract;absurd;abuse;access;\"accident\""));
        assert!(!holds_words("2LXnpjb6rTFFQr1YgfJh3ZdEC7rT 3XgiQdWivFFqmYDiypSv6kkHgv6e"));
    }

    #[tokio::test]
    async fn only_recovery_words_are_copied() {
        let err = copy_words(vec!["freebank".into(); 24]).await.unwrap_err();
        assert!(err.contains("Only the recovery words"), "{}", err);
        let err = copy_words(vec!["abandon".into(); 12]).await.unwrap_err();
        assert!(err.contains("Only the recovery words"), "{}", err);
    }

    /// Needs a display and xclip: run by hand on a virtual one (`Xvfb :96 -noreset`, as a display with no clients left
    /// resets), e.g. `DISPLAY=:96 XDG_SESSION_TYPE=x11 cargo test -j2 clipboard --
    /// --ignored --test-threads=1` with Xvfb on :96. Each test leaves the clipboard empty.
    fn xclip_set(text: &str) -> std::process::Child {
        let mut c = std::process::Command::new("xclip")
            .args(["-quiet", "-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("xclip");
        use std::io::Write;
        c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        c
    }

    fn xclip_primary(text: &str) -> std::process::Child {
        let mut c = std::process::Command::new("xclip")
            .args(["-quiet", "-selection", "primary"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("xclip");
        use std::io::Write;
        c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        c
    }

    fn text_now() -> String {
        arboard::Clipboard::new().unwrap().get_text().unwrap_or_default()
    }

    /// The copy: both forms, the hint, the own-copy test, and the clear after the wait.
    #[test]
    #[ignore]
    fn copied_both_ways_then_cleared() {
        tauri::async_runtime::block_on(start_copy(WORDS)).unwrap().unwrap();
        let mut cb = arboard::Clipboard::new().unwrap();
        assert_eq!(cb.get_text().unwrap(), WORDS);
        let got = cb.get().html().unwrap();
        assert!(got.contains("<td>1. abandon</td>") && got.contains("<td>12. accident</td>"), "{got}");
        let targets = std::process::Command::new("xclip").args(["-selection", "clipboard", "-t", "TARGETS", "-o"]).output().unwrap();
        assert!(String::from_utf8_lossy(&targets.stdout).contains("x-kde-passwordManagerHint"));
        assert!(own_copy_now());
        std::thread::sleep(CLEAR_AFTER + Duration::from_millis(700));
        assert!(text_now().is_empty(), "still on the clipboard after the wait");
        assert!(!own_copy_now());
    }

    /// The same words copied from somewhere else (a password manager) are not the app's own copy; pasting them into
    /// the app clears them, though another program holds the clipboard.
    #[test]
    #[ignore]
    fn a_copy_from_elsewhere_is_not_ours_and_is_cleared_after_a_paste() {
        tauri::async_runtime::block_on(start_copy(WORDS)).unwrap().unwrap();
        let mut x = xclip_set(WORDS);
        assert_eq!(text_now(), WORDS);
        assert!(!own_copy_now(), "plain words from another app taken for the app's own copy");
        assert_eq!(clear_words_now(), Cleared::Gone, "the clear reported a failure");
        std::thread::sleep(Duration::from_millis(300));
        assert!(text_now().is_empty(), "the pasted copy is still there");
        let _ = x.kill();
        let _ = x.wait();
    }

    /// After a clear, words put back on the clipboard (as a clipboard manager that "prevents an empty clipboard"
    /// does) are noticed, so the screen doesn't say they are gone.
    #[test]
    #[ignore]
    fn words_put_back_are_noticed() {
        // Another program holds the words; right after the app's clear, a "keeper" puts them back.
        let mut x = xclip_set(WORDS);
        // Late enough to miss the first read at a quarter second, not the second at a second (check C2).
        let keeper = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(600));
            xclip_set(WORDS)
        });
        assert_eq!(clear_words_now(), Cleared::NotGone, "the words came back, yet the clear reported them gone");
        let mut k = keeper.join().unwrap();
        for p in [&mut x, &mut k] {
            let _ = p.kill();
            let _ = p.wait();
        }
        // With nothing put back, the clear reports them gone; other text isn't taken for words.
        let mut x = xclip_set(WORDS);
        assert_eq!(clear_words_now(), Cleared::Gone);
        let _ = x.kill();
        let _ = x.wait();
        // Words left in the selection for middle-click paste are cleared too (check C3).
        let mut p = xclip_primary(WORDS);
        assert_eq!(clear_words_now(), Cleared::Gone, "words in the selection not cleared");
        let _ = p.kill();
        let _ = p.wait();
        // And in a cut buffer, where xterm puts its selections (final check, Note 1).
        {
            use x11rb::connection::Connection;
            use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
            let (conn, screen) = x11rb::connect(None).unwrap();
            let root = conn.setup().roots[screen].root;
            conn.change_property(PropMode::REPLACE, root, 9u32, AtomEnum::STRING, 8, WORDS.len() as u32, WORDS.as_bytes())
                .unwrap();
            conn.flush().unwrap();
            assert_eq!(os::cut_buffers_with_words(false), Some(true));
        }
        assert_eq!(clear_words_now(), Cleared::Gone, "words in a cut buffer not cleared");
        assert_eq!(os::cut_buffers_with_words(false), Some(false));
    }

    /// At quit, or after a panic, the app's copy goes; a copy the user made since of something else stays.
    #[test]
    #[ignore]
    fn quit_clears_only_the_apps_copy() {
        tauri::async_runtime::block_on(start_copy(WORDS)).unwrap().unwrap();
        clear_at_exit();
        assert!(text_now().is_empty());
        tauri::async_runtime::block_on(start_copy(WORDS)).unwrap().unwrap();
        clear_after_panic();
        assert!(text_now().is_empty(), "still there after the panic hook");
        tauri::async_runtime::block_on(start_copy(WORDS)).unwrap().unwrap();
        let mut x = xclip_set("something else");
        clear_at_exit();
        assert_eq!(text_now(), "something else");
        let _ = x.kill();
        let _ = x.wait();
        clear_words_now();
    }
}
