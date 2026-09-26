//! Installing freebankd: pick the newest C++ release, check that its SHA256SUMS carries a good
//! signature from FreeBank's release key (SHA256SUMS.sig, pinned key in release_key.rs; releases
//! before v0.2.16 have none and are refused) before anything else is downloaded, download it and
//! check it against its line there, unpack it into the app's data folder, find or fetch grpcurl,
//! and write freebank.conf. Update takes the same path.

use super::{
    detect, home, platform, release_key, NodeManager, DATADIR_MARK, GRPCURL_VERSION, RELEASES_URL,
    RELEASE_DOWNLOAD, SEED_TAG,
};
use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// What the install screen shows. Stages run in order:
/// release, signature, download, verify, unpack, grpcurl, config, start.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InstallProgress {
    pub running: bool,
    pub done: bool,
    pub stage: String,
    pub tag: Option<String>,
    pub bytes: u64,
    pub total: Option<u64>,
    /// A short line under the current stage ("using BitWindow's grpcurl").
    pub note: Option<String>,
    pub error: Option<String>,
    /// Set by "Cancel"; the install stops before it writes anything.
    pub cancelled: bool,
}

/// The stages "Cancel" can stop: nothing has been written yet (bar the download in tmp/).
pub fn cancellable(stage: &str) -> bool {
    matches!(stage, "release" | "signature" | "download" | "verify")
}

/// Move on to `stage`, unless the install was cancelled. Checked and set under one lock, so a
/// cancel either lands before the stage starts or is refused.
fn enter(p: &Arc<std::sync::Mutex<InstallProgress>>, stage: &str) -> Result<(), String> {
    let mut s = p.lock().unwrap();
    if s.cancelled {
        return Err("Cancelled.".into());
    }
    s.stage = stage.into();
    s.note = None;
    Ok(())
}

const TAG_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// "freebank-" plus 4 characters that can't be misread.
pub fn suggest_tag() -> String {
    let mut rng = rand::thread_rng();
    let tail: String = (0..4)
        .map(|_| TAG_ALPHABET[rng.gen_range(0..TAG_ALPHABET.len())] as char)
        .collect();
    format!("freebank-{}", tail)
}

/// The name on your blocks: what freebankd accepts, minus what breaks freebank.conf.
pub fn validate_tag(t: &str) -> Result<(), String> {
    if t.is_empty() {
        return Err("Please enter a name.".into());
    }
    if t.len() > 64 {
        return Err("The name can be at most 64 characters.".into());
    }
    if t.bytes().any(|b| !(0x20..=0x7e).contains(&b)) {
        return Err("Use plain letters, digits, punctuation and spaces.".into());
    }
    if t.contains('#') {
        return Err("The name can't contain #.".into());
    }
    if t.starts_with(['"', '\'']) || t.ends_with(['"', '\'']) {
        return Err("No quotes around the name, please.".into());
    }
    if t.starts_with(' ') || t.ends_with(' ') {
        return Err("No spaces at the start or end.".into());
    }
    if t == SEED_TAG {
        return Err(format!("{} is the FreeBank seed's name; pick your own.", SEED_TAG));
    }
    Ok(())
}

/// The newest published C++ release: non-draft, non-prerelease, tag v0.2.N.
/// (The retired Rust node is the v0.3.x line; GitHub's "latest" flag is not relied on.)
pub async fn find_release(http: &reqwest::Client) -> Result<String, String> {
    let list: serde_json::Value = http
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't reach GitHub for the release list ({}). Try again in a minute.", e))?
        .json()
        .await
        .map_err(|e| format!("GitHub's release list didn't parse: {}", e))?;
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] == false && r["prerelease"] == false)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let patch: u32 = tag.strip_prefix("v0.2.")?.parse().ok()?;
            Some((patch, tag.to_string()))
        })
        .max()
        .map(|(_, tag)| tag)
        .ok_or_else(|| "GitHub lists no FreeBank v0.2 release.".into())
}

/// Stream `url` to `dest`, reporting bytes as they arrive.
async fn download(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut resp = http
        .get(url)
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download failed: {} ({})", url, e))?;
    let total = resp.content_length();
    let mut f = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut got = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("Download interrupted: {}", e))? {
        f.write_all(&chunk).map_err(|e| e.to_string())?;
        got += chunk.len() as u64;
        progress(got, total);
    }
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

async fn download_text(http: &reqwest::Client, url: &str) -> Result<String, String> {
    http.get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download failed: {} ({})", url, e))?
        .text()
        .await
        .map_err(|e| e.to_string())
}

