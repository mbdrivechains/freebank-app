//! "Obliterate": remove everything FreeBank put on this computer. The list of what that is, with
//! paths and sizes, is always worked out here: `execute` acts only on the ticked items of a fresh
//! plan, never on paths from the screen. The screen sends back each tick with the path it showed,
//! and a tick whose item now names another path is refused. A node data folder is ticked for you
//! only when the app created it and its mark is still there, because freebankd's default folder
//! may hold a node run by hand; folders FreeBank moved aside (during setup or a restore) never are,
//! and only the ones it recorded are called that. A folder a node is using (its lock is held) can't be ticked, whatever
//! port that node answers on. The eCash node, the enforcer and BitWindow are never on the list.
//! What the running screen still uses (the app's folder on Linux, the caches) goes when the app exits.

use super::{detect, lock, process, wallet_files, wallets_inside, NodeManager, DATADIR_MARK};

/// A recorded moved-aside folder: setup moves an older data folder aside, and a restore the wallet it
/// replaces (recovery/job.rs); both are recorded in Settings::moved_aside.
pub const ASIDE_LABEL: &str = "A folder FreeBank moved aside (during setup or a restore)";
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// FreeBank's own files in the app's folder. They go at once, even when the folder waits for exit.
/// v0.2.0 added: `wallet/` (the recovery words, encrypted), `sends.json` (the send log, written
/// through `.sends.json.tmp`), `phone/` (the phone link: this computer's key, paired phones, held
/// sends and the phone send log), `backups.json` (the wallet backups the app made, for Settings >
/// Security) and `node.pid` (the node the app started, written through `node.pid.new`).
const APP_FILES: &[&str] = &[
    "settings.json", "releases", "tools", "tmp", "logs", "wallet", "sends.json", ".sends.json.tmp", "phone",
    "backups.json", "node.pid", "node.pid.new",
];

/// FreeBank's own files named by how they begin: a damaged send log moved aside
/// (`sends.json.damaged-<time>`).
const APP_FILE_PREFIXES: &[&str] = &["sends.json.damaged-"];

/// FreeBank's own files in the app's folder that exist now (APP_FILES and APP_FILE_PREFIXES).
fn app_files(app_dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = APP_FILES.iter().map(|n| app_dir.join(n)).filter(|p| exists(p)).collect();
    let mut named: Vec<PathBuf> = std::fs::read_dir(app_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            APP_FILE_PREFIXES.iter().any(|p| n.starts_with(p))
        })
        .map(|e| e.path())
        .collect();
    named.sort();
    found.extend(named);
    found
}

/// The app's copy of the recovery words, encrypted with the wallet passphrase.
pub const SEED_FILE: &str = "wallet/seed.enc";

/// A wallet backup: the wallet file it copies, and where the copy is. A backup covers only its own
/// wallet, so one backup never silences the warning for another.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Backup {
    pub wallet: String,
    pub saved: String,
}

/// What WebKitGTK keeps in the folders it names after the program ("freebank") rather than the
/// app's identifier: its cache and HSTS list. Such a folder is listed only if it holds nothing else.
const WEBKIT_NAMES: &[&str] = &[
    "WebKitCache", "CacheStorage", "hsts-storage.sqlite", "hsts-storage.sqlite-journal",
    "hsts-storage.sqlite-wal", "hsts-storage.sqlite-shm", "mediakeys", "storage", "databases",
    "localstorage", "indexeddb", "serviceworkers", "cookies", "deviceidhashsalts", "itp",
];

/// A folder or file the screen (the system webview) may have written.
#[derive(Debug, Clone)]
pub struct Cache {
    pub id: &'static str,
    pub path: PathBuf,
    /// Named after the program, not the app's identifier: listed only when it holds nothing but
    /// WebKit's files.
    pub program_named: bool,
}

/// Everywhere the plan looks. The app fills this from Tauri's path API; tests pass temp folders.
#[derive(Debug, Clone)]
pub struct Places {
    pub home: PathBuf,
    /// The app's own folder: settings.json, releases/, tools/, tmp/, logs/.
    pub app_dir: PathBuf,
    /// The screen keeps its own data in app_dir (WebKitGTK on Linux), so that folder waits for exit.
    pub screen_uses_app_dir: bool,
    /// app_dir was chosen with FREEBANK_APP_DIR and may hold other things: only FreeBank's own
    /// files in it go, never the folder.
    pub app_dir_shared: bool,
    pub datadir: PathBuf,
    /// Every node folder the app created when it installed (Settings::created).
    pub created: Vec<PathBuf>,
    /// The folders FreeBank moved aside, during setup or a restore (Settings::moved_aside).
    pub moved_aside: Vec<PathBuf>,
    pub caches: Vec<Cache>,
    /// What must survive whatever is ticked: wallet backups made from the plan.
    pub keep: Vec<PathBuf>,
    /// The process ids of the node the app runs: its locks don't make a folder "in use", because
    /// Obliterate stops that node first.
    pub ours: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    App,
    /// The node's data folder (the one the settings name).
    Node,
    /// Another node data folder the app created, before a switch to another folder under Advanced.
    Earlier,
    Aside,
    Cache,
}

/// One line of the list.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// "app", "node", "earlier:<path>", "aside:<path>" or "cache:<which>".
    pub id: String,
    pub kind: Kind,
    pub label: String,
    pub path: String,
    /// Bytes, links not followed.
    pub size: u64,
    /// Ticked when the list opens.
    pub checked: bool,
    /// Can be ticked at all.
    pub allowed: bool,
    pub note: String,
    /// The wallets deleting it would delete (node folders and folders FreeBank moved aside). None for a
    /// link, which goes alone.
    pub wallets: Vec<String>,
}

/// A ticked line as the screen showed it: its id and the path it named then.
#[derive(Debug, Clone, Deserialize)]
pub struct Tick {
    pub id: String,
    pub path: String,
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Bytes under `path`, never following links (a link counts as nothing).
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| size_of(&e.path()))
        .sum()
}

/// Where `path` really is: its folder resolved, the last part not followed, so a link stays a link.
fn real_location(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = path.parent()?.canonicalize().ok()?;
    Some(parent.join(name))
}

/// The guard every deletion passes: never a relative path, "/", the home folder or anything above
/// it, nor a link that leads there. Returns where the entry really is (see `real_location`).
pub fn check_deletable(path: &Path, home: &Path) -> Result<PathBuf, String> {
    let refuse = |why: &str| Err(format!("FreeBank never deletes {}: {}.", path.display(), why));
    if !path.is_absolute() {
        return refuse("it isn't a full path");
    }
    let Some(full) = real_location(path) else {
        return refuse("it can't tell where that is");
    };
    let Ok(home_real) = home.canonicalize() else {
        return refuse("it can't find your home folder");
    };
    let above_home = |p: &Path| home_real.starts_with(p) || home.starts_with(p);
    if above_home(&full) || above_home(path) || full.canonicalize().is_ok_and(|c| above_home(&c)) {
        return refuse("it holds your home folder");
    }
    Ok(full)
}

/// Would deleting `dir` (a real location) take `other` with it? Checked both where `other` is and
/// where it leads, so a link into `dir` counts.
fn holds(dir: &Path, other: &Path) -> bool {
    let inside = |p: Option<PathBuf>| p.map(|p| p.starts_with(dir)).unwrap_or(false);
    inside(real_location(other)) || inside(other.canonicalize().ok())
}

/// Delete one planned item: a folder with everything in it, or a file or link (never followed).
/// Returns false when it was already gone.
fn remove_item(path: &Path, home: &Path) -> Result<bool, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("Couldn't look at {}: {}", path.display(), e)),
    };
    let full = check_deletable(path, home)?;
    // symlink_metadata never calls a link a folder, so a link is removed as a file: only the link.
    if meta.is_dir() {
        std::fs::remove_dir_all(&full)
    } else {
        std::fs::remove_file(&full)
    }
    .map_err(|e| format!("Couldn't remove {}: {}", full.display(), e))?;
    Ok(true)
}

/// Folders named as setup names the ones it moves aside: siblings of the node's folder named
/// <name>.old-<time>[-n] (`aside_stamp`). Only those recorded in the settings were moved by setup.
fn look_alikes(datadir: &Path) -> Vec<(String, PathBuf)> {
    let (Some(parent), Some(name)) = (datadir.parent(), datadir.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.old-", name.to_string_lossy());
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            super::aside_stamp(n.strip_prefix(&prefix)?).then(|| (n.clone(), e.path()))
        })
        .collect();
    found.sort();
    found
}

/// A folder named after the program that holds nothing but WebKit's files.
fn only_webkit(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.filter_map(|e| e.ok())
        .all(|e| WEBKIT_NAMES.contains(&e.file_name().to_string_lossy().as_ref()))
}

/// One line of the list, with its size, a note on links, and never tickable if the guard says no.
fn entry(p: &Places, id: String, kind: Kind, label: &str, path: &Path, on: bool, allowed: bool, note: &str) -> Item {
    // A path the guard refuses is never walked for its size: it may be "/" or the home folder.
    let (allowed, mut note, size) = match check_deletable(path, &p.home) {
        Ok(_) => (allowed, note.to_string(), size_of(path)),
        Err(why) => (false, why, 0),
    };
    if let Ok(target) = std::fs::read_link(path) {
        note = format!(
            "{} This is a link to {}: only the link goes, not what it points to.",
            note,
            target.display()
        )
        .trim()
        .to_string();
    }
    Item {
        id,
        kind,
        label: label.to_string(),
        path: path.to_string_lossy().into_owned(),
        size,
        checked: on && allowed,
        allowed,
        note,
        wallets: Vec::new(),
    }
}

/// The wallets that deleting `dir` would delete: its node's wallets that lie inside it (a walletdir
/// elsewhere stays), none for a link.
fn wallet_strings(dir: &Path) -> Vec<String> {
    wallets_inside(dir, &wallet_files(dir))
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect()
}

/// A folder a node other than the app's own is using (its lock is held) can't be ticked.
fn mark_in_use(item: &mut Item, p: &Places) {
    if let Some(u) = lock::in_use(Path::new(&item.path), &p.ours) {
        item.allowed = false;
        item.checked = false;
        item.note = format!("{} Stop that node first; until then this folder stays.", u.say());
    }
}

