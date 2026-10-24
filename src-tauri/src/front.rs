//! The window to the front after a restart the app made itself (v0.6.5): an update's restart (app_update.rs) and a
//! network switch (network.rs). On a Mac the new copy opened behind the other windows. The restart leaves a mark in the
//! top folder; the next start deletes it and brings the window forward. Any other start leaves the window where the
//! system puts it.

use std::path::Path;
use tauri::Manager;

const MARK: &str = "restarted";

/// Just before a restart the app makes itself.
pub fn mark(top: &Path) {
    let _ = std::fs::write(top.join(MARK), b"");
}

/// The mark was there; it is gone now.
fn take(top: &Path) -> bool {
    std::fs::remove_file(top.join(MARK)).is_ok()
}

/// At start: after a restart the app made, the main window to the front once it shows (on macOS `set_focus` activates
/// the app too).
pub fn after_start(app: &tauri::AppHandle, top: &Path) {
    if !take(top) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // The window shows a moment after setup, and a request before that may be ignored.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        crate::activity::note("restarted: window to the front");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_counts_once() {
        let d = std::env::temp_dir().join(format!("fb-front-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(!take(&d), "an ordinary start");
        mark(&d);
        assert!(take(&d), "the start after a restart");
        assert!(!take(&d), "and only that one");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