/// A small file from a release, whole and byte for byte. Ok(None) when the release has no such file.
async fn download_bytes(http: &reqwest::Client, url: &str) -> Result<Option<Vec<u8>>, String> {
    let failed = |e: reqwest::Error| format!("Download failed: {} ({})", url, e);
    let resp = http
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(failed)?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = resp.error_for_status().map_err(failed)?.bytes().await.map_err(failed)?;
    Ok(Some(body.to_vec()))
}

/// SHA256SUMS as text, once its signature checks out: the very bytes that were verified.
/// `sig` is None when the release has no SHA256SUMS.sig (v0.2.15 and older).
fn signed_sums<'a>(tag: &str, sums: &'a [u8], sig: Option<&[u8]>) -> Result<&'a str, String> {
    let sig = sig.ok_or_else(|| format!("FreeBank {} isn't signed, so it wasn't installed.", tag))?;
    release_key::verify_sums(sums, sig).map_err(|why| {
        format!("The release's signature didn't check out, so nothing was installed ({}).", why)
    })?;
    std::str::from_utf8(sums)
        .map_err(|_| "The release's checksums file isn't plain text, so nothing was installed.".into())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| e.to_string())?;
    Ok(hex::encode(h.finalize()))
}

/// The hash listed for `name` in a sha256sum-style file ("<hash>  name" or "<hash> *name").
fn listed_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let file = it.next()?.trim_start_matches('*');
        (file == name).then(|| hash.to_lowercase())
    })
}

fn untar_gz(archive: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
    a.set_preserve_permissions(true);
    a.unpack(dest).map_err(|e| format!("Couldn't unpack {}: {}", archive.display(), e))
}

/// Does this program run here? (`-version` for both freebankd and grpcurl.)
fn runs(bin: &Path) -> bool {
    std::process::Command::new(bin)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn unquarantine(p: &Path) {
    let _ = std::process::Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(p)
        .output();
}
#[cfg(not(target_os = "macos"))]
fn unquarantine(_: &Path) {}

/// grpcurl, in order of preference: the one we used last time, BitWindow's, PATH, our own copy.
pub fn find_grpcurl(app_dir: &Path, saved: Option<&str>) -> Option<(PathBuf, &'static str)> {
    let bitwindow = if cfg!(target_os = "macos") {
        home().join("Library/Application Support/bitwindow/assets/bin/grpcurl")
    } else {
        home().join(".local/share/bitwindow/assets/bin/grpcurl")
    };
    let on_path = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("grpcurl"))
            .find(|f| f.is_file())
    });
    let candidates = [
        (saved.map(PathBuf::from), "the grpcurl FreeBank used before"),
        (Some(bitwindow), "BitWindow's grpcurl"),
        (on_path, "grpcurl from your PATH"),
        (Some(app_dir.join("tools/grpcurl")), "FreeBank's own grpcurl"),
    ];
    candidates
        .into_iter()
        .filter_map(|(p, why)| p.map(|p| (p, why)))
        .find(|(p, _)| p.is_file() && runs(p))
}

async fn fetch_grpcurl(http: &reqwest::Client, app_dir: &Path, tmp: &Path) -> Result<PathBuf, String> {
    let (_, os_arch) = platform()?;
    let asset = format!("grpcurl_{v}_{os_arch}.tar.gz", v = GRPCURL_VERSION);
    let base = format!(
        "https://github.com/fullstorydev/grpcurl/releases/download/v{}",
        GRPCURL_VERSION
    );
    let archive = tmp.join(&asset);
    download(http, &format!("{}/{}", base, asset), &archive, |_, _| {}).await?;
    let sums = download_text(http, &format!("{}/grpcurl_{}_checksums.txt", base, GRPCURL_VERSION)).await?;
    let want = listed_hash(&sums, &asset).ok_or("grpcurl's checksum list doesn't include this build")?;
    if want != sha256_file(&archive)? {
        return Err("grpcurl's download didn't match its checksum. Nothing was installed.".into());
    }
    let unpacked = tmp.join("grpcurl-unpacked");
    untar_gz(&archive, &unpacked)?;
    let tools = app_dir.join("tools");
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    let dest = tools.join("grpcurl");
    std::fs::copy(unpacked.join("grpcurl"), &dest).map_err(|e| e.to_string())?;
    unquarantine(&dest);
    if !runs(&dest) {
        return Err("The downloaded grpcurl doesn't run on this machine.".into());
    }
    Ok(dest)
}

