//! "Copy words": people copy their words whatever they are told, so the app makes it safe to. The recovery words go on the clipboard in two forms at
//! once, so each app takes the one it understands: HTML, a numbered table in the app's own order
//! (1 2 3 on the first row), for Notes, Pages or mail; and plain text, the words space-separated,
//! for Electrum's seed box and password managers. Both are marked for clipboard managers to leave
//! out of their history. After 60 seconds, if the clipboard still holds them, it is cleared.
//! Nothing here is logged.
//!
//! The clipboard is written from Rust, not the page, so the page never gets clipboard access.

use crate::seed::WORD_COUNT;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use zeroize::Zeroizing;

const CLEAR_AFTER: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 60 });
/// The newest copy. Only its clearer acts, so a second copy gets its own full minute.
static COPIES: AtomicU64 = AtomicU64::new(0);
/// The words of a copy whose minute isn't up, for `clear_at_exit` (security review L4).
static PENDING: std::sync::Mutex<Option<Zeroizing<String>>> = std::sync::Mutex::new(None);

/// At quit: if a copy's minute isn't up and the clipboard still holds its words, clear it now (on a
/// Mac the pasteboard would otherwise keep them after FreeBank has gone).
pub fn clear_at_exit() {
    let Some(plain) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.get_text().is_ok_and(|now| Zeroizing::new(now).as_str() == plain.as_str()) {
            let _ = cb.clear();
        }
    }
}

/// The two forms: the HTML table and the plain words.
fn forms(words: &[String]) -> (Zeroizing<String>, Zeroizing<String>) {
    let mut html = Zeroizing::new(String::from("<table>"));
    for (r, row) in words.chunks(3).enumerate() {
        html.push_str("<tr>");
        for (c, w) in row.iter().enumerate() {
            html.push_str(&format!("<td>{}. {}</td>", r * 3 + c + 1, w));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    (html, Zeroizing::new(words.join(" ")))
}

/// Put the words on the clipboard, and clear it after a minute if it still holds them. Only the
/// recovery words are taken (24 known words), so this is no general clipboard writer.
#[tauri::command]
pub async fn copy_words(words: Vec<String>) -> Result<(), String> {
    let words = Zeroizing::new(words);
    let english = bip39::Language::English;
    if words.len() != WORD_COUNT || words.iter().any(|w| english.find_word(w).is_none()) {
        return Err("Only the recovery words can be copied here.".into());
    }
    let (html, plain) = forms(&words);
    let gen = COPIES.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = tokio::sync::oneshot::channel();
    // One thread holds the clipboard for the minute: on Linux the words are served from this
    // process only while a clipboard object lives.
    std::thread::spawn(move || {
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                let _ = tx.send(Err(format!("FreeBank couldn't reach the clipboard ({}).", e)));
                return;
            }
        };
        let set = cb.set();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let set = {
            #[cfg(target_os = "linux")]
            use arboard::SetExtLinux;
            #[cfg(target_os = "macos")]
            use arboard::SetExtApple;
            set.exclude_from_history()
        };
        let done = set.html(html.as_str(), Some(plain.as_str()));
        let ok = done.is_ok();
        // Only words that are on the clipboard are waiting to be cleared (review note).
        if ok {
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(plain.clone());
        }
        let _ = tx.send(done.map_err(|e| format!("FreeBank couldn't copy the words ({}).", e)));
        if !ok {
            return;
        }
        std::thread::sleep(CLEAR_AFTER);
        if COPIES.load(Ordering::SeqCst) != gen {
            return;
        }
        PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
        if cb.get_text().is_ok_and(|now| Zeroizing::new(now).as_str() == plain.as_str()) {
            let _ = cb.clear();
        }
    });
    rx.await.map_err(|_| "FreeBank couldn't copy the words.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_forms_keep_the_order() {
        let words: Vec<String> = (1..=24).map(|i| format!("w{}", i)).collect();
        let (html, plain) = forms(&words);
        assert!(html.starts_with("<table><tr><td>1. w1</td><td>2. w2</td><td>3. w3</td></tr><tr><td>4. w4</td>"));
        assert!(html.ends_with("<td>22. w22</td><td>23. w23</td><td>24. w24</td></tr></table>"));
        assert_eq!(html.matches("<tr>").count(), 8);
        assert_eq!(plain.as_str(), words.join(" "));
    }

    /// Needs a display: run by hand on a virtual one, e.g.
    /// `DISPLAY=:97 cargo test --lib clipboard -- --ignored` with Xvfb on :97.
    #[tokio::test]
    #[ignore]
    async fn copied_both_ways_then_cleared() {
        let words: Vec<String> = bip39::Mnemonic::from_entropy(&[7u8; 32]).unwrap().words().map(String::from).collect();
        copy_words(words.clone()).await.unwrap();
        let mut cb = arboard::Clipboard::new().unwrap();
        assert_eq!(cb.get_text().unwrap(), words.join(" "));
        let html = cb.get().html().unwrap();
        assert!(html.contains(&format!("<td>1. {}</td>", words[0])) && html.contains(&format!("<td>24. {}</td>", words[23])));
        std::thread::sleep(CLEAR_AFTER + Duration::from_secs(1));
        assert!(cb.get_text().map_or(true, |t| t.is_empty()), "still on the clipboard");
    }

    #[tokio::test]
    async fn only_recovery_words_are_copied() {
        let err = copy_words(vec!["freebank".into(); 24]).await.unwrap_err();
        assert!(err.contains("Only the recovery words"), "{}", err);
        let err = copy_words(vec!["abandon".into(); 12]).await.unwrap_err();
        assert!(err.contains("Only the recovery words"), "{}", err);
    }
}
