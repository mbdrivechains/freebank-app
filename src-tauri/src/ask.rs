//! Ask FreeBank (v0.4.6): a question from the app to FreeBank's AI helper, which answers from FreeBank's notes only
//! and can't act. It is asked from Rust at one fixed address over TLS, so the page never talks to the internet and no
//! key ships in the app (the helper's key stays on FreeBank's server). Nothing is kept: no question, no answer.
//!
//! A question that looks like recovery words (12 or more BIP39 words) is refused here and never sent. The helper
//! refuses those too, and has its own limits: 500 characters, so many questions an hour, a daily spend cap.

use serde_json::{json, Value};
use std::time::Duration;

/// The only place a question goes.
pub const ASK_URL: &str = "https://ask.ecxfreebank.com/ask";
pub const MAX_QUESTION: usize = 500;

/// Twelve or more recovery-list words within any 15 in a row: likely someone's recovery words, however much text is
/// around them (security review of v0.4.6, M2). A word counts by its first four letters too, as BIP39 words are
/// unique by them and people shorten them.
pub fn looks_like_words(q: &str) -> bool {
    let english = bip39::Language::English;
    let list = english.word_list();
    let hit: Vec<bool> = q
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let t = t.to_ascii_lowercase();
            english.find_word(&t).is_some() || (t.len() == 4 && list.iter().any(|w| w.starts_with(&t)))
        })
        .collect();
    hit.windows(15.min(hit.len()).max(1)).any(|w| w.iter().filter(|&&h| h).count() >= 12)
}

/// What to send, or why not.
pub fn check(question: &str) -> Result<String, String> {
    let q = question.trim();
    if q.is_empty() {
        return Err("Ask a question.".into());
    }
    if q.chars().count() > MAX_QUESTION {
        return Err(format!("Please keep it under {MAX_QUESTION} characters."));
    }
    if looks_like_words(q) {
        return Err("That looks like recovery words. Never type them anywhere, here included: anyone who has them can \
                    take your coins. Nothing was sent."
            .into());
    }
    Ok(q.to_string())
}

/// The most of an answer read.
const MAX_BODY: usize = 64 * 1024;

#[tauri::command]
pub async fn ask_helper(question: String) -> Result<String, String> {
    let q = check(&question)?;
    // Its own client: https only, no redirects (a 307 would carry the question elsewhere), the app's user agent.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .user_agent(concat!("FreeBank-app/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut r = http
        .post(ASK_URL)
        .timeout(Duration::from_secs(50))
        .json(&json!({ "q": q }))
        .send()
        .await
        .map_err(|_| "FreeBank's helper isn't answering just now. Try again in a minute.".to_string())?;
    let mut body = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| "FreeBank's helper stopped answering. Try again.".to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err("FreeBank's helper gave an answer FreeBank couldn't read.".into());
        }
    }
    let v: Value = serde_json::from_slice(&body).map_err(|_| "FreeBank's helper gave an answer FreeBank couldn't read.".to_string())?;
    if let Some(a) = v["answer"].as_str() {
        return Ok(a.chars().take(4000).collect());
    }
    Err(v["error"].as_str().map(|e| e.chars().take(400).collect()).unwrap_or_else(|| "FreeBank's helper couldn't answer just now.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_words_never_leave_the_computer() {
        let words = bip39::Mnemonic::from_entropy(&[9u8; 32]).unwrap().to_string();
        assert!(check(&words).unwrap_err().contains("recovery words"));
        assert!(check(&format!("are these right? {words}")).is_err());
        let twelve = bip39::Mnemonic::from_entropy(&[9u8; 16]).unwrap().to_string();
        assert!(check(&twelve).is_err());
        // Wrapped in a long question (the review's bypass), or shortened to four letters, or in capitals.
        let chatter = "I wrote these down on a card yesterday and I am not sure they are right so could you please tell me \
                       whether this looks like a proper backup for my wallet when I get home tonight";
        assert!(check(&format!("{chatter} {words}")).is_err());
        let short: Vec<String> = words.split(' ').map(|w| w.chars().take(4).collect()).collect();
        assert!(check(&short.join(" ")).is_err());
        assert!(check(&words.to_uppercase()).is_err());
        assert!(check(chatter).is_ok(), "a long ordinary question goes");
        // An ordinary question, even one full of common words, goes.
        assert_eq!(check("  How do I borrow from a house, and when do I repay?  ").unwrap(), "How do I borrow from a house, and when do I repay?");
        assert!(check("").is_err() && check(&"x".repeat(501)).is_err() && check(&"é".repeat(500)).is_ok());
    }

    #[test]
    fn questions_go_only_to_freebanks_helper() {
        assert_eq!(ASK_URL, "https://ask.ecxfreebank.com/ask");
    }
}
