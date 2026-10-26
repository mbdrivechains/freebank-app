//! Changes to the computer itself that need its administrator, made through pkexec on Linux (the desktop's own
//! password prompt), and on a Mac through Finder: the opt-in to FreeBank's apt repository (Settings > App updates),
//! installing a new version of the Debian package (Update and restart), and "Remove the app too" (after Obliterate). Each script is a constant, or built from paths FreeBank found itself;
//! nothing typed by the user goes into one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the .deb puts the apt repository's public key (`scripts/deb-finalize.sh`).
pub const APT_KEYRING: &str = "/usr/share/keyrings/freebank-archive-keyring.gpg";
/// The apt source FreeBank writes when the user says yes (deb822 style).
pub const APT_SOURCE: &str = "/etc/apt/sources.list.d/freebank.sources";

/// pkexec, where the desktop has it.
pub fn pkexec() -> Option<PathBuf> {
    ["/usr/bin/pkexec", "/bin/pkexec"].iter().map(PathBuf::from).find(|p| p.is_file())
}

/// The apt source's text: FreeBank's repository, trusted only with its own key.
pub fn apt_source_text() -> String {
    format!(
        "Types: deb\nURIs: https://apt.ecxfreebank.com\nSuites: stable\nComponents: main\nSigned-By: {APT_KEYRING}\n"
    )
}

/// Writes the apt source in one step (a temporary file, then a rename), readable by everyone.
pub fn apt_enable_script() -> String {
    let lines: Vec<String> = apt_source_text().lines().map(|l| format!("'{l}'")).collect();
    format!(
        "set -e\numask 022\ntest -d /etc/apt/sources.list.d\nprintf '%s\\n' {} > {APT_SOURCE}.new\nmv -f {APT_SOURCE}.new {APT_SOURCE}\n",
        lines.join(" ")
    )
}

/// Removes the FreeBank package, then any apt source line that names FreeBank's repository (the opt-in's file, or the
/// one the apt page's command writes). The package's own postrm does the same on purge; this covers a package older
/// than that.
pub const REMOVE_DEB_SCRIPT: &str = "set -e
export DEBIAN_FRONTEND=noninteractive
if ! dpkg-query -W -f='${Status}' freebank 2>/dev/null | grep -q 'install ok installed'; then
  echo 'The FreeBank package is not installed, so there is nothing to remove.' >&2
  exit 3
fi
apt-get purge -y freebank
for f in /etc/apt/sources.list.d/freebank.sources /etc/apt/sources.list.d/freebank.list; do
  if [ -f \"$f\" ] && grep -q 'apt.ecxfreebank.com' \"$f\"; then rm -f \"$f\"; fi
done
";

/// Installs a new FreeBank .deb that the app downloaded and checked against the release's signed checksums (v0.6.6,
/// Update and restart for the Debian package). The package comes in on stdin, from the very file the app checked, so
/// root never opens a path the user could change; it is written into a folder only root can change (512 MiB at most,
/// as the download) and checked again there before apt installs it, with any new dependency. apt's downloads go to
/// that folder too, and it waits for another apt or Software Updater to finish. Exit 4: the copy doesn't match the
/// signed checksum; 5: apt left an older version. Only a checksum and a version go in, each checked to be plain.
pub fn deb_install_script(sha256: &str, version: &str) -> Result<String, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("The package's checksum isn't plain, so it isn't installed.".into());
    }
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err("The package's version isn't plain, so it isn't installed.".into());
    }
    Ok(format!(
        "set -e
export DEBIAN_FRONTEND=noninteractive
t=$(mktemp -d)
trap 'rm -rf \"$t\"' EXIT
f=\"$t/freebank_{version}_amd64.deb\"
head -c 536870912 > \"$f\"
if ! echo '{sha256}  '\"$f\" | sha256sum -c --status; then
  echo 'The package changed after FreeBank checked it, so nothing was installed.' >&2
  exit 4
fi
# apt reads a local package, and downloads, as its own unprivileged user.
chmod 755 \"$t\"
chmod 644 \"$f\"
mkdir -p \"$t/archives/partial\"
apt-get install -y -o DPkg::Lock::Timeout=300 -o Dir::Cache::Archives=\"$t/archives/\" \"$f\"
if ! dpkg --compare-versions \"$(dpkg-query -W -f='${{Version}}' freebank)\" ge '{version}'; then
  echo 'apt finished, but FreeBank {version} is not the version installed.' >&2
  exit 5
fi
"
    ))
}

