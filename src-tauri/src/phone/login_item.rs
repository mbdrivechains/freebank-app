//! "Start when I log in" (daemon mode, v0.2.4; operator 2026-10-02: "yes.. 0.2.4. both ."): a login item that starts
//! FreeBank's background part light (`background.rs`, `--light`) when the user logs in, so a phone reaches the desktop
//! whenever the computer is on, with no window open.
//! - macOS: a LaunchAgent, `~/Library/LaunchAgents/com.ecxfreebank.freebank.phone.plist` (RunAtLoad, no KeepAlive).
//! - Linux: an autostart entry, `$XDG_CONFIG_HOME/autostart/com.ecxfreebank.freebank.phone.desktop` (freedesktop.org).
//!
//! It names this program as it is: an AppImage by its own file (`$APPIMAGE`, only when this process runs from that
//! AppImage's mount), else the running program. Each app start writes it again if it changed, so an AppImage moved
//! and opened from its new place keeps working. It is refused for a program another user could replace (it, or a
//! folder above it, writable by others: /tmp, say), and for a Mac app run from where macOS moved a download aside (App
//! Translocation: that path changes every time). It is only ever changed for this app's own folder.

use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.ecxfreebank.freebank.phone";

/// Where the login item lives on this system; None where there's none.
fn item_path() -> Option<PathBuf> {
    let home = crate::node::home();
    if cfg!(target_os = "macos") {
        Some(home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    } else if cfg!(target_os = "linux") {
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
        Some(config.join("autostart").join(format!("{LABEL}.desktop")))
    } else {
        None
    }
}

/// This program, as a login item should name it.
fn program() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("Couldn't find FreeBank's program: {e}"))?;
    let exe = running_appimage(&current).unwrap_or(current);
    if exe.to_string_lossy().contains("/AppTranslocation/") {
        return Err("Move FreeBank to your Applications folder and open it from there first: macOS runs it from a \
                    temporary place now, which changes every time."
            .into());
    }
    safe_place(&exe)?;
    Ok(exe)
}

/// The AppImage this program runs from: `$APPIMAGE`, only when `current` (this program) is inside that AppImage's
/// mount, since every child of any AppImage inherits the variable. The updater (`app_update.rs`) replaces this file.
pub(crate) fn running_appimage(current: &Path) -> Option<PathBuf> {
    let mount = std::env::var_os("APPDIR").map(PathBuf::from);
    let from_appimage = mount.as_ref().is_some_and(|m| current.starts_with(m)) || current.starts_with("/tmp/.mount_");
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file() && from_appimage)
}

/// The program, and every folder above it, belongs to this user or the system, and no one else can write to it
/// (a root-owned folder may be group-writable, as macOS's Applications is for admins; so may this user's own
/// files where the group is the user's private one, as Debian and Ubuntu set up: their umask 002 makes a downloaded
/// AppImage group-writable). The updater (`app_update.rs`) replaces the app only in such a place too.
pub(crate) fn safe_place(exe: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let me = unsafe { libc::getuid() };
        let mut p = Some(exe);
        while let Some(at) = p {
            let m = std::fs::metadata(at).map_err(|e| format!("Couldn't check {}: {e}", at.display()))?;
            let (uid, mode) = (m.uid(), m.mode());
            let others_write = mode & 0o002 != 0;
            let group_write = mode & 0o020 != 0 && uid != 0 && !(uid == me && own_private_group(m.gid()));
            if (uid != me && uid != 0) || others_write || group_write {
                return Err(format!(
                    "FreeBank runs from {}, which other users of this computer could change. Move it to your own \
                     folders (or install it) first.",
                    exe.display()
                ));
            }
            p = at.parent();
        }
    }
    Ok(())
}