/// The list's name for a folder the screen (the system webview) wrote, one per kind so no two lines
/// read the same.
fn cache_label(id: &str) -> &'static str {
    match id {
        "cache" => "App cache",
        "local-data" => "App local data",
        "webkit-cache" | "caches" => "Window cache",
        "webkit-data" | "webkit" => "Window data",
        "http-storage" => "Window web storage",
        "cookies" => "Window cookies",
        "saved-state" => "Saved window position",
        _ => "Window files",
    }
}

/// What of FreeBank's own is in its folder, as the list says it: "Settings, the node program and
/// logs". grpcurl counts only when it is FreeBank's own copy (tools/), not one found elsewhere.
fn app_contents(app_dir: &Path) -> String {
    let damaged_log = app_files(app_dir)
        .iter()
        .any(|f| f.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sends.json.damaged-")));
    let parts: Vec<&str> = [
        ("settings.json", "settings"),
        ("releases", "the node program"),
        ("tools/grpcurl", "grpcurl"),
        ("wallet", "your recovery words (encrypted)"),
        ("sends.json", "the record of your sends"),
        ("phone", "the phone link (this computer's key, your paired phones, held sends and the phone's send log)"),
        ("backups.json", "the list of wallet backups FreeBank made (the backups themselves stay)"),
        ("logs", "logs"),
        ("tmp", "an unfinished download"),
    ]
    .iter()
    .filter(|(name, _)| exists(&app_dir.join(name)) || (*name == "sends.json" && damaged_log))
    .map(|(_, what)| *what)
    .collect();
    let text = match parts.as_slice() {
        [] => return String::new(),
        [one] => one.to_string(),
        [first @ .., last] => format!("{} and {}", first.join(", "), last),
    };
    let mut c = text.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Everything FreeBank put on this computer that still exists, one item each.
pub fn plan_items(p: &Places) -> Vec<Item> {
    let mut items = Vec::new();
    if exists(&p.app_dir) {
        let what = app_contents(&p.app_dir);
        let note = match (what.is_empty(), p.app_dir_shared, p.screen_uses_app_dir) {
            (false, true, _) => format!(
                "{}. This folder was set with FREEBANK_APP_DIR, so it stays; only FreeBank's own files in it go.",
                what
            ),
            (true, true, _) => {
                "Nothing of FreeBank's own is left in it. This folder was set with FREEBANK_APP_DIR, so it stays.".into()
            }
            (false, false, true) => format!("{}, and the app's window cache, which is removed when FreeBank closes.", what),
            (true, false, true) => "The app's window cache, which is removed when FreeBank closes.".into(),
            (false, false, false) => format!("{}.", what),
            (true, false, false) => "None of its settings or programs are left in it.".into(),
        };
        let mut app = entry(p, "app".into(), Kind::App, "FreeBank's own folder", &p.app_dir, true, true, &note);
        if p.app_dir_shared && app.allowed {
            app.size = app_files(&p.app_dir).iter().map(|f| size_of(f)).sum();
        }
        items.push(app);
    }
    if exists(&p.datadir) {
        let recorded = p.created.contains(&p.datadir);
        let marked = p.datadir.join(DATADIR_MARK).exists();
        // The record alone isn't enough: after the folder was deleted, another program (BitWindow's
        // FreeBank uses the same default) may have made a new one at the same path, without the mark.
        let created = recorded && marked;
        let note = if created {
            "Chain data, your wallet and freebank.conf. FreeBank created this folder."
        } else if marked {
            "FreeBank has run a node in this folder but has no record of creating it, so it may also be a node you run \
             yourself (BitWindow's FreeBank uses the same folder). Tick it to delete it."
        } else if recorded {
            "FreeBank created a folder here once, but its mark (.freebank-node) is gone, so another program may have made this one. It stays."
        } else {
            "FreeBank didn't create or set up this folder; it may hold a node you run yourself, so it stays."
        };
        let mut node = entry(p, "node".into(), Kind::Node, "The node's data folder", &p.datadir, created, marked, note);
        node.wallets = wallet_strings(&p.datadir);
        mark_in_use(&mut node, p);
        items.push(node);
    }
    // Node folders the app created before a switch to another folder under Advanced.
    for dir in &p.created {
        if *dir == p.datadir || !exists(dir) {
            continue;
        }
        let marked = dir.join(DATADIR_MARK).exists();
        let note = if marked {
            "Chain data, a wallet and freebank.conf. FreeBank created this folder for an earlier install."
        } else {
            "FreeBank created a folder here once, but its mark (.freebank-node) is gone, so another program may have made this one. It stays."
        };
        let id = format!("earlier:{}", dir.display());
        let mut item = entry(p, id, Kind::Earlier, "An earlier node data folder", dir, marked, marked, note);
        item.wallets = wallet_strings(dir);
        mark_in_use(&mut item, p);
        items.push(item);
    }
    for path in &p.moved_aside {
        if !exists(path) {
            continue;
        }
        let wallets = wallet_strings(path);
        let note = if wallets.is_empty() {
            "FreeBank moved this here during setup or a restore and promised not to delete it. No wallet was \
             found in it."
        } else {
            "FreeBank moved this here during setup or a restore and promised not to delete it. It holds an \
             older wallet."
        };
        let id = format!("aside:{}", path.display());
        let mut aside = entry(p, id, Kind::Aside, ASIDE_LABEL, path, false, true, note);
        aside.wallets = wallets;
        mark_in_use(&mut aside, p);
        items.push(aside);
    }
    // Named like those, but not recorded (an older app, or someone else): shown, never removed here.
    for (_, path) in look_alikes(&p.datadir) {
        if p.moved_aside.contains(&path) {
            continue;
        }
        let id = format!("aside:{}", path.display());
        let note = "Named like a folder FreeBank moves aside, but FreeBank has no record of moving it, so it stays.";
        let mut item = entry(p, id, Kind::Aside, "Older node folder", &path, false, false, note);
        item.wallets = wallet_strings(&path);
        items.push(item);
    }
    let mut seen: Vec<PathBuf> = items.iter().map(|i| PathBuf::from(&i.path)).collect();
    for c in &p.caches {
        // Inside something already listed, or holding it (then it isn't a cache): leave it out.
        let overlaps = seen.iter().any(|s| c.path.starts_with(s) || s.starts_with(&c.path));
        // A folder with FreeBank's settings or programs in it is an app folder, not a cache.
        let app_like = c.path.join("settings.json").exists() || c.path.join("releases").exists();
        if !exists(&c.path) || overlaps || app_like || (c.program_named && !only_webkit(&c.path)) {
            continue;
        }
        seen.push(c.path.clone());
        items.push(entry(
            p,
            format!("cache:{}", c.id),
            Kind::Cache,
            cache_label(c.id),
            &c.path,
            true,
            true,
            "What the app's window stored. It is removed when FreeBank closes.",
        ));
    }
    items
}

/// Check the ticks against a fresh plan: each must be on it with the path the screen showed, be
/// allowed, pass the guard, and hold nothing that stays. Returns the plan and the chosen items;
/// nothing is deleted.
fn choose(p: &Places, ticks: &[Tick]) -> Result<(Vec<Item>, Vec<Item>), String> {
    let plan = plan_items(p);
    if ticks.is_empty() {
        return Err("Nothing is ticked, so nothing was removed.".into());
    }
    let mut chosen: Vec<Item> = Vec::new();
    for t in ticks {
        // "node" follows the data folder in settings, so the id alone could now name a folder the
        // user never saw on the list.
        let item = plan.iter().find(|i| i.id == t.id && i.path == t.path).ok_or(
            "The list has changed since it was shown, so nothing was removed. Please look at it again.",
        )?;
        if !item.allowed {
            return Err(format!("{} can't be removed here, so nothing was removed.", item.path));
        }
        if !chosen.iter().any(|c| c.id == item.id) {
            chosen.push(item.clone());
        }
    }
    let staying: Vec<PathBuf> = plan
        .iter()
        .filter(|i| !chosen.iter().any(|c| c.id == i.id))
        .map(|i| PathBuf::from(&i.path))
        .chain(p.keep.iter().cloned())
        .collect();
    for item in &chosen {
        let path = Path::new(&item.path);
        let full = check_deletable(path, &p.home)?;
        if matches!(item.kind, Kind::Node | Kind::Earlier) && !path.join(DATADIR_MARK).exists() {
            return Err(format!(
                "{} wasn't set up by FreeBank, so nothing was removed.",
                item.path
            ));
        }
        if let Some(s) = staying.iter().find(|s| holds(&full, s)) {
            return Err(format!(
                "{} holds {}, which isn't ticked, so nothing was removed.",
                item.path,
                s.display()
            ));
        }
    }
    Ok((plan, chosen))
}

/// What `execute` did.
#[derive(Debug, Default)]
pub struct Done {
    pub removed: Vec<PathBuf>,
    /// Still used by the screen: deleted when the app exits.
    pub at_exit: Vec<PathBuf>,
    /// Of `at_exit`, the folders named after the program: deleted only if they still hold nothing
    /// but WebKit's files then.
    pub webkit_only: Vec<PathBuf>,
    /// On the list but not ticked.
    pub kept: Vec<PathBuf>,
    /// The app's own folder is going, so nothing may be written to it again.
    pub app_removed: bool,
}

/// Delete the ticked items of a fresh plan. The node folders and the moved-aside ones go first, so
/// a failure there leaves the app's settings as they were. Called once the app's node has stopped:
/// then no node at all may be using a folder that goes.
pub fn execute(p: &Places, ticks: &[Tick]) -> Result<Done, String> {
    let (plan, chosen) = choose(p, ticks)?;
    let mut done = Done::default();
    let folders: Vec<&Item> = chosen
        .iter()
        .filter(|i| matches!(i.kind, Kind::Node | Kind::Earlier | Kind::Aside))
        .collect();
    for item in &folders {
        if let Some(u) = lock::in_use(Path::new(&item.path), &[]) {
            return Err(format!("{} Stop it first. Nothing was removed.", u.say()));
        }
    }
    // Where each link that goes leads: that stays, and the list says so.
    let mut targets = Vec::new();
    for item in &folders {
        let path = Path::new(&item.path);
        if is_link(path) {
            targets.extend(path.canonicalize().ok());
        }
        remove_item(path, &p.home)?;
        done.removed.push(PathBuf::from(&item.path));
    }
    if chosen.iter().any(|i| i.kind == Kind::App) {
        if p.app_dir_shared {
            // A developer's FREEBANK_APP_DIR: FreeBank's own files go, the folder and the rest stay.
            for path in app_files(&p.app_dir) {
                if super::remove_inside(&p.app_dir, &path)? {
                    done.removed.push(path);
                }
            }
        } else if !p.screen_uses_app_dir {
            remove_item(&p.app_dir, &p.home)?;
            done.removed.push(p.app_dir.clone());
        } else {
            // A link goes whole at exit; only a real folder is emptied of FreeBank's files now.
            if !is_link(&p.app_dir) {
                for path in app_files(&p.app_dir) {
                    super::remove_inside(&p.app_dir, &path)?;
                }
            }
            done.at_exit.push(p.app_dir.clone());
        }
        done.app_removed = true;
    }
    for item in chosen.iter().filter(|i| i.kind == Kind::Cache) {
        let path = PathBuf::from(&item.path);
        if p.caches.iter().any(|c| c.program_named && item.id == format!("cache:{}", c.id)) {
            done.webkit_only.push(path.clone());
        }
        done.at_exit.push(path);
    }
    done.kept = plan
        .iter()
        .filter(|i| !chosen.iter().any(|c| c.id == i.id))
        .map(|i| PathBuf::from(&i.path))
        .chain(targets)
        .collect();
    Ok(done)
}

/// What `execute` left for the app's exit.
#[derive(Debug, Default)]
pub struct AtExit {
    pub home: PathBuf,
    pub paths: Vec<PathBuf>,
    /// Of `paths`, the folders named after the program (see `Done::webkit_only`).
    pub webkit_only: Vec<PathBuf>,
}

/// Delete what waited for exit, through the same guard. Returns what couldn't be removed.
pub fn wipe(later: &AtExit) -> Vec<String> {
    later
        .paths
        .iter()
        .filter_map(|p| {
            // The app may have stayed open for days. A folder named after the program can meanwhile
            // have become another program's (BitWindow's Rust FreeBank keeps its data in
            // ~/.local/share/freebank), so it is looked at again and left if anything else is in it.
            if later.webkit_only.contains(p) && exists(p) && !only_webkit(p) {
                return Some(format!(
                    "Left {}: something other than the screen's files is in it now.",
                    p.display()
                ));
            }
            remove_item(p, &later.home).err()
        })
        .collect()
}

/// Called from RunEvent::Exit, after the node has stopped.
pub fn wipe_at_exit(mgr: &NodeManager) {
    let later = std::mem::take(&mut *mgr.at_exit.lock().unwrap());
    for e in wipe(&later) {
        eprintln!("{}", e);
    }
}

/// The list the confirm panel shows, with the wallet's balance when it can be trusted.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    /// The wallets of the node whose data folder the settings name.
    pub wallets: Vec<String>,
    /// Everything their wallet holds, from getwalletinfo: spendable, unconfirmed and newly mined
    /// coins still maturing. (getbalance, the number the Home screen shows, counts only the first.)
    /// Given only when it is the whole story: exactly one wallet, and the node caught up.
    pub balance: Option<f64>,
    /// How much of `balance` isn't spendable yet (unconfirmed or maturing), when any.
    pub pending: Option<f64>,
    /// Why there is no balance.
    pub balance_note: Option<String>,
    /// Backups made since the app started, each with the wallet it copies.
    pub backups: Vec<Backup>,
    /// What backing up will do beyond copying (stop the node for a moment), or why it can't.
    pub backup_note: Option<String>,
    /// The app's copy of the recovery words (encrypted), when there is one: it goes with the app's
    /// own folder.
    pub seed: Option<String>,
    /// Why Obliterate can't run right now.
    pub blocked: Option<String>,
}

/// Why Obliterate must wait: an install or update under way, or a node another program started
/// (its folder may be in use, and the app never stops it).
async fn refusal(mgr: &NodeManager) -> Option<String> {
    let installing = mgr.install.lock().unwrap().running;
    let updating = mgr.update.lock().unwrap().running;
    if installing {
        return Some("FreeBank is being installed. Let it finish, or cancel it, first.".into());
    }
    if updating {
        return Some("FreeBank is being updated. Let it finish first.".into());
    }
    if process::someone_elses_node(mgr).await {
        return Some("A FreeBank node started by another program is running. Stop it there first.".into());
    }
    None
}

/// From getwalletinfo: everything the wallet holds, and how much of it isn't spendable yet
/// (unconfirmed, or newly mined and still maturing).
fn holdings(info: &serde_json::Value) -> Option<(f64, f64)> {
    let spendable = info["balance"].as_f64()?;
    let waiting = info["unconfirmed_balance"].as_f64().unwrap_or(0.0) + info["immature_balance"].as_f64().unwrap_or(0.0);
    Some((spendable + waiting, waiting))
}

/// Has the node caught up, so the balance it reports is all the wallet holds? While it syncs (after
/// "Delete chain data", say) a funded wallet reads 0. Caught up: as many blocks as headers, out of
/// initial block download, and not behind the explorer's tip when the app has seen it lately.
fn caught_up(info: &serde_json::Value, explorer_tip: Option<u64>) -> Result<(), String> {
    let blocks = info["blocks"].as_u64();
    let headers = info["headers"].as_u64();
    let ibd = info["initialblockdownload"].as_bool();
    let (Some(b), Some(h), Some(ibd)) = (blocks, headers, ibd) else {
        return Err("Your node didn't say how far it has synced, so the balance it reports may be missing coins.".into());
    };
    let tip = explorer_tip.unwrap_or(0).max(h);
    if b == h && !ibd && b + 1 >= tip {
        return Ok(());
    }
    Err(if tip > b {
        format!(
            "Your node is still catching up (block {} of {}), so the balance it reports may be missing coins.",
            detect::grouped(b),
            detect::grouped(tip)
        )
    } else {
        "Your node is still catching up, so the balance it reports may be missing coins.".into()
    })
}

/// The balance of the node's one wallet, when it can be trusted: (balance, pending, why not).
async fn balance_verdict(mgr: &NodeManager, s: &super::Settings, wallets: usize) -> (Option<f64>, Option<f64>, Option<String>) {
    if wallets == 0 {
        return (None, None, None);
    }
    if wallets > 1 {
        return (
            None,
            None,
            Some(format!(
                "Your node has {} wallets and reports the balance of only one, so FreeBank can't say what they hold.",
                wallets
            )),
        );
    }
    let probe = detect::probe(&mgr.http, s).await;
    match probe.state {
        detect::RpcState::Up => {
            let c = detect::local_client(&mgr.http, s);
            let chain = match c.call("getblockchaininfo", vec![]).await {
                Ok(v) => v,
                Err(e) => return (None, None, Some(format!("Your node couldn't say how far it has synced ({}).", e))),
            };
            if let Err(why) = caught_up(&chain, mgr.explorer_tip_seen()) {
                return (None, None, Some(why));
            }
            match c.call("getwalletinfo", vec![]).await {
                Ok(v) => match holdings(&v) {
                    Some((total, waiting)) => (Some(total), (waiting > 0.0).then_some(waiting), None),
                    None => (None, None, Some("Your node didn't say what the balance is.".to_string())),
                },
                Err(e) => (None, None, Some(format!("Your node couldn't say what the balance is ({}).", e))),
            }
        }
        detect::RpcState::Down => (None, None, Some("Your node isn't running, so the balance can't be shown.".into())),
        detect::RpcState::Warming | detect::RpcState::Busy => {
            (None, None, Some("Your node is still starting, so the balance can't be shown yet.".into()))
        }
        detect::RpcState::Locked => (None, None, Some(probe.message)),
    }
}

/// Why backing up several wallets needs more than a copy, said before the button is pressed.
fn several_wallets(n: usize, ours: bool) -> String {
    if ours {
        format!(
            "Your node has {} wallets. To copy each one whole, FreeBank stops the node for a moment, then starts it again.",
            n
        )
    } else {
        format!(
            "Your node has {} wallets. While it runs, it can safely copy only the one it has open, and it was started \
             by another program, which FreeBank never stops. Stop it there, then back up again: FreeBank then copies \
             every wallet.",
            n
        )
    }
}

pub async fn plan(mgr: &NodeManager, places: Places) -> Result<Plan, String> {
    mgr.still_here()?;
    let s = mgr.settings.lock().await.clone();
    let activity = mgr.activity.lock().unwrap().clone();
    let blocked = match activity {
        Some(a) => Some(format!("Please wait: {}", a)),
        None => refusal(mgr).await,
    };
    let datadir = places.datadir.clone();
    let seed = places.app_dir.join(SEED_FILE);
    let seed = seed.is_file().then(|| seed.to_string_lossy().into_owned());
    let items = tokio::task::spawn_blocking(move || plan_items(&places))
        .await
        .map_err(|e| e.to_string())?;
    let wallets: Vec<String> = wallet_files(&datadir)
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect();
    let (balance, pending, balance_note) = balance_verdict(mgr, &s, wallets.len()).await;
    let backup_note = if wallets.len() > 1 {
        if process::child_alive(mgr).await {
            Some(several_wallets(wallets.len(), true))
        } else if detect::probe(&mgr.http, &s).await.state == detect::RpcState::Up {
            Some(several_wallets(wallets.len(), false))
        } else {
            None
        }
    } else {
        None
    };
    Ok(Plan {
        items,
        wallets,
        balance,
        pending,
        balance_note,
        backups: mgr.backups.lock().unwrap().clone(),
        backup_note,
        seed,
        blocked,
    })
}

/// How to remove the app itself, which it can't do while it runs.
#[derive(Debug, Serialize)]
pub struct RemoveApp {
    /// "deb" (sudo apt remove freebank), "appimage" (delete the file at `path`), "mac" (drag it
    /// to the Trash), or "other" (delete the program at `path`).
    pub kind: &'static str,
    pub path: Option<String>,
}

pub fn how_to_remove_app() -> RemoveApp {
    let exe = std::env::current_exe().ok();
    let show = |p: &Path| Some(p.to_string_lossy().into_owned());
    if cfg!(target_os = "macos") {
        let bundle = exe
            .as_deref()
            .and_then(|e| e.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")));
        return RemoveApp {
            kind: "mac",
            path: bundle.and_then(show),
        };
    }
    if let Some(image) = std::env::var_os("APPIMAGE") {
        return RemoveApp {
            kind: "appimage",
            path: show(Path::new(&image)),
        };
    }
    match exe {
        Some(e) if e.starts_with("/usr") => RemoveApp { kind: "deb", path: None },
        e => RemoveApp {
            kind: "other",
            path: e.as_deref().and_then(show),
        },
    }
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub removed: Vec<String>,
    /// Deleted when the app closes.
    pub at_exit: Vec<String>,
    /// On the list but not ticked, so kept.
    pub kept: Vec<String>,
    pub backups: Vec<String>,
    pub app_removed: bool,
    pub app: RemoveApp,
}

fn strings(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

/// "Obliterate": check the ticks, stop the app's node, delete them, and leave what the screen
/// still uses for exit. After it, nothing is written to the app's folder again.
pub async fn run(mgr: &NodeManager, places: Places, ticks: Vec<Tick>) -> Result<Outcome, String> {
    mgr.still_here()?;
    let _busy = mgr.busy("Removing everything…")?;
    if let Some(why) = refusal(mgr).await {
        return Err(format!("{} Nothing was removed.", why));
    }
    // Check the list before stopping anything, then again as it is deleted.
    let (p, t) = (places.clone(), ticks.clone());
    tokio::task::spawn_blocking(move || choose(&p, &t).map(|_| ()))
        .await
        .map_err(|e| e.to_string())??;
    process::stop(mgr).await?;
    let home = places.home.clone();
    let done = tokio::task::spawn_blocking(move || execute(&places, &ticks))
        .await
        .map_err(|e| e.to_string())??;
    if done.app_removed {
        mgr.obliterated.store(true, Ordering::SeqCst);
    }
    {
        let mut later = mgr.at_exit.lock().unwrap();
        later.home = home;
        later.paths.extend(done.at_exit.iter().cloned());
        later.webkit_only.extend(done.webkit_only.iter().cloned());
    }
    Ok(Outcome {
        removed: strings(&done.removed),
        at_exit: strings(&done.at_exit),
        kept: strings(&done.kept),
        backups: mgr.backups.lock().unwrap().iter().map(|b| b.saved.clone()).collect(),
        app_removed: done.app_removed,
        app: how_to_remove_app(),
    })
}

/// Local time as YYYYMMDD-HHMMSS, for backup names (seconds since 1970 if the clock can't say).
fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the tm we own.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            );
        }
    }
    secs.to_string()
}