/// Written into releases/<tag>/ once its archive has passed the signature and hash checks; it holds
/// the archive's SHA-256. Only a release with it is reused. One unpacked any other way (by an
/// earlier build of the app that didn't check signatures) is downloaded and checked again.
const VERIFIED: &str = ".verified";

/// Is releases/<tag> one this code checked, and does it run? A copy without the marker is never
/// run, not even for -version.
fn already_verified(mgr: &NodeManager, tag: &str) -> bool {
    let bin = mgr.freebankd(tag);
    mgr.release_dir(tag).join(VERIFIED).is_file() && bin.is_file() && runs(&bin)
}

/// Stages signature, download, verify: check SHA256SUMS's signature and find `asset` in it, and
/// only then download `asset` from `base` into `archive` and check it against that line. An
/// unsigned or badly signed release costs no download. Returns the archive's SHA-256.
async fn fetch_checked(
    http: &reqwest::Client,
    base: &str,
    tag: &str,
    asset: &str,
    archive: &Path,
    p: &Arc<std::sync::Mutex<InstallProgress>>,
) -> Result<String, String> {
    set(p, |s| {
        s.stage = "signature".into();
        s.note = None;
    });
    let sums = download_bytes(http, &format!("{}/SHA256SUMS", base)).await?;
    let sig = download_bytes(http, &format!("{}/SHA256SUMS.sig", base)).await?;
    let sums = sums.ok_or_else(|| format!("FreeBank {} has no checksums file, so it wasn't installed.", tag))?;
    let sums = signed_sums(tag, &sums, sig.as_deref())?;
    let want = listed_hash(sums, asset).ok_or(format!("{} isn't in the release's signed checksums.", asset))?;

    set(p, |s| {
        s.stage = "download".into();
        s.note = Some(asset.to_string());
    });
    let prog = p.clone();
    download(http, &format!("{}/{}", base, asset), archive, move |got, total| {
        set(&prog, |s| {
            s.bytes = got;
            s.total = total;
        })
    })
    .await?;

    set(p, |s| {
        s.stage = "verify".into();
        s.note = None;
    });
    let got = sha256_file(archive)?;
    if want != got {
        return Err(format!(
            "The download didn't match its signed checksums, so nothing was installed. Please try again. (expected {}, got {})",
            want, got
        ));
    }
    Ok(got)
}