/// `gid` is this user's private group: the user's primary group, named after the user, with no other members (user
/// private groups, as Debian, Ubuntu and Fedora make them). A shared primary group (macOS's "staff", a "users" group)
/// isn't. Not seen, and accepted: another account given this group as its primary one (`useradd -g alice bob`), or a
/// second group with the same id. Both take an administrator, and such an account could already change the user's files.
#[cfg(unix)]
fn own_private_group(gid: u32) -> bool {
    use std::ffi::CStr;
    // SAFETY: getpwuid_r and getgrgid_r fill structs that point into buffers owned here, which outlive every read of
    // them; each result pointer is checked before use, and the member list ends with a null pointer.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut pw_found: *mut libc::passwd = std::ptr::null_mut();
        let mut pw_buf = vec![0 as libc::c_char; 16 * 1024];
        if libc::getpwuid_r(libc::getuid(), &mut pw, pw_buf.as_mut_ptr(), pw_buf.len(), &mut pw_found) != 0
            || pw_found.is_null()
            || pw.pw_gid != gid
        {
            return false;
        }
        let mut gr: libc::group = std::mem::zeroed();
        let mut gr_found: *mut libc::group = std::ptr::null_mut();
        let mut gr_buf = vec![0 as libc::c_char; 64 * 1024];
        if libc::getgrgid_r(gid, &mut gr, gr_buf.as_mut_ptr(), gr_buf.len(), &mut gr_found) != 0 || gr_found.is_null() {
            return false;
        }
        if pw.pw_name.is_null() || gr.gr_name.is_null() {
            return false;
        }
        let user = CStr::from_ptr(pw.pw_name);
        if CStr::from_ptr(gr.gr_name) != user {
            return false;
        }
        let mut member = gr.gr_mem;
        while !member.is_null() && !(*member).is_null() {
            if CStr::from_ptr(*member) != user {
                return false;
            }
            member = member.add(1);
        }
        true
    }
}

/// The login item's text.
fn contents(exe: &Path, app_dir: &Path) -> Result<String, String> {
    let (exe, dir) = (exe.to_string_lossy(), app_dir.to_string_lossy());
    if cfg!(target_os = "macos") {
        let x = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>{}</string>
		<string>--app-dir</string>
		<string>{}</string>
		<string>{}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#,
            x(&exe),
            super::background::ARG,
            x(&dir),
            super::background::LIGHT
        ))
    } else {
        // Desktop Entry Exec: each argument in double quotes; inside them `"`, `` ` ``, `$` and `\` take a backslash,
        // and the value's own escaping doubles every backslash. Field codes (`%`) are written `%%`.
        let q = |s: &str| -> Result<String, String> {
            // A '%' or '\\' would need escaping some readers get wrong: refused, rather than an item that fails quietly.
            if s.contains(['\n', '\r', '%', '\\']) {
                return Err("FreeBank's folder or program has a character in its name a login item can't hold \
                            (%, \\ or a line break)."
                    .into());
            }
            let inner: String = s
                .chars()
                .flat_map(|c| match c {
                    '"' | '`' | '$' => vec!['\\', c],
                    '\\' => vec!['\\', '\\'],
                    '%' => vec!['%', '%'],
                    c => vec![c],
                })
                .collect();
            Ok(format!("\"{}\"", inner.replace('\\', "\\\\")))
        };
        Ok(format!(
            "[Desktop Entry]\nType=Application\nName=FreeBank (phone link)\n\
             Comment=Keeps your phone connected to FreeBank; the node starts when your phone asks.\n\
             TryExec={}\nExec={} {} --app-dir {} {}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            exe,
            q(&exe)?,
            super::background::ARG,
            q(&dir)?,
            super::background::LIGHT
        ))
    }
}

/// The login item there was written for this app folder (one per user; a test folder never touches the real one's).
fn names(path: &Path, app_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let dir = app_dir.to_string_lossy();
    if cfg!(target_os = "macos") {
        text.contains(&format!("<string>{}</string>", dir.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")))
    } else {
        let quoted: String = dir.chars().flat_map(|c| if matches!(c, '"' | '`' | '$') { vec!['\\', c] } else { vec![c] }).collect();
        text.contains(&format!(" --app-dir \"{}\" ", quoted.replace('\\', "\\\\")))
    }
}

/// "Start when I log in" is on, for this app folder.
pub fn is_on(app_dir: &Path) -> bool {
    item_path().is_some_and(|p| names(&p, app_dir))
}

/// Turn "Start when I log in" on or off. On writes the login item (owner-only), off removes it if it is this app
/// folder's. Off never fails where there's nothing to remove.
pub fn set(app_dir: &Path, on: bool) -> Result<(), String> {
    let Some(path) = item_path() else {
        return if on { Err("Starting at login isn't available on this system.".into()) } else { Ok(()) };
    };
    if !on {
        if !names(&path, app_dir) {
            return Ok(());
        }
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Couldn't remove the login item: {e}")),
        };
    }
    let text = contents(&program()?, app_dir)?;
    if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
        return Ok(()); // unchanged: no rewrite (macOS tells the user about each new login item)
    }
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    // A new file, never one already there (it could be a link somewhere else), then renamed over the item.
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    use std::io::Write;
    o.open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("Couldn't write the login item: {e}"))
}