/// <base>.dat in `folder`, or <base>-2.dat and so on if that is taken.
fn free_name(folder: &Path, base: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{}.dat", base) } else { format!("{}-{}.dat", base, n) };
        let p = folder.join(name);
        if !exists(&p) {
            return p;
        }
        n += 1;
    }
}

/// Fit for a file name: letters, digits, - and _ stay, anything else becomes -.
fn file_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// "wallets/savings" -> "wallets-savings": a wallet's place in the data folder, fit for a file name.
/// A wallet in a walletdir elsewhere is named after its folder and file ("mywallets-savings").
fn wallet_label(datadir: &Path, wallet: &Path) -> String {
    let rel = match wallet.strip_prefix(datadir) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => wallet
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|c| c.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    };
    file_safe(rel.strip_suffix(".dat").unwrap_or(&rel))
}

/// Only its owner may read or write `path` (0600 on Unix).
fn set_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Couldn't protect {}: {}", path.display(), e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Copy a stopped node's wallets into `folder`: the first as <base>.dat, any others with their place
/// in the data folder added. Each copy is readable only by its owner. Never overwrites a file.
fn copy_wallets(datadir: &Path, folder: &Path, base: &str) -> Result<Vec<Backup>, String> {
    let wallets = wallet_files(datadir);
    if wallets.is_empty() {
        return Err(format!("There's no wallet in {}.", datadir.display()));
    }
    let mut saved = Vec::new();
    for (i, w) in wallets.iter().enumerate() {
        let name = if i == 0 {
            base.to_string()
        } else {
            format!("{}-{}", base, wallet_label(datadir, w))
        };
        let dest = free_name(folder, &name);
        let mut from = std::fs::File::open(w).map_err(|e| format!("Couldn't read {}: {}", w.display(), e))?;
        let mut to = super::install::private_file(&dest).map_err(|e| format!("Couldn't write {}: {}", dest.display(), e))?;
        std::io::copy(&mut from, &mut to)
            .and_then(|_| to.sync_all())
            .map_err(|e| format!("Couldn't write {}: {}", dest.display(), e))?;
        saved.push(Backup {
            wallet: w.to_string_lossy().into_owned(),
            saved: dest.to_string_lossy().into_owned(),
        });
    }
    Ok(saved)
}