/// Run `script` with /bin/sh as root. pkexec shows the password prompt; its exit codes 126 and 127 mean the prompt was
/// dismissed or the password refused.
pub fn run_as_root(script: &str) -> Result<(), String> {
    run_as_root_with(script, std::process::Stdio::null())
}

/// `run_as_root`, with `input` as the script's stdin (pkexec passes it on).
pub fn run_as_root_with(script: &str, input: std::process::Stdio) -> Result<(), String> {
    let pk = pkexec().ok_or("This computer has no pkexec, so FreeBank can't ask for your password.")?;
    let out = Command::new(pk)
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(input)
        .output()
        .map_err(|e| format!("FreeBank couldn't ask for your password: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(say_failure(out.status.code(), &String::from_utf8_lossy(&out.stderr)))
}

/// pkexec's (or the script's) failure, in plain words. pkexec exits 126 when the prompt is dismissed, and 127 when the
/// password is refused ("Not authorized", then "This incident has been reported") or no password prompt runs.
pub fn say_failure(code: Option<i32>, stderr: &str) -> String {
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match code {
        Some(126) => "Nothing changed: the password prompt was closed.".into(),
        Some(127) if stderr.contains("No authentication agent") => {
            "Nothing changed: your desktop isn't running its password prompt, so FreeBank can't ask for your password.".into()
        }
        Some(127) if stderr.contains("Not authorized") || last.is_empty() => {
            "Nothing changed: the password wasn't accepted.".into()
        }
        _ if last.is_empty() => "It didn't work, and the system gave no reason.".into(),
        _ => format!("It didn't work: {last}"),
    }
}

/// The Mac app's bundle to the Bin, through Finder (so it can be put back). Finder may first ask whether FreeBank may
/// control it.
pub fn trash_mac_bundle(bundle: &Path) -> Result<(), String> {
    let path = bundle.to_str().ok_or("The app's path isn't text.")?;
    let quoted = path.replace('\\', "\\\\").replace('"', "\\\"");
    let out = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!("tell application \"Finder\" to delete (POSIX file \"{quoted}\" as alias)"))
        .output()
        .map_err(|e| format!("FreeBank couldn't ask Finder: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        Err(format!("Finder didn't move it: {}", said.trim()))
    }
}

/// Show the bundle in Finder, for dragging to the Bin by hand.
pub fn reveal_mac_bundle(bundle: &Path) {
    let _ = Command::new("/usr/bin/open").arg("-R").arg(bundle).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opt_in_writes_the_source_the_update_check_reads() {
        let tmp = std::env::temp_dir().join(format!("fb-admin-{}", std::process::id()));
        let d = tmp.join("etc/apt/sources.list.d");
        std::fs::create_dir_all(&d).unwrap();
        // The script, run against a scratch folder in place of /etc/apt.
        let script = apt_enable_script().replace("/etc/apt", &tmp.join("etc/apt").to_string_lossy());
        let ok = Command::new("/bin/sh").arg("-c").arg(&script).status().unwrap();
        assert!(ok.success());
        let written = std::fs::read_to_string(d.join("freebank.sources")).unwrap();
        assert_eq!(written, apt_source_text());
        assert!(!d.join("freebank.sources.new").exists());
        assert!(crate::app_update::apt_repository_set_up(&tmp.join("etc/apt")));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn pkexecs_answers_in_plain_words() {
        assert!(say_failure(Some(126), "").contains("closed"));
        assert!(say_failure(Some(127), "Error executing command as another user: Not authorized\n\nThis incident has been reported.\n").contains("wasn't accepted"));
        assert!(say_failure(Some(127), "Error executing command as another user: No authentication agent found.\n").contains("password prompt"));
        assert_eq!(say_failure(Some(3), "The FreeBank package is not installed, so there is nothing to remove.\n"), "It didn't work: The FreeBank package is not installed, so there is nothing to remove.");
    }

    /// The install script against a fake apt-get and dpkg-query (first on PATH), the package on its stdin: its exit
    /// code, what apt was given, and its stderr.
    fn run_deb_install(name: &str, deb_bytes: &[u8], sha: &str, installed: &str) -> (Option<i32>, String, String) {
        let tmp = std::env::temp_dir().join(format!("fb-admin-deb-{name}-{}", std::process::id()));
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let deb = tmp.join("freebank-update.deb");
        std::fs::write(&deb, deb_bytes).unwrap();
        let log = tmp.join("apt.log");
        let fake = |n: &str, body: String| {
            let p = bin.join(n);
            std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        };
        // The fake apt-get records its arguments, then each file's checksum and mode, as apt would find them.
        fake(
            "apt-get",
            format!(
                "echo \"$@\" >> {log}\nfor a; do if [ -f \"$a\" ]; then sha256sum \"$a\" | cut -d' ' -f1 >> {log}; stat -c %a \"$a\" >> {log}; fi; done\n",
                log = log.display()
            ),
        );
        fake("dpkg-query", format!("printf %s '{installed}'\n"));
        let script = deb_install_script(sha, "0.6.6").unwrap();
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .stdin(std::fs::File::open(&deb).unwrap())
            .output()
            .unwrap();
        let logged = std::fs::read_to_string(&log).unwrap_or_default();
        std::fs::remove_dir_all(&tmp).unwrap();
        (out.status.code(), logged, String::from_utf8_lossy(&out.stderr).into_owned())
    }

    #[test]
    fn the_deb_install_checks_its_own_copy_then_hands_it_to_apt() {
        use sha2::Digest;
        let bytes = b"a package";
        let sha = format!("{:x}", sha2::Sha256::digest(bytes));
        let (code, logged, _) = run_deb_install("ok", bytes, &sha, "0.6.6");
        assert_eq!(code, Some(0));
        let lines: Vec<&str> = logged.lines().collect();
        assert!(lines[0].starts_with("install -y -o DPkg::Lock::Timeout=300 -o Dir::Cache::Archives=/"), "{logged}");
        assert!(lines[0].ends_with("/freebank_0.6.6_amd64.deb"), "a copy in root's own folder: {logged}");
        assert!(!lines[0].contains("freebank-update.deb"), "never the user's own file: {logged}");
        assert_eq!(lines[1], sha);
        assert_eq!(lines[2], "644");

        // Not the signed checksum: apt is never called.
        let (code, logged, err) = run_deb_install("bad", b"something else", &sha, "0.6.6");
        assert_eq!(code, Some(4));
        assert!(logged.is_empty());
        assert!(say_failure(code, &err).contains("changed after FreeBank checked it"));

        // apt left an older version in place; a Debian revision of the same version is that version.
        let (code, _, err) = run_deb_install("old", bytes, &sha, "0.6.5");
        assert_eq!(code, Some(5));
        assert!(err.contains("FreeBank 0.6.6 is not the version installed"));
        assert_eq!(run_deb_install("rev", bytes, &sha, "0.6.6-1").0, Some(0));
    }

    #[test]
    fn the_deb_install_takes_only_plain_values() {
        let sha = "a".repeat(64);
        assert!(deb_install_script(&sha, "0.6.6").is_ok());
        assert!(deb_install_script("abc", "0.6.6").is_err());
        assert!(deb_install_script(&"A".repeat(64), "0.6.6").is_err());
        assert!(deb_install_script(&sha, "0.6.6'").is_err());
        assert!(deb_install_script(&sha, "").is_err());
    }

    #[test]
    fn removing_takes_out_only_sources_that_name_freebank() {
        let tmp = std::env::temp_dir().join(format!("fb-admin-rm-{}", std::process::id()));
        let d = tmp.join("etc/apt/sources.list.d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("freebank.list"), "deb https://apt.ecxfreebank.com stable main\n").unwrap();
        std::fs::write(d.join("freebank.sources"), "Types: deb\nURIs: https://example.org\n").unwrap();
        // Only the loop: the package check and the purge need a real package.
        let loop_at = REMOVE_DEB_SCRIPT.find("for f in").unwrap();
        let script = REMOVE_DEB_SCRIPT[loop_at..]
            .to_string()
            .replace("/etc/apt", &tmp.join("etc/apt").to_string_lossy());
        assert!(Command::new("/bin/sh").arg("-c").arg(&script).status().unwrap().success());
        assert!(!d.join("freebank.list").exists());
        assert!(d.join("freebank.sources").exists(), "someone else's file of that name stays");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