/// At each app start: this folder's login item names this program as it is now (a moved AppImage).
pub fn refresh(app_dir: &Path) {
    if is_on(app_dir) {
        let _ = set(app_dir, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_program_and_folder_safely() {
        let t = contents(Path::new("/opt/Free Bank/FreeBank.AppImage"), Path::new("/home/a/.local/share/x\"y$z")).unwrap();
        if cfg!(target_os = "linux") {
            assert!(t.contains(r#"Exec="/opt/Free Bank/FreeBank.AppImage" --phone-background --app-dir "/home/a/.local/share/x\\"y\\$z" --light"#), "{t}");
            assert!(t.contains("TryExec=/opt/Free Bank/FreeBank.AppImage\n"), "a missing program is skipped quietly");
            // Characters some readers get wrong are refused, not written into an item that fails quietly.
            for bad in ["/a\nb", "/opt/50%/f", "/opt/a\\b"] {
                assert!(contents(Path::new(bad), Path::new("/d")).is_err(), "{bad}");
            }
        } else if cfg!(target_os = "macos") {
            assert!(t.contains("<string>/opt/Free Bank/FreeBank.AppImage</string>"));
            assert!(t.contains("<string>--light</string>"));
        }
    }

    #[test]
    fn only_this_app_folders_item_counts() {
        let tmp = std::env::temp_dir().join(format!("fb-login-item-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let item = tmp.join("item");
        let mine = Path::new("/home/a/.local/share/com.ecxfreebank.freebank");
        std::fs::write(&item, contents(Path::new("/usr/bin/freebank"), mine).unwrap()).unwrap();
        assert!(names(&item, mine));
        assert!(!names(&item, Path::new("/tmp/scratch/app")), "a test folder never takes over the real item");
        assert!(!names(&item, Path::new("/home/a/.local/share/com.ecxfreebank")), "nor a folder that is a prefix of it");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_program_others_could_replace_is_refused() {
        let tmp = std::env::temp_dir().join(format!("fb-login-place-{}", std::process::id()));
        let shared = tmp.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        let exe = shared.join("FreeBank.AppImage");
        std::fs::write(&exe, b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(safe_place(&exe).is_err(), "a world-writable folder");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = safe_place(&exe); // the rest depends on the machine's /tmp (sticky, world-writable): refused too
        assert!(safe_place(Path::new("/usr/bin/env")).is_ok(), "a system program");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_users_private_group_counts_as_the_user() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // Beside the source, so the folders above are the user's (not /tmp). On a Debian or Ubuntu machine they are
        // group-writable too (umask 002), which this test then shows is accepted.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tmp = src.join(format!(".fb-login-group-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        let exe = tmp.join("FreeBank.AppImage");
        std::fs::write(&exe, b"x").unwrap();
        // As a download looks after chmod +x with umask 002.
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o775)).unwrap();
        let gid = std::fs::metadata(&exe).unwrap().gid();
        if safe_place(src).is_ok() {
            assert_eq!(safe_place(&exe).is_ok(), own_private_group(gid), "group-writable is fine only in the user's own group");
        } else {
            eprintln!("skipped: {} is a place other users could change", src.display());
        }
        // A shared group, never: root's group is no one's private group here.
        assert!(!own_private_group(0) || unsafe { libc::getuid() } == 0);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_background_part_reads_what_the_login_item_says() {
        // The arguments after the program, as the system passes them, are what background.rs takes.
        let args: Vec<String> =
            ["freebank", super::super::background::ARG, "--app-dir", "/d", super::super::background::LIGHT].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::super::background::requested(&args), Some((PathBuf::from("/d"), true)));
    }
}