/// Download `tag` for this machine, check SHA256SUMS's signature and the download against it,
/// and unpack it into releases/<tag>/. Skipped when this code already did that and it runs.
/// This is the only way a freebankd gets into releases/, for install and Update alike.
async fn fetch_release(
    mgr: &NodeManager,
    tag: &str,
    p: &Arc<std::sync::Mutex<InstallProgress>>,
    tmp: &Path,
) -> Result<(), String> {
    let (triplet, _) = platform()?;
    let bin = mgr.freebankd(tag);
    if already_verified(mgr, tag) {
        enter(p, "unpack")?;
        set(p, |s| s.note = Some(format!("FreeBank {} is already downloaded.", tag)));
        return Ok(());
    }
    let version = tag.trim_start_matches('v');
    let asset = format!("freebank-{}-{}.tar.gz", version, triplet);
    let base = format!("{}/{}", RELEASE_DOWNLOAD, tag);
    let archive = tmp.join(&asset);
    let got = fetch_checked(&mgr.http, &base, tag, &asset, &archive, p).await?;

    enter(p, "unpack")?;
    let rel = mgr.release_dir(tag);
    let _ = std::fs::remove_dir_all(&rel);
    std::fs::create_dir_all(&rel).map_err(|e| e.to_string())?;
    untar_gz(&archive, &rel)?;
    unquarantine(&rel);
    if !bin.is_file() {
        return Err("The release archive has no freebank/bin/freebankd.".into());
    }
    if !runs(&bin) {
        return Err("freebankd doesn't run on this machine.".into());
    }
    std::fs::write(rel.join(VERIFIED), format!("{}\n", got)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Update, run as a background task; the Node tab polls `mgr.update`.
/// Stages: release, signature, download, verify, unpack, stop, start. The old release folder is kept.
pub async fn run_update(mgr: Arc<NodeManager>) {
    let p = mgr.update.clone();
    let result = update(&mgr).await;
    set(&p, |s| {
        s.running = false;
        match result {
            Ok(()) => s.done = true,
            Err(_) if s.cancelled => {}
            Err(e) => s.error = Some(e),
        }
    });
}

async fn update(mgr: &Arc<NodeManager>) -> Result<(), String> {
    let p = mgr.update.clone();
    mgr.still_here()?;
    let old = mgr
        .settings
        .lock()
        .await
        .installed_tag
        .clone()
        .ok_or("This app didn't install the FreeBank node, so it can't update it.")?;
    let ours = super::process::child_alive(mgr).await;
    if !ours {
        let s = mgr.settings.lock().await.clone();
        if detect::probe(&mgr.http, &s).await.state != detect::RpcState::Down {
            return Err("A FreeBank node started by another program is running. \
                        Stop it there first, then update."
                .into());
        }
    }

    set(&p, |s| s.stage = "release".into());
    let tag = mgr.latest_release(true).await?;
    set(&p, |s| s.tag = Some(tag.clone()));
    if super::patch(&tag) <= super::patch(&old) {
        set(&p, |s| s.note = Some(format!("{} is already the newest.", old)));
        return Ok(());
    }

    let tmp = mgr.app_dir.join("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    fetch_release(mgr, &tag, &p, &tmp).await?;
    let _ = std::fs::remove_dir_all(&tmp);

    set(&p, |s| {
        s.stage = "stop".into();
        s.note = None;
    });
    let running = {
        let _busy = mgr.busy(&format!("Updating to {}…", tag))?;
        let running = super::process::child_alive(mgr).await;
        if running {
            super::process::stop(mgr).await?;
        }
        let mut s = mgr.settings.lock().await.clone();
        s.installed_tag = Some(tag.clone());
        mgr.save_settings(s).await?;
        running
    };

    set(&p, |s| s.stage = "start".into());
    if running {
        super::process::start(mgr).await?;
    } else {
        set(&p, |s| s.note = Some("The node was stopped; start it when you're ready.".into()));
    }
    Ok(())
}

/// Write the name into freebank.conf, keeping every other line the user has there.
pub fn write_conf(datadir: &Path, tag: &str) -> Result<(), String> {
    std::fs::create_dir_all(datadir).map_err(|e| e.to_string())?;
    let path = datadir.join("freebank.conf");
    let mut out = match std::fs::read_to_string(&path) {
        Ok(old) => old
            .lines()
            .filter(|l| !l.starts_with("coinbasetag="))
            .map(|l| format!("{}\n", l))
            .collect::<String>(),
        Err(_) => "# FreeBank node settings. Written by the FreeBank app.\n".to_string(),
    };
    out.push_str(&format!("coinbasetag={}\n", tag));
    let tmp = datadir.join("freebank.conf.new");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn set(p: &Arc<std::sync::Mutex<InstallProgress>>, f: impl FnOnce(&mut InstallProgress)) {
    f(&mut p.lock().unwrap());
}

/// The whole install, run as a background task; the screen polls `mgr.install`.
pub async fn run(mgr: Arc<NodeManager>, tag_name: String, move_aside: bool) {
    let p = mgr.install.clone();
    let result = install(&mgr, &tag_name, move_aside).await;
    set(&p, |s| {
        s.running = false;
        match result {
            Ok(()) => s.done = true,
            Err(e) => s.error = Some(e),
        }
    });
}

async fn install(mgr: &Arc<NodeManager>, tag_name: &str, move_aside: bool) -> Result<(), String> {
    let p = mgr.install.clone();
    mgr.still_here()?;
    platform()?;
    validate_tag(tag_name)?;
    let settings = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&settings.datadir);

    // Settle the data folder before anything is downloaded, so a refusal costs nothing.
    let dd = detect::check_datadir(&datadir);
    // Missing, empty or only leftovers, or moved aside at the config stage: this install creates
    // the folder, and it is recorded so "Obliterate" may remove it. A folder in use stays unrecorded.
    let creates_datadir = dd.kind != "ours";
    if dd.kind == "other" && !move_aside {
        return Err(dd.message);
    }

    set(&p, |s| s.stage = "release".into());
    let tag = mgr.latest_release(true).await?;
    set(&p, |s| s.tag = Some(tag.clone()));

    let tmp = mgr.app_dir.join("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    fetch_release(mgr, &tag, &p, &tmp).await?;

    set(&p, |s| {
        s.stage = "grpcurl".into();
        s.note = None;
    });
    let grpcurl = match find_grpcurl(&mgr.app_dir, settings.grpcurl.as_deref()) {
        Some((path, why)) => {
            set(&p, |s| s.note = Some(format!("Using {}.", why)));
            path
        }
        None => {
            set(&p, |s| s.note = Some(format!("Downloading grpcurl {}.", GRPCURL_VERSION)));
            fetch_grpcurl(&mgr.http, &mgr.app_dir, &tmp).await?
        }
    };

    set(&p, |s| {
        s.stage = "config".into();
        s.note = None;
    });
    // Moved aside only now, past the last point "Cancel" can stop the install (unpack), so a
    // cancel or a failed download never leaves the folder moved.
    if dd.kind == "other" {
        let away = dd.away.clone().unwrap_or_default();
        std::fs::rename(&datadir, &away)
            .map_err(|e| format!("Couldn't move {} aside: {}", datadir.display(), e))?;
    }
    std::fs::create_dir_all(&datadir).map_err(|e| e.to_string())?;
    std::fs::write(datadir.join(DATADIR_MARK), b"").map_err(|e| e.to_string())?;
    write_conf(&datadir, tag_name)?;
    let mut s2 = mgr.settings.lock().await.clone();
    s2.installed_tag = Some(tag.clone());
    s2.grpcurl = Some(grpcurl.to_string_lossy().into_owned());
    if creates_datadir {
        s2.datadir_created = Some(settings.datadir.clone());
    }
    mgr.save_settings(s2).await?;
    let _ = std::fs::remove_dir_all(&tmp);

    set(&p, |s| s.stage = "start".into());
    super::process::start(mgr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert!(validate_tag("freebank-ab2c").is_ok());
        assert!(validate_tag("Jo's pool, Leeds").is_ok());
        assert!(validate_tag("").is_err());
        assert!(validate_tag(&"x".repeat(65)).is_err());
        assert!(validate_tag("a#b").is_err());
        assert!(validate_tag("\"quoted\"").is_err());
        assert!(validate_tag(" lead").is_err());
        assert!(validate_tag("caf\u{e9}").is_err());
        assert!(validate_tag(SEED_TAG).is_err());
        let t = suggest_tag();
        assert_eq!(t.len(), 13);
        assert!(validate_tag(&t).is_ok());
    }

    #[test]
    fn sums() {
        let s = "abc123  freebank-0.2.15-x86_64-linux-gnu.tar.gz\nDEF *other.tar.gz\n";
        assert_eq!(listed_hash(s, "freebank-0.2.15-x86_64-linux-gnu.tar.gz").as_deref(), Some("abc123"));
        assert_eq!(listed_hash(s, "other.tar.gz").as_deref(), Some("def"));
        assert_eq!(listed_hash(s, "missing"), None);
    }

    #[test]
    fn sums_must_be_signed() {
        let sums = include_bytes!("../../testdata/v0.2.16/SHA256SUMS");
        let sig = include_bytes!("../../testdata/v0.2.16/SHA256SUMS.sig");
        let other = include_bytes!("../../testdata/other-key/SHA256SUMS.sig");

        let text = signed_sums("v0.2.16", sums, Some(sig)).unwrap();
        assert_eq!(
            listed_hash(text, "freebank-0.2.16-x86_64-linux-gnu.tar.gz").as_deref(),
            Some("b19da93fcf2ea3195e90ea441acbdd16f669f43fdecc225721ddc48d152ee253")
        );

        assert_eq!(
            signed_sums("v0.2.15", sums, None).unwrap_err(),
            "FreeBank v0.2.15 isn't signed, so it wasn't installed."
        );
        let bad = signed_sums("v0.2.16", sums, Some(other)).unwrap_err();
        assert!(bad.starts_with("The release's signature didn't check out, so nothing was installed"), "{}", bad);
        let mut changed = sums.to_vec();
        changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
        assert!(signed_sums("v0.2.16", &changed, Some(sig)).is_err());
    }

    /// A local stand-in for a release's download folder: serves `files` by path (404 for anything
    /// else) and records every path asked for. One request per connection.
    fn serve(files: Vec<(String, Vec<u8>)>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let mut header = String::new();
                while reader.read_line(&mut header).map(|n| n > 2).unwrap_or(false) {
                    header.clear();
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                    Some((_, b)) => ("200 OK", b.clone()),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status,
                    body.len()
                );
                let _ = conn.write_all(head.as_bytes()).and_then(|_| conn.write_all(&body));
            }
        });
        (url, asked)
    }

    /// fetch_checked against a release with the given SHA256SUMS.sig (None: the release has none)
    /// and archive bytes. Returns the result, the stage it stopped in, and the paths it asked for.
    async fn fetch_from(sig: Option<&[u8]>, asset: &str, archive: &[u8]) -> (Result<String, String>, String, Vec<String>) {
        let sums = include_bytes!("../../testdata/v0.2.16/SHA256SUMS");
        let mut files = vec![
            ("/v0.2.16/SHA256SUMS".to_string(), sums.to_vec()),
            (format!("/v0.2.16/{}", asset), archive.to_vec()),
        ];
        if let Some(sig) = sig {
            files.push(("/v0.2.16/SHA256SUMS.sig".to_string(), sig.to_vec()));
        }
        let (url, asked) = serve(files);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let d = std::env::temp_dir().join(format!("fbfetch-{}-{}", std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        let p = Arc::new(std::sync::Mutex::new(InstallProgress::default()));
        let base = format!("{}/v0.2.16", url);
        let r = fetch_checked(&http, &base, "v0.2.16", asset, &d.join(asset), &p).await;
        std::fs::remove_dir_all(&d).unwrap();
        let stage = p.lock().unwrap().stage.clone();
        let asked = asked.lock().unwrap().clone();
        (r, stage, asked)
    }

    #[tokio::test]
    async fn signature_checked_before_the_download() {
        let asset = "freebank-0.2.16-x86_64-linux-gnu.tar.gz";
        let archive_path = format!("/v0.2.16/{}", asset);
        let sig = include_bytes!("../../testdata/v0.2.16/SHA256SUMS.sig");
        let other = include_bytes!("../../testdata/other-key/SHA256SUMS.sig");

        // No SHA256SUMS.sig: refused in the signature stage, and the archive is never asked for.
        let (r, stage, asked) = fetch_from(None, asset, b"archive").await;
        assert_eq!(r.unwrap_err(), "FreeBank v0.2.16 isn't signed, so it wasn't installed.");
        assert_eq!(stage, "signature");
        assert!(!asked.contains(&archive_path), "{:?}", asked);

        // Signed with another key: the same.
        let (r, stage, asked) = fetch_from(Some(other), asset, b"archive").await;
        let e = r.unwrap_err();
        assert!(e.starts_with("The release's signature didn't check out"), "{}", e);
        assert_eq!(stage, "signature");
        assert!(!asked.contains(&archive_path), "{:?}", asked);

        // Well signed, but this build isn't on the list: nothing downloaded either.
        let (r, _, asked) = fetch_from(Some(sig), "freebank-0.2.16-riscv64-linux-gnu.tar.gz", b"archive").await;
        assert!(r.unwrap_err().contains("isn't in the release's signed checksums"));
        assert!(!asked.iter().any(|a| a.ends_with(".tar.gz")), "{:?}", asked);

        // Well signed: the archive is downloaded after the signature files, then checked against it.
        let (r, stage, asked) = fetch_from(Some(sig), asset, b"not the release").await;
        let e = r.unwrap_err();
        assert!(
            e.starts_with("The download didn't match its signed checksums, so nothing was installed. Please try again."),
            "{}",
            e
        );
        assert_eq!(stage, "verify");
        assert_eq!(asked, ["/v0.2.16/SHA256SUMS", "/v0.2.16/SHA256SUMS.sig", archive_path.as_str()]);
    }

    #[cfg(unix)]
    #[test]
    fn only_checked_releases_are_reused() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("fbverified-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mgr = NodeManager::new(d.clone());
        assert!(!already_verified(&mgr, "v0.2.16"));

        // A freebankd that runs, unpacked by something other than this code (no marker): never reused.
        let bin = mgr.freebankd("v0.2.16");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!already_verified(&mgr, "v0.2.16"));

        // With the marker fetch_release writes after the checks, it is.
        std::fs::write(mgr.release_dir("v0.2.16").join(VERIFIED), "b19da93f\n").unwrap();
        assert!(already_verified(&mgr, "v0.2.16"));
        // Marked but gone or broken: downloaded again.
        std::fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(!already_verified(&mgr, "v0.2.16"));
        std::fs::remove_file(&bin).unwrap();
        assert!(!already_verified(&mgr, "v0.2.16"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn conf_keeps_other_lines() {
        let d = std::env::temp_dir().join(format!("fbconf-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("freebank.conf"), "rpcport=9000\ncoinbasetag=old\n").unwrap();
        write_conf(&d, "new name").unwrap();
        let c = std::fs::read_to_string(d.join("freebank.conf")).unwrap();
        assert_eq!(c, "rpcport=9000\ncoinbasetag=new name\n");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
