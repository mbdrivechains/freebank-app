//! Installing freebankd: pick the newest C++ release, download and verify it against SHA256SUMS,
//! unpack it into the app's data folder, find or fetch grpcurl, and write freebank.conf.

use super::{
    detect, home, platform, NodeManager, DATADIR_MARK, GRPCURL_VERSION, RELEASES_URL,
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
/// release, download, verify, unpack, grpcurl, config, start.
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

/// Download `tag` for this machine, check it against SHA256SUMS and unpack it into
/// releases/<tag>/. Skipped when that release is already there and runs.
async fn fetch_release(
    mgr: &NodeManager,
    tag: &str,
    p: &Arc<std::sync::Mutex<InstallProgress>>,
    tmp: &Path,
) -> Result<(), String> {
    let (triplet, _) = platform()?;
    let bin = mgr.freebankd(tag);
    if bin.is_file() && runs(&bin) {
        set(p, |s| {
            s.stage = "unpack".into();
            s.note = Some(format!("FreeBank {} is already downloaded.", tag));
        });
        return Ok(());
    }
    let version = tag.trim_start_matches('v');
    let asset = format!("freebank-{}-{}.tar.gz", version, triplet);
    let base = format!("{}/{}", RELEASE_DOWNLOAD, tag);
    let archive = tmp.join(&asset);

    set(p, |s| {
        s.stage = "download".into();
        s.note = Some(asset.clone());
    });
    let prog = p.clone();
    download(&mgr.http, &format!("{}/{}", base, asset), &archive, move |got, total| {
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
    let sums = download_text(&mgr.http, &format!("{}/SHA256SUMS", base)).await?;
    let want = listed_hash(&sums, &asset).ok_or(format!("{} is not listed in SHA256SUMS.", asset))?;
    let got = sha256_file(&archive)?;
    if want != got {
        return Err(format!(
            "The download didn't match SHA256SUMS, so nothing was installed. Please try again. (want {}, got {})",
            want, got
        ));
    }

    set(p, |s| s.stage = "unpack".into());
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
    Ok(())
}

/// Update, run as a background task; the Node tab polls `mgr.update`.
/// Stages: release, download, verify, unpack, stop, start. The old release folder is kept.
pub async fn run_update(mgr: Arc<NodeManager>) {
    let p = mgr.update.clone();
    let result = update(&mgr).await;
    set(&p, |s| {
        s.running = false;
        match result {
            Ok(()) => s.done = true,
            Err(e) => s.error = Some(e),
        }
    });
}

async fn update(mgr: &Arc<NodeManager>) -> Result<(), String> {
    let p = mgr.update.clone();
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
    platform()?;
    validate_tag(tag_name)?;
    let settings = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&settings.datadir);

    // Settle the data folder before anything is downloaded, so a refusal costs nothing.
    let dd = detect::check_datadir(&datadir);
    if dd.kind == "other" {
        if !move_aside {
            return Err(dd.message);
        }
        let away = dd.away.clone().unwrap_or_default();
        std::fs::rename(&datadir, &away)
            .map_err(|e| format!("Couldn't move {} aside: {}", datadir.display(), e))?;
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
    std::fs::create_dir_all(&datadir).map_err(|e| e.to_string())?;
    std::fs::write(datadir.join(DATADIR_MARK), b"").map_err(|e| e.to_string())?;
    write_conf(&datadir, tag_name)?;
    let mut s2 = mgr.settings.lock().await.clone();
    s2.installed_tag = Some(tag.clone());
    s2.grpcurl = Some(grpcurl.to_string_lossy().into_owned());
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