/// Copy the wallets of a folder no node is using (no lock held on it or its wallet folders).
async fn copy_unused(dir: &Path, folder: &Path, base: &str) -> Result<Vec<Backup>, String> {
    if let Some(u) = lock::in_use(dir, &[]) {
        return Err(format!(
            "{} FreeBank copies a wallet only when no node has it open. Stop that node, then back up again.",
            u.say()
        ));
    }
    let (d, f, b) = (dir.to_path_buf(), folder.to_path_buf(), base.to_string());
    tokio::task::spawn_blocking(move || copy_wallets(&d, &f, &b))
        .await
        .map_err(|e| e.to_string())?
}

/// The node's own wallets (see `backup_wallet`).
async fn backup_node(
    mgr: &NodeManager,
    s: &super::Settings,
    wallets: &[PathBuf],
    folder: &Path,
    base: &str,
    saved: &mut Vec<Backup>,
) -> Result<(), String> {
    let datadir = PathBuf::from(&s.datadir);
    let ours = process::child_alive(mgr).await;
    let probe = detect::probe(&mgr.http, s).await;
    match probe.state {
        // backupwallet saves only the wallet the node has open. To copy every one whole, the app
        // stops its own node for a moment and starts it again.
        detect::RpcState::Up if wallets.len() > 1 && ours => {
            let _busy = mgr.busy("Backing up your wallets: the node stops for a moment…")?;
            process::stop(mgr).await?;
            let copied = copy_unused(&datadir, folder, base).await;
            let restarted = process::start(mgr).await;
            saved.extend(copied?);
            restarted.map_err(|e| {
                format!("Your wallets are backed up, but your node didn't start again ({}). Start it on the Node tab.", e)
            })
        }
        detect::RpcState::Up if wallets.len() > 1 => Err(several_wallets(wallets.len(), false)),
        detect::RpcState::Up => {
            let dest = free_name(folder, base);
            // Made first, readable only by its owner; the node then writes into it.
            drop(super::install::private_file(&dest).map_err(|e| format!("Couldn't write {}: {}", dest.display(), e))?);
            if let Err(e) = detect::local_client(&mgr.http, s)
                .call("backupwallet", vec![serde_json::json!(dest.to_string_lossy())])
                .await
            {
                let _ = std::fs::remove_file(&dest);
                return Err(format!("Your node couldn't back up the wallet ({}).", e.trim_start_matches("RPC error: ")));
            }
            // The node's copy may carry the wallet's own permissions: put the owner-only ones back.
            set_private(&dest)?;
            if std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == 0 {
                return Err(format!("Your node said it saved the backup, but {} is empty.", dest.display()));
            }
            saved.push(Backup {
                wallet: wallets[0].to_string_lossy().into_owned(),
                saved: dest.to_string_lossy().into_owned(),
            });
            Ok(())
        }
        detect::RpcState::Down if !ours => {
            saved.extend(copy_unused(&datadir, folder, base).await?);
            Ok(())
        }
        detect::RpcState::Locked => Err(format!("{} Stop that node, then back up again.", probe.message)),
        _ => Err("Your node is still starting. Back up once it's running.".into()),
    }
}

/// Obliterate's "Back up wallet": FreeBank-wallet-<local time>.dat in `folder`, readable only by its
/// owner. A running node with one wallet writes the copy itself (RPC backupwallet), so it is whole.
/// With several, only a stopped node's files can all be copied whole: the app stops its own node for
/// a moment, copies each and starts it again; a node another program started is never stopped, and
/// the backup says why it can't. A stopped node's wallets are copied once no node holds the folder.
/// The wallets in the other folders Obliterate may delete (earlier data folders the app created,
/// and the folders FreeBank moved aside) are copied too, named after their folder. Each backup is
/// recorded with the wallet it copies.
pub async fn backup_wallet(mgr: &NodeManager, folder: &Path) -> Result<Vec<String>, String> {
    mgr.still_here()?;
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    let base = format!("FreeBank-wallet-{}", stamp());
    let node_wallets = wallet_files(&datadir);
    let others: Vec<PathBuf> = s
        .created()
        .iter()
        .chain(s.moved_aside.iter())
        .map(PathBuf::from)
        .filter(|d| *d != datadir && d.is_dir() && !wallet_files(d).is_empty())
        .collect();
    if node_wallets.is_empty() && others.is_empty() {
        return Err(format!("There's no wallet in {}.", datadir.display()));
    }
    let mut saved: Vec<Backup> = Vec::new();
    let result = async {
        if !node_wallets.is_empty() {
            backup_node(mgr, &s, &node_wallets, folder, &base, &mut saved).await?;
        }
        for dir in &others {
            // ".freebank.old-1727000000" -> "FreeBank-wallet-<time>-freebank-old-1727000000"
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let named = format!("{}-{}", base, file_safe(name.trim_start_matches('.')));
            saved.extend(copy_unused(dir, folder, &named).await?);
        }
        Ok::<(), String>(())
    }
    .await;
    // What was saved is recorded even when a later step failed.
    mgr.backups.lock().unwrap().extend(saved.iter().cloned());
    result.map(|_| saved.into_iter().map(|b| b.saved).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp folder standing in for a home: nothing here ever touches a real one.
    fn base(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fbob-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("home")).unwrap();
        d.canonicalize().unwrap()
    }

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// The layout of a Linux install under `b`/home: the app's folder, a node folder the app
    /// created (with its mark and a wallet), and nothing else yet.
    fn places(b: &Path) -> Places {
        let home = b.join("home");
        let app_dir = home.join(".local/share/com.ecxfreebank.freebank");
        let datadir = home.join(".freebank");
        write(&app_dir.join("settings.json"), b"{}");
        write(&app_dir.join("releases/v0.2.16/freebank/bin/freebankd"), &[0u8; 1000]);
        // Checked by this code (install.rs), so the app may start it.
        write(&app_dir.join("releases/v0.2.16/.verified"), b"");
        write(&datadir.join(DATADIR_MARK), b"");
        write(&datadir.join("wallet.dat"), &[1u8; 300]);
        write(&datadir.join("blocks/blk00000.dat"), &[2u8; 700]);
        Places {
            home,
            app_dir,
            screen_uses_app_dir: false,
            app_dir_shared: false,
            datadir: datadir.clone(),
            created: vec![datadir],
            moved_aside: Vec::new(),
            caches: Vec::new(),
            keep: Vec::new(),
            ours: Vec::new(),
        }
    }

    /// Ticks as the screen sends them: each id with the path the list shows for it now (none for
    /// an id that isn't on it).
    fn ticks(p: &Places, v: &[&str]) -> Vec<Tick> {
        let items = plan_items(p);
        v.iter()
            .map(|id| Tick {
                id: id.to_string(),
                path: items.iter().find(|i| i.id == *id).map(|i| i.path.clone()).unwrap_or_default(),
            })
            .collect()
    }

    fn find<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{} not in the plan", id))
    }

    #[test]
    fn node_folder_ticked_only_when_created() {
        let b = base("created");
        let mut p = places(&b);

        let items = plan_items(&p);
        let app = find(&items, "app");
        assert!(app.checked && app.allowed);
        assert_eq!(app.size, 1002);
        let node = find(&items, "node");
        assert!(node.checked && node.allowed);
        assert_eq!(node.size, 1000);

        // Set up by the app (the mark) but not recorded, as older installs: shown, not ticked.
        p.created.clear();
        let node = find(&plan_items(&p), "node").clone();
        assert!(!node.checked && node.allowed);
        assert!(node.note.contains("no record of creating it"), "{}", node.note);

        // Neither recorded nor marked: can't be ticked, and execute refuses it.
        std::fs::remove_file(p.datadir.join(DATADIR_MARK)).unwrap();
        let node = find(&plan_items(&p), "node").clone();
        assert!(!node.checked && !node.allowed);
        assert!(execute(&p, &ticks(&p, &["node"])).is_err());
        assert!(p.datadir.join("wallet.dat").exists());

        // Recorded but the mark is gone: another program may have made the folder again (BitWindow's
        // FreeBank uses the same default), so the record alone doesn't let it go.
        p.created = vec![p.datadir.clone()];
        let node = find(&plan_items(&p), "node").clone();
        assert!(!node.checked && !node.allowed);
        assert!(node.note.contains("mark"), "{}", node.note);
        assert!(execute(&p, &ticks(&p, &["node"])).is_err());
        assert!(p.datadir.join("wallet.dat").exists());

        // Recorded and marked: ticked, and it goes.
        write(&p.datadir.join(DATADIR_MARK), b"");
        assert!(find(&plan_items(&p), "node").checked);
        let done = execute(&p, &ticks(&p, &["node"])).unwrap();
        assert_eq!(done.removed, vec![p.datadir.clone()]);
        assert!(!p.datadir.exists());
        assert!(p.app_dir.join("settings.json").exists());
        assert_eq!(done.kept, vec![p.app_dir.clone()]);
        assert!(!done.app_removed);
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn only_recorded_asides_are_called_moved() {
        let b = base("aside");
        let mut p = places(&b);
        let home = p.home.clone();
        write(&home.join(".freebank.old-1727000000/wallet.dat"), b"w");
        write(&home.join(".freebank.old-1727000001/debug.log"), b"l");
        write(&home.join(".freebank.old-1727000002/wallet.dat"), b"someone's own copy");
        write(&home.join(".freebank.old-abc/wallet.dat"), b"w");
        write(&home.join(".freebank.old-/wallet.dat"), b"w");
        write(&home.join(".freebank-other/wallet.dat"), b"w");
        // Setup moved the first two aside and recorded them; nobody recorded the third.
        let moved = [home.join(".freebank.old-1727000000"), home.join(".freebank.old-1727000001")];
        p.moved_aside = moved.to_vec();
        let id = |path: &Path| format!("aside:{}", path.display());

        let items = plan_items(&p);
        let asides: Vec<&Item> = items.iter().filter(|i| i.kind == Kind::Aside).collect();
        let unrecorded = home.join(".freebank.old-1727000002");
        assert_eq!(
            asides.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
            vec![id(&moved[0]), id(&moved[1]), id(&unrecorded)]
        );
        assert!(asides[..2].iter().all(|i| !i.checked && i.allowed && i.label == ASIDE_LABEL));
        assert!(asides[0].note.contains("It holds an older wallet"));
        assert_eq!(asides[0].wallets, vec![moved[0].join("wallet.dat").to_string_lossy()]);
        assert!(asides[1].note.contains("No wallet"));
        assert!(asides[1].wallets.is_empty());
        // Named the same way but never recorded: not called moved by setup, and never removed here.
        assert!(!asides[2].checked && !asides[2].allowed);
        assert_eq!(asides[2].label, "Older node folder");
        assert!(asides[2].note.contains("no record of moving it"), "{}", asides[2].note);
        assert!(!asides[2].note.contains("Setup moved"));
        assert!(execute(&p, &ticks(&p, &[&id(&unrecorded)])).is_err());
        assert_eq!(find(&items, "node").wallets, vec![p.datadir.join("wallet.dat").to_string_lossy()]);
        assert!(find(&items, "app").wallets.is_empty());

        let done = execute(&p, &ticks(&p, &[&id(&moved[0])])).unwrap();
        assert_eq!(done.removed, vec![moved[0].clone()]);
        assert!(!moved[0].exists());
        assert!(moved[1].exists() && unrecorded.join("wallet.dat").exists());
        assert!(home.join(".freebank.old-abc/wallet.dat").exists());
        assert!(p.datadir.join("wallet.dat").exists());

        // A recorded folder is listed wherever the node's folder is now.
        p.datadir = home.join("disk/.freebank");
        assert!(plan_items(&p).iter().any(|i| i.id == id(&moved[1])));
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn every_folder_the_app_created_is_listed() {
        let b = base("earlier");
        let mut p = places(&b);
        let home = p.home.clone();
        // Installed into ~/.freebank, then switched to another folder and installed there too.
        let first = p.datadir.clone();
        let second = home.join("disk/freebank");
        write(&second.join(DATADIR_MARK), b"");
        write(&second.join("wallet.dat"), &[3u8; 50]);
        p.datadir = second.clone();
        p.created = vec![first.clone(), second.clone()];

        let items = plan_items(&p);
        let node = find(&items, "node");
        assert_eq!(node.path, second.to_string_lossy());
        assert!(node.checked && node.allowed);
        let earlier = find(&items, &format!("earlier:{}", first.display()));
        assert_eq!(earlier.kind, Kind::Earlier);
        assert!(earlier.checked && earlier.allowed, "{}", earlier.note);
        assert_eq!(earlier.wallets, vec![first.join("wallet.dat").to_string_lossy()]);

        // Without its mark it may be another program's now: listed, never ticked or removed.
        std::fs::remove_file(first.join(DATADIR_MARK)).unwrap();
        let earlier = find(&plan_items(&p), &format!("earlier:{}", first.display())).clone();
        assert!(!earlier.allowed && earlier.note.contains("mark"));
        write(&first.join(DATADIR_MARK), b"");

        let done = execute(&p, &ticks(&p, &["node", &format!("earlier:{}", first.display())])).unwrap();
        assert_eq!(done.removed, vec![second.clone(), first.clone()]);
        assert!(!first.exists() && !second.exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn caches_listed_only_when_theirs() {
        let b = base("caches");
        let mut p = places(&b);
        let home = p.home.clone();
        let cache = |id, path: PathBuf, program_named| Cache { id, path, program_named };
        write(&home.join(".cache/com.ecxfreebank.freebank/x"), &[0u8; 10]);
        write(&home.join(".cache/freebank/WebKitCache/Version 17/blob"), &[0u8; 5]);
        write(&home.join(".local/share/freebank/hsts-storage.sqlite"), &[0u8; 5]);
        write(&home.join(".local/share/freebank/notes.txt"), b"someone else's");
        write(&home.join("dev-app/settings.json"), b"{}");
        p.caches = vec![
            cache("cache", home.join(".cache/com.ecxfreebank.freebank"), false),
            cache("local-data", p.app_dir.clone(), false),
            cache("gone", home.join("Library/WebKit/com.ecxfreebank.freebank"), false),
            cache("webkit-cache", home.join(".cache/freebank"), true),
            cache("webkit-data", home.join(".local/share/freebank"), true),
            cache("real-app", home.join("dev-app"), false),
            cache("above", home.join(".local"), false),
        ];
        let items = plan_items(&p);
        let caches: Vec<&str> = items.iter().filter(|i| i.kind == Kind::Cache).map(|i| i.id.as_str()).collect();
        assert_eq!(caches, vec!["cache:cache", "cache:webkit-cache"]);
        assert!(find(&items, "cache:cache").checked);
        // Each line has a name of its own, not two called "Window cache".
        assert_eq!(find(&items, "cache:cache").label, "App cache");
        assert_eq!(find(&items, "cache:webkit-cache").label, "Window cache");
        assert_eq!(find(&items, "cache:webkit-cache").size, 5);

        // Caches wait for exit, then go through the same guard.
        let done = execute(&p, &ticks(&p, &["cache:cache", "cache:webkit-cache"])).unwrap();
        assert!(done.removed.is_empty());
        assert_eq!(done.at_exit.len(), 2);
        assert_eq!(done.webkit_only, vec![home.join(".cache/freebank")]);
        assert!(home.join(".cache/freebank").exists());

        // Before exit, another program named freebank starts keeping data in the program-named
        // folder: at exit it is looked at again and left, and only the other cache goes.
        write(&home.join(".cache/freebank/wallet.dat"), b"theirs");
        let later = AtExit { home: home.clone(), paths: done.at_exit, webkit_only: done.webkit_only };
        let errors = wipe(&later);
        assert_eq!(errors.len(), 1, "{:?}", errors);
        assert!(errors[0].starts_with("Left "), "{}", errors[0]);
        assert!(home.join(".cache/freebank/wallet.dat").exists());
        assert!(!home.join(".cache/com.ecxfreebank.freebank").exists());

        // Holding only WebKit's files again, it goes.
        std::fs::remove_file(home.join(".cache/freebank/wallet.dat")).unwrap();
        assert!(wipe(&later).is_empty());
        assert!(!home.join(".cache/freebank").exists());
        assert!(home.join(".local/share/freebank/notes.txt").exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn ticks_must_name_the_path_shown() {
        let b = base("shown");
        let mut p = places(&b);
        let home = p.home.clone();
        let shown = ticks(&p, &["app", "node"]);

        // Settings now point at another folder FreeBank once set up, with its own wallet. The
        // "node" id now names that folder.
        let other = home.join("disk/.freebank");
        write(&other.join(DATADIR_MARK), b"");
        write(&other.join("wallet.dat"), b"other");
        p.datadir = other.clone();
        let err = execute(&p, &shown[1..]).unwrap_err();
        assert!(err.contains("list has changed"), "{}", err);
        let err = execute(&p, &shown).unwrap_err();
        assert!(err.contains("list has changed"), "{}", err);
        assert!(other.join("wallet.dat").exists());
        assert!(home.join(".freebank/wallet.dat").exists());
        assert!(p.app_dir.join("settings.json").exists());

        // Ticks made from the list as it is now are taken.
        p.created.push(other.clone());
        let now = ticks(&p, &["node"]);
        assert_eq!(now[0].path, other.to_string_lossy());
        execute(&p, &now).unwrap();
        assert!(!other.exists());
        assert!(home.join(".freebank/wallet.dat").exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn shared_app_folder_keeps_the_rest() {
        let b = base("shared");
        let mut p = places(&b);
        p.app_dir_shared = true;
        write(&p.app_dir.join("tools/grpcurl"), b"g");
        write(&p.app_dir.join("notes/mine.txt"), &[7u8; 50]);
        let app = find(&plan_items(&p), "app").clone();
        assert!(app.checked && app.note.contains("FREEBANK_APP_DIR"), "{}", app.note);
        // Only FreeBank's own files count: settings.json, the release and grpcurl.
        assert_eq!(app.size, 2 + 1000 + 1);

        for screen in [false, true] {
            p.screen_uses_app_dir = screen;
            write(&p.app_dir.join("settings.json"), b"{}");
            let done = execute(&p, &ticks(&p, &["app"])).unwrap();
            assert!(done.app_removed && done.at_exit.is_empty());
            for name in APP_FILES {
                assert!(!p.app_dir.join(name).exists(), "{}", name);
            }
            assert!(p.app_dir.join("notes/mine.txt").exists());
        }
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn app_note_names_only_what_is_there() {
        let b = base("appnote");
        let mut p = places(&b);
        // grpcurl from PATH or BitWindow leaves nothing in tools/: it isn't named.
        assert_eq!(find(&plan_items(&p), "app").note, "Settings and the node program.");
        write(&p.app_dir.join("tools/grpcurl"), b"g");
        write(&p.app_dir.join("logs/freebankd.out"), b"log");
        assert_eq!(find(&plan_items(&p), "app").note, "Settings, the node program, grpcurl and logs.");
        p.screen_uses_app_dir = true;
        assert_eq!(
            find(&plan_items(&p), "app").note,
            "Settings, the node program, grpcurl and logs, and the app's window cache, which is removed when FreeBank closes."
        );
        for name in ["settings.json", "releases", "tools", "logs"] {
            let path = p.app_dir.join(name);
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
        assert_eq!(find(&plan_items(&p), "app").note, "The app's window cache, which is removed when FreeBank closes.");
        p.screen_uses_app_dir = false;
        write(&p.app_dir.join("logs/freebankd.out"), b"log");
        assert_eq!(find(&plan_items(&p), "app").note, "Logs.");
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn balance_counts_every_coin() {
        let info = serde_json::json!({"balance": 1.5, "unconfirmed_balance": 0.25, "immature_balance": 50.0});
        assert_eq!(holdings(&info), Some((51.75, 50.25)));
        assert_eq!(holdings(&serde_json::json!({"balance": 2.0})), Some((2.0, 0.0)));
        assert_eq!(holdings(&serde_json::json!({"walletname": "wallet.dat"})), None);
    }

    #[test]
    fn app_folder_waits_for_exit_when_the_screen_uses_it() {
        let b = base("appdir");
        let mut p = places(&b);
        p.screen_uses_app_dir = true;
        for name in ["tools/grpcurl", "tmp/x", "logs/freebankd.out", "storage/localstorage", "mediakeys/k"] {
            write(&p.app_dir.join(name), b"x");
        }
        let done = execute(&p, &ticks(&p, &["app"])).unwrap();
        assert!(done.app_removed);
        assert!(done.removed.is_empty());
        assert_eq!(done.at_exit, vec![p.app_dir.clone()]);
        // FreeBank's own files go now; the screen's stay until exit.
        for name in APP_FILES {
            assert!(!p.app_dir.join(name).exists(), "{}", name);
        }
        assert!(p.app_dir.join("storage/localstorage").exists());
        assert!(wipe(&AtExit { home: p.home.clone(), paths: done.at_exit, webkit_only: done.webkit_only }).is_empty());
        assert!(!p.app_dir.exists());
        assert!(p.datadir.join("wallet.dat").exists());

        // When the screen doesn't use it (macOS), the folder goes at once.
        let mut p = places(&b);
        p.screen_uses_app_dir = false;
        let done = execute(&p, &ticks(&p, &["app", "node"])).unwrap();
        assert!(done.at_exit.is_empty());
        assert!(!p.app_dir.exists() && !p.datadir.exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_removed_never_followed() {
        let b = base("links");
        let mut p = places(&b);
        // The node's folder is a link to a folder elsewhere.
        let real = b.join("disk/freebank");
        std::fs::create_dir_all(b.join("disk")).unwrap();
        std::fs::rename(&p.datadir, &real).unwrap();
        std::os::unix::fs::symlink(&real, &p.datadir).unwrap();
        // A cache that is a link too.
        let elsewhere = b.join("disk/cache");
        write(&elsewhere.join("keep"), b"k");
        let cache_link = p.home.join(".cache/com.ecxfreebank.freebank");
        std::fs::create_dir_all(cache_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &cache_link).unwrap();
        p.caches = vec![Cache { id: "cache", path: cache_link.clone(), program_named: false }];

        let items = plan_items(&p);
        let node = find(&items, "node");
        assert!(node.checked && node.allowed);
        assert_eq!(node.size, 0);
        assert!(node.note.contains("only the link goes"), "{}", node.note);
        // Only the link goes, so no wallet goes with it: the box won't say the wallet is deleted.
        assert!(node.wallets.is_empty(), "{:?}", node.wallets);

        let done = execute(&p, &ticks(&p, &["node", "cache:cache"])).unwrap();
        assert!(std::fs::symlink_metadata(&p.datadir).is_err());
        assert!(real.join("wallet.dat").exists() && real.join(DATADIR_MARK).exists());
        // And where it led is shown as left in place.
        assert!(done.kept.contains(&real), "{:?}", done.kept);
        assert!(wipe(&AtExit { home: p.home.clone(), paths: done.at_exit, webkit_only: done.webkit_only }).is_empty());
        assert!(std::fs::symlink_metadata(&cache_link).is_err());
        assert!(elsewhere.join("keep").exists());

        // What the guard relies on: std's remove_dir_all on a link removes only the link.
        let link = b.join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        std::fs::remove_dir_all(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(elsewhere.join("keep").exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn guard_protects_home_and_above() {
        let b = base("guard");
        let home = b.join("home");
        std::fs::create_dir_all(home.join("x")).unwrap();
        assert!(check_deletable(Path::new("/"), &home).is_err());
        assert!(check_deletable(&home, &home).is_err());
        assert!(check_deletable(&b, &home).is_err());
        assert!(check_deletable(b.parent().unwrap(), &home).is_err());
        assert!(check_deletable(&home.join("x/.."), &home).is_err());
        assert!(check_deletable(&home.join("x/../."), &home).is_err());
        assert!(check_deletable(&home.join("x/../../home"), &home).is_err());
        assert!(check_deletable(Path::new("relative/.freebank"), &home).is_err());
        assert!(check_deletable(&home.join("missing/deeper"), &home).is_err());
        assert_eq!(check_deletable(&home.join("x"), &home).unwrap(), home.join("x"));
        assert!(check_deletable(&home.join("x"), &b.join("no-home")).is_err());
        // A link that leads to the home folder or above is refused too.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&home, home.join("x/to-home")).unwrap();
            std::os::unix::fs::symlink("/", home.join("x/to-root")).unwrap();
            assert!(check_deletable(&home.join("x/to-home"), &home).is_err());
            assert!(check_deletable(&home.join("x/to-root"), &home).is_err());
        }

        // A node folder set to the home folder, or above it, is listed but never deleted.
        for datadir in [home.clone(), b.clone(), PathBuf::from("/")] {
            let mut p = places(&b);
            if datadir != Path::new("/") {
                write(&datadir.join(DATADIR_MARK), b"");
            }
            p.datadir = datadir.clone();
            p.created = vec![datadir.clone()];
            let node = find(&plan_items(&p), "node").clone();
            assert!(!node.allowed && !node.checked, "{}", datadir.display());
            assert!(node.note.contains("never deletes"), "{}", node.note);
            assert!(execute(&p, &ticks(&p, &["node"])).is_err());
            assert!(home.exists());
            if datadir != Path::new("/") {
                std::fs::remove_file(datadir.join(DATADIR_MARK)).unwrap();
            }
        }
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn only_planned_ids_are_removed() {
        let b = base("ids");
        let p = places(&b);
        assert!(execute(&p, &[]).is_err());
        for bad in ["../..", "/", "aside:../../home", "cache:anything", p.datadir.to_str().unwrap()] {
            assert!(execute(&p, &ticks(&p, &[bad])).is_err(), "{}", bad);
        }
        // One bad id refuses the lot, before anything is deleted.
        assert!(execute(&p, &ticks(&p, &["app", "node", "bogus"])).is_err());
        assert!(p.app_dir.join("settings.json").exists());
        assert!(p.datadir.join("wallet.dat").exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn nothing_that_stays_is_taken_along() {
        let b = base("inside");
        let mut p = places(&b);
        // A node folder inside the app's folder goes only if it is ticked too.
        let inner = p.app_dir.join("node");
        write(&inner.join(DATADIR_MARK), b"");
        write(&inner.join("wallet.dat"), b"w");
        p.datadir = inner.clone();
        p.created = vec![inner.clone()];
        let err = execute(&p, &ticks(&p, &["app"])).unwrap_err();
        assert!(err.contains("isn't ticked"), "{}", err);
        assert!(inner.join("wallet.dat").exists());

        // A wallet backup inside a ticked folder stops it too.
        p.keep = vec![inner.join("FreeBank-wallet-20260927-120000.dat")];
        write(&p.keep[0], b"backup");
        assert!(execute(&p, &ticks(&p, &["app", "node"])).is_err());
        p.keep.clear();
        let done = execute(&p, &ticks(&p, &["app", "node"])).unwrap();
        assert!(done.app_removed && !p.app_dir.exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn backups_copy_every_wallet() {
        let b = base("backup");
        let p = places(&b);
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        let mut bdb = vec![0u8; 4096];
        bdb[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
        write(&p.datadir.join("savings"), &bdb);

        let base_name = "FreeBank-wallet-20260927-120000";
        write(&docs.join(format!("{}.dat", base_name)), b"an older backup");
        // The wallet may be readable by others (made under a loose umask); its copies never are.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p.datadir.join("wallet.dat"), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let old = unsafe { libc::umask(0o022) };
        let saved = copy_wallets(&p.datadir, &docs, base_name);
        unsafe { libc::umask(old) };
        let saved = saved.unwrap();
        assert_eq!(
            saved,
            vec![
                Backup {
                    wallet: p.datadir.join("wallet.dat").to_string_lossy().into_owned(),
                    saved: docs.join(format!("{}-2.dat", base_name)).to_string_lossy().into_owned(),
                },
                Backup {
                    wallet: p.datadir.join("savings").to_string_lossy().into_owned(),
                    saved: docs.join(format!("{}-savings.dat", base_name)).to_string_lossy().into_owned(),
                },
            ]
        );
        #[cfg(unix)]
        for b in &saved {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&b.saved).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", b.saved);
        }
        assert_eq!(std::fs::read(&saved[0].saved).unwrap(), vec![1u8; 300]);
        assert_eq!(std::fs::read(docs.join(format!("{}.dat", base_name))).unwrap(), b"an older backup");
        assert_eq!(wallet_label(&p.datadir, &p.datadir.join("wallets/house/wallet.dat")), "wallets-house-wallet");
        assert_eq!(wallet_label(&p.datadir, Path::new("/mnt/w/mywallets/savings")), "mywallets-savings");
        assert!(copy_wallets(&p.home.join("nothing"), &docs, base_name).is_err());

        let s = stamp();
        assert!(s.len() == 15 && s.as_bytes()[8] == b'-', "{}", s);
        std::fs::remove_dir_all(&b).unwrap();
    }

    /// A NodeManager on the temp layout, its RPC port one nothing listens on.
    fn manager(p: &Places) -> NodeManager {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let s = super::super::Settings {
            datadir: p.datadir.to_string_lossy().into_owned(),
            datadirs_created: p.created.iter().map(|d| d.to_string_lossy().into_owned()).collect(),
            moved_aside: p.moved_aside.iter().map(|d| d.to_string_lossy().into_owned()).collect(),
            installed_tag: Some("v0.2.16".into()),
            rpc_port: port,
            p2p_port: port,
            ..Default::default()
        };
        write(&p.app_dir.join("settings.json"), &serde_json::to_vec(&s).unwrap());
        let mgr = NodeManager::new(p.app_dir.clone());
        assert_eq!(mgr.settings.try_lock().unwrap().rpc_port, port);
        mgr
    }

    #[tokio::test]
    async fn run_end_to_end() {
        let b = base("run");
        let mut p = places(&b);
        p.screen_uses_app_dir = true;
        write(&p.app_dir.join("storage/localstorage"), b"x");
        let mgr = manager(&p);

        // The plan through the manager: nothing blocks it, and a stopped node has no balance.
        let plan = plan(&mgr, p.clone()).await.unwrap();
        assert_eq!(plan.blocked, None);
        assert_eq!(plan.wallets, vec![p.datadir.join("wallet.dat").to_string_lossy().into_owned()]);
        assert_eq!(plan.balance, None);
        assert!(plan.balance_note.unwrap().contains("isn't running"));

        // With the node stopped, the backup is a copy of the wallet file.
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].starts_with(docs.join("FreeBank-wallet-").to_str().unwrap()));
        assert_eq!(std::fs::read(&saved[0]).unwrap(), vec![1u8; 300]);
        assert_eq!(
            *mgr.backups.lock().unwrap(),
            vec![Backup { wallet: p.datadir.join("wallet.dat").to_string_lossy().into_owned(), saved: saved[0].clone() }]
        );
        // The plan says which wallet each backup covers.
        let plan2 = super::plan(&mgr, p.clone()).await.unwrap();
        assert_eq!(plan2.backups[0].wallet, plan2.wallets[0]);

        // Refused, with nothing removed, while an install runs or another operation holds the node.
        mgr.install.lock().unwrap().running = true;
        let err = run(&mgr, p.clone(), ticks(&p, &["app", "node"])).await.unwrap_err();
        assert!(err.contains("being installed") && err.ends_with("Nothing was removed."), "{}", err);
        mgr.install.lock().unwrap().running = false;
        {
            let _other = mgr.busy("Stopping FreeBank…").unwrap();
            assert!(run(&mgr, p.clone(), ticks(&p, &["app", "node"])).await.is_err());
        }
        assert!(p.datadir.join("wallet.dat").exists() && p.app_dir.join("settings.json").exists());

        let out = run(&mgr, p.clone(), ticks(&p, &["app", "node"])).await.unwrap();
        assert!(out.app_removed);
        assert_eq!(out.removed, vec![p.datadir.to_string_lossy().into_owned()]);
        assert_eq!(out.at_exit, vec![p.app_dir.to_string_lossy().into_owned()]);
        assert_eq!(out.backups, saved);
        assert!(!p.datadir.exists());
        assert!(!p.app_dir.join("settings.json").exists());
        assert!(std::path::Path::new(&saved[0]).exists());

        // Nothing is written again: settings stay gone, and the node won't start or be renamed.
        let s = mgr.settings.lock().await.clone();
        mgr.save_settings(s).await.unwrap();
        assert!(!p.app_dir.join("settings.json").exists());
        assert!(mgr.still_here().is_err());
        assert!(super::super::process::start(&mgr).await.is_err());
        assert!(super::super::process::set_tag(&mgr, "freebank-test").await.is_err());
        assert!(!p.datadir.exists());
        assert!(!p.app_dir.join("logs").exists());
        assert!(run(&mgr, p.clone(), ticks(&p, &["app"])).await.is_err());

        // At exit the screen's folder goes too.
        assert!(p.app_dir.join("storage/localstorage").exists());
        wipe_at_exit(&mgr);
        assert!(!p.app_dir.exists());
        assert!(p.home.exists());
        std::fs::remove_dir_all(&b).unwrap();
    }

    /// A restore records the folder it moves the replaced wallet into, as setup records its moves
    /// (recovery/job.rs): Obliterate lists it as FreeBank's own, unticked like setup's, and Back up
    /// copies its wallet first.
    #[tokio::test]
    async fn a_restores_folder_is_freebanks_own() {
        let b = base("restoreaside");
        let mut p = places(&b);
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        let dir = p.home.join(".freebank.old-1790700000");
        let wallet = dir.join("wallet.dat.old-20260929-181500");
        let mut w = vec![0u8; 4096];
        w[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
        write(&wallet, &w);
        let id = format!("aside:{}", dir.display());
        // Not recorded: only a look-alike, which stays.
        let items = plan_items(&p);
        assert_eq!(find(&items, &id).label, "Older node folder");
        assert!(!find(&items, &id).allowed);
        // Recorded: FreeBank's own, with its wallet named.
        p.moved_aside = vec![dir.clone()];
        let items = plan_items(&p);
        let item = find(&items, &id);
        assert_eq!(item.label, ASIDE_LABEL);
        assert!(!item.checked && item.allowed);
        assert!(item.note.contains("during setup or a restore") && item.note.contains("older wallet"), "{}", item.note);
        assert_eq!(item.wallets, vec![wallet.to_string_lossy()]);
        // Back up copies it too, named after its folder.
        let mgr = manager(&p);
        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert!(
            saved.iter().any(|s| s.contains("-freebank-old-1790700000") && std::fs::read(s).unwrap() == w),
            "{:?}",
            saved
        );
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[tokio::test]
    async fn backups_take_moved_aside_wallets_too() {
        let b = base("asidebackup");
        let mut p = places(&b);
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        write(&p.home.join(".freebank.old-1727000000/wallets/wallet.dat"), b"old node's");
        write(&p.home.join(".freebank.old-1727000001/debug.log"), b"no wallet here");
        // Named the same way but not moved by setup: its wallet isn't FreeBank's to copy.
        write(&p.home.join(".freebank.old-1727000002/wallet.dat"), b"someone else's");
        p.moved_aside = vec![p.home.join(".freebank.old-1727000000"), p.home.join(".freebank.old-1727000001")];
        let mgr = manager(&p);

        // The node is stopped: its wallet is copied, and so is the moved-aside one, named after its folder.
        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert_eq!(saved.len(), 2, "{:?}", saved);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), vec![1u8; 300]);
        assert!(saved[1].ends_with("-freebank-old-1727000000.dat"), "{}", saved[1]);
        assert_eq!(std::fs::read(&saved[1]).unwrap(), b"old node's");
        assert_eq!(
            mgr.backups.lock().unwrap()[1].wallet,
            p.home.join(".freebank.old-1727000000/wallets/wallet.dat").to_string_lossy()
        );

        // Only a moved-aside wallet: that one is still backed up.
        std::fs::remove_file(p.datadir.join("wallet.dat")).unwrap();
        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert_eq!(saved.len(), 1, "{:?}", saved);
        // (-2 when it lands in the same second as the backup above: nothing is overwritten.)
        assert!(saved[0].contains("-freebank-old-1727000000"), "{}", saved[0]);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"old node's");

        // No wallet anywhere.
        std::fs::remove_dir_all(p.home.join(".freebank.old-1727000000")).unwrap();
        assert!(backup_wallet(&mgr, &docs).await.is_err());
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[tokio::test]
    async fn node_folder_alone_leaves_setup_to_run_again() {
        let b = base("nodeonly");
        let p = places(&b);
        let mgr = manager(&p);
        let s = mgr.settings.lock().await.clone();
        assert!(mgr.can_start(&s));

        // The app stays, the node's folder goes: freebankd can't start without it, so first run
        // must set FreeBank up again rather than start the node.
        let out = run(&mgr, p.clone(), ticks(&p, &["node"])).await.unwrap();
        assert!(!out.app_removed);
        assert!(!p.datadir.exists() && mgr.freebankd("v0.2.16").is_file());
        assert!(!mgr.can_start(&mgr.settings.lock().await.clone()));
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn older_settings_have_no_record() {
        let s: super::super::Settings = serde_json::from_str(r#"{"datadir":"/x","installed_tag":"v0.2.15"}"#).unwrap();
        assert_eq!(s.datadir_created, None);
        assert!(s.created().is_empty() && s.moved_aside.is_empty() && !s.keep_running);
        // v0.1.1 recorded one folder; it still counts, and the new list keeps every one.
        let mut s: super::super::Settings =
            serde_json::from_str(r#"{"datadir":"/b","datadir_created":"/a"}"#).unwrap();
        assert_eq!(s.created(), vec!["/a".to_string()]);
        s.add_created("/b");
        s.add_created("/b");
        assert_eq!(s.created(), vec!["/a".to_string(), "/b".to_string()]);
        assert_eq!(s.datadir_created.as_deref(), Some("/b"));
    }

    fn bdb() -> Vec<u8> {
        let mut b = vec![0u8; 4096];
        b[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
        b
    }

    /// Finding 6: a node in the data folder on another port (BitWindow's FreeBank uses the same one)
    /// is seen by its lock. Obliterate won't run, the folder can't be ticked, and its wallet isn't
    /// copied while that node has it open.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_node_on_another_port_stops_obliterate_and_the_copy() {
        use super::super::testnode::{free_port, FakeNode, Opts};
        let b = base("elsewhere");
        let p = places(&b);
        let mgr = manager(&p);
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        let node = FakeNode::spawn(&p.datadir, free_port(), Opts::default());

        let plan1 = plan(&mgr, p.clone()).await.unwrap();
        assert!(plan1.blocked.as_deref().unwrap_or("").contains("another program"), "{:?}", plan1.blocked);
        let item = find(&plan1.items, "node");
        assert!(!item.allowed && !item.checked);
        assert!(item.note.contains(&format!("process {}", node.pid())), "{}", item.note);
        let err = backup_wallet(&mgr, &docs).await.unwrap_err();
        assert!(err.contains("no node has it open"), "{}", err);
        assert_eq!(std::fs::read_dir(&docs).unwrap().count(), 0);
        let err = run(&mgr, p.clone(), ticks(&p, &["app"])).await.unwrap_err();
        assert!(err.ends_with("Nothing was removed."), "{}", err);
        // The last check, once the app's node has stopped: nothing in use may go. (Here the list
        // took that node for the app's own, as it would the app's node until Obliterate stops it.)
        let mut as_ours = p.clone();
        as_ours.ours = vec![node.pid()];
        let err = execute(&as_ours, &ticks(&as_ours, &["app", "node"])).unwrap_err();
        assert!(err.contains("Nothing was removed"), "{}", err);
        assert!(p.datadir.join("wallet.dat").exists() && p.app_dir.join("settings.json").exists());

        drop(node);
        let plan2 = plan(&mgr, p.clone()).await.unwrap();
        assert_eq!(plan2.blocked, None);
        assert!(find(&plan2.items, "node").allowed);
        assert_eq!(backup_wallet(&mgr, &docs).await.unwrap().len(), 1);
        std::fs::remove_dir_all(&b).unwrap();
    }

    /// Findings 1 and 2 through a running node: the balance counts only with one wallet and the node
    /// caught up, and the copy the node writes is readable only by its owner.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_running_node_gives_a_trusted_balance_and_a_private_copy() {
        use super::super::testnode::{FakeNode, Opts};
        use std::os::unix::fs::PermissionsExt;
        let b = base("rpcbackup");
        let p = places(&b);
        let mgr = manager(&p);
        let port = mgr.settings.try_lock().unwrap().rpc_port;
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::set_permissions(p.datadir.join("wallet.dat"), std::fs::Permissions::from_mode(0o644)).unwrap();
        let _node = FakeNode::spawn(&p.datadir, port, Opts::default());

        let plan1 = plan(&mgr, p.clone()).await.unwrap();
        assert_eq!((plan1.balance, plan1.balance_note.clone()), (Some(0.0), None));
        assert_eq!(plan1.backup_note, None);
        // The explorer, as the Node tab last saw it, is well ahead: the node hasn't caught up.
        *mgr.explorer_tip.lock().unwrap() = Some((std::time::Instant::now(), 900));
        let behind = plan(&mgr, p.clone()).await.unwrap();
        assert_eq!(behind.balance, None);
        assert!(behind.balance_note.unwrap().contains("block 10 of 900"));
        *mgr.explorer_tip.lock().unwrap() = None;

        let old = unsafe { libc::umask(0o022) };
        let saved = backup_wallet(&mgr, &docs).await;
        unsafe { libc::umask(old) };
        let saved = saved.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), vec![1u8; 300]);
        assert_eq!(std::fs::metadata(&saved[0]).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(mgr.backups.lock().unwrap()[0].wallet, p.datadir.join("wallet.dat").to_string_lossy());

        // A second wallet: the node reports one balance only, so none is given; and copying each
        // whole means stopping the node, which FreeBank never does to one it didn't start.
        write(&p.datadir.join("savings"), &bdb());
        let plan2 = plan(&mgr, p.clone()).await.unwrap();
        assert_eq!(plan2.balance, None);
        assert!(plan2.balance_note.unwrap().contains("2 wallets"));
        let note = plan2.backup_note.unwrap();
        assert!(note.contains("started by another program"), "{}", note);
        assert_eq!(backup_wallet(&mgr, &docs).await.unwrap_err(), note);
        std::fs::remove_dir_all(&b).unwrap();
    }

    /// Several wallets and the app's own node: it stops for a moment, every wallet is copied whole,
    /// and it starts again.
    #[cfg(unix)]
    #[tokio::test]
    async fn several_wallets_are_copied_with_the_apps_node_stopped_for_a_moment() {
        use super::super::testnode::{self, KillOnDrop};
        let (d, mgr) = testnode::manager("severalours", false).await;
        let s = mgr.settings.lock().await.clone();
        let datadir = PathBuf::from(&s.datadir);
        let docs = d.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        write(&datadir.join("wallet.dat"), &[1u8; 300]);
        write(&datadir.join("savings"), &bdb());
        process::start(&mgr).await.unwrap();
        testnode::wait_for_port(s.rpc_port);
        let first = process::managed_pids(&mgr).await[0];
        let _g1 = KillOnDrop { pid: first, datadir: datadir.clone() };

        let mut p = places(&d);
        p.datadir = datadir.clone();
        p.created = vec![datadir.clone()];
        p.ours = vec![first];
        let plan1 = plan(&mgr, p).await.unwrap();
        assert!(plan1.backup_note.unwrap().contains("stops the node for a moment"));

        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert_eq!(saved.len(), 2, "{:?}", saved);
        assert_eq!(std::fs::read(&saved[1]).unwrap(), bdb());
        testnode::wait_for_port(s.rpc_port);
        let again = process::managed_pids(&mgr).await;
        assert_eq!(again.len(), 1);
        assert_ne!(again[0], first);
        let _g2 = KillOnDrop { pid: again[0], datadir: datadir.clone() };
        process::stop(&mgr).await.unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_balance_only_once_the_node_has_caught_up() {
        let info = |b: u64, h: u64, ibd: bool| serde_json::json!({"blocks": b, "headers": h, "initialblockdownload": ibd});
        assert_eq!(caught_up(&info(10, 10, false), None), Ok(()));
        assert_eq!(caught_up(&info(10, 10, false), Some(11)), Ok(()));
        // Re-syncing after "Delete chain data": a funded wallet reads 0 until then.
        let e = caught_up(&info(5, 10, false), None).unwrap_err();
        assert!(e.contains("block 5 of 10"), "{}", e);
        assert!(caught_up(&info(10, 10, true), None).unwrap_err().contains("still catching up"));
        let e = caught_up(&info(10, 10, false), Some(1_500)).unwrap_err();
        assert!(e.contains("block 10 of 1,500"), "{}", e);
        assert!(caught_up(&serde_json::json!({"blocks": 10}), None).is_err());
    }

    #[test]
    fn app_note_names_the_new_files() {
        // Everything v0.2.0 keeps in the app's folder, as the send, phone, security, wallet and node
        // code write it.
        let files = [
            "wallet/seed.enc",
            "sends.json",
            ".sends.json.tmp",
            "sends.json.damaged-1790000000",
            "phone/desktop.key",
            "phone/devices.json",
            "phone/config.json",
            "phone/held.json",
            "phone/sends.log",
            "backups.json",
            "node.pid",
            "node.pid.new",
        ];
        for (shared, screen) in [(true, false), (false, true)] {
            let b = base(&format!("appfiles-{}", shared));
            let mut p = places(&b);
            for f in files {
                write(&p.app_dir.join(f), b"x");
            }
            write(&p.app_dir.join("notes/mine.txt"), b"a developer's own");
            assert_eq!(
                find(&plan_items(&p), "app").note,
                "Settings, the node program, your recovery words (encrypted), the record of your sends, the phone link \
                 (this computer's key, your paired phones, held sends and the phone's send log) and the list of wallet \
                 backups FreeBank made (the backups themselves stay)."
            );
            // Each goes with FreeBank's own files: from a shared developer folder (FREEBANK_APP_DIR),
            // and at once when the screen keeps the folder until exit.
            p.app_dir_shared = shared;
            p.screen_uses_app_dir = screen;
            execute(&p, &ticks(&p, &["app"])).unwrap();
            for f in files {
                assert!(!p.app_dir.join(f).exists(), "{} was left behind", f);
            }
            for dir in ["wallet", "phone"] {
                assert!(!p.app_dir.join(dir).exists(), "{}/ was left behind", dir);
            }
            assert!(p.app_dir.join("notes/mine.txt").exists());
            std::fs::remove_dir_all(&b).unwrap();
        }
        // A damaged log alone still counts as the record of your sends.
        let b = base("appfiles-damaged");
        let p = places(&b);
        write(&p.app_dir.join("sends.json.damaged-1790000000"), b"x");
        assert!(find(&plan_items(&p), "app").note.contains("the record of your sends"));
        std::fs::remove_dir_all(&b).unwrap();
    }
}
