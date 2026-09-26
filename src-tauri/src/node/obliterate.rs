//! "Obliterate": remove everything FreeBank put on this computer. The list of what that is, with
//! paths and sizes, is always worked out here: `execute` acts only on the ticked items of a fresh
//! plan, never on paths from the screen. The screen sends back each tick with the path it showed,
//! and a tick whose item now names another path is refused. The node's data folder is ticked for
//! you only when the app created it and its mark is still there, because freebankd's default
//! folder may hold a node run by hand; folders setup moved aside never are. The eCash node, the
//! enforcer and BitWindow are never on the list. What the running screen still uses (the app's
//! folder on Linux, the caches) goes when the app exits.

use super::{detect, process, wallet_files, NodeManager, DATADIR_MARK};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// FreeBank's own files in the app's folder. They go at once, even when the folder waits for exit.
const APP_FILES: &[&str] = &["settings.json", "releases", "tools", "tmp", "logs"];

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
    /// The node folder the app created when it installed (Settings::datadir_created).
    pub datadir_created: Option<PathBuf>,
    pub caches: Vec<Cache>,
    /// What must survive whatever is ticked: wallet backups made from the plan.
    pub keep: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    App,
    Node,
    Aside,
    Cache,
}

/// One line of the list.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// "app", "node", "aside:<folder name>" or "cache:<which>".
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
    /// The wallets in it (the node's folder and folders setup moved aside).
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

/// Folders setup moved aside: siblings of the node's folder named <name>.old-<digits>.
fn asides(datadir: &Path) -> Vec<(String, PathBuf)> {
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
            let digits = n.strip_prefix(&prefix)?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| (n.clone(), e.path()))
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

fn wallet_strings(dir: &Path) -> Vec<String> {
    wallet_files(dir).iter().map(|w| w.to_string_lossy().into_owned()).collect()
}

/// What of FreeBank's own is in its folder, as the list says it: "Settings, the node program and
/// logs". grpcurl counts only when it is FreeBank's own copy (tools/), not one found elsewhere.
fn app_contents(app_dir: &Path) -> String {
    let parts: Vec<&str> = [
        ("settings.json", "settings"),
        ("releases", "the node program"),
        ("tools/grpcurl", "grpcurl"),
        ("logs", "logs"),
        ("tmp", "an unfinished download"),
    ]
    .iter()
    .filter(|(name, _)| exists(&app_dir.join(name)))
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
            app.size = APP_FILES.iter().map(|n| size_of(&p.app_dir.join(n))).sum();
        }
        items.push(app);
    }
    if exists(&p.datadir) {
        let recorded = p.datadir_created.as_deref() == Some(p.datadir.as_path());
        let marked = p.datadir.join(DATADIR_MARK).exists();
        // The record alone isn't enough: after the folder was deleted, another program (BitWindow's
        // FreeBank uses the same default) may have made a new one at the same path, without the mark.
        let created = recorded && marked;
        let note = if created {
            "Chain data, your wallet and freebank.conf. FreeBank created this folder."
        } else if marked {
            "FreeBank didn't create this folder; it may hold a node you run yourself. Tick it only if you're sure."
        } else if recorded {
            "FreeBank created a folder here once, but its mark (.freebank-node) is gone, so another program may have made this one. It stays."
        } else {
            "FreeBank didn't create or set up this folder; it may hold a node you run yourself, so it stays."
        };
        let mut node = entry(p, "node".into(), Kind::Node, "The node's data folder", &p.datadir, created, marked, note);
        node.wallets = wallet_strings(&p.datadir);
        items.push(node);
    }
    for (name, path) in asides(&p.datadir) {
        let wallets = wallet_strings(&path);
        let note = if wallets.is_empty() {
            "Setup moved an older node's data here and promised not to delete it. No wallet was found in it."
        } else {
            "Setup moved an older node's data here and promised not to delete it. It holds that node's wallet."
        };
        let mut aside = entry(p, format!("aside:{}", name), Kind::Aside, "Folder setup moved aside", &path, false, true, note);
        aside.wallets = wallets;
        items.push(aside);
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
            "Window cache",
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
        // "node" and "aside:…" follow the data folder in settings, so the id alone could now name a
        // folder the user never saw on the list.
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
        if item.kind == Kind::Node && !path.join(DATADIR_MARK).exists() {
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

/// Delete the ticked items of a fresh plan. The node's folder and the moved-aside ones go first, so
/// a failure there leaves the app's settings as they were.
pub fn execute(p: &Places, ticks: &[Tick]) -> Result<Done, String> {
    let (plan, chosen) = choose(p, ticks)?;
    let mut done = Done::default();
    for item in chosen.iter().filter(|i| matches!(i.kind, Kind::Node | Kind::Aside)) {
        remove_item(Path::new(&item.path), &p.home)?;
        done.removed.push(PathBuf::from(&item.path));
    }
    if chosen.iter().any(|i| i.kind == Kind::App) {
        if p.app_dir_shared {
            // A developer's FREEBANK_APP_DIR: FreeBank's own files go, the folder and the rest stay.
            for name in APP_FILES {
                let path = p.app_dir.join(name);
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
                for name in APP_FILES {
                    super::remove_inside(&p.app_dir, &p.app_dir.join(name))?;
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

/// The list the confirm panel shows, with the wallet's balance when the node answers.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    /// The wallets in the node's data folder.
    pub wallets: Vec<String>,
    /// Everything their wallet holds, from getwalletinfo: spendable, unconfirmed and newly mined
    /// coins still maturing. (getbalance, the number the Home screen shows, counts only the first.)
    pub balance: Option<f64>,
    /// How much of `balance` isn't spendable yet (unconfirmed or maturing), when any.
    pub pending: Option<f64>,
    /// Why there is no balance.
    pub balance_note: Option<String>,
    /// Backups made with "Back up wallet first".
    pub backups: Vec<String>,
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

pub async fn plan(mgr: &NodeManager, places: Places) -> Result<Plan, String> {
    mgr.still_here()?;
    let s = mgr.settings.lock().await.clone();
    let activity = mgr.activity.lock().unwrap().clone();
    let blocked = match activity {
        Some(a) => Some(format!("Please wait: {}", a)),
        None => refusal(mgr).await,
    };
    let datadir = places.datadir.clone();
    let items = tokio::task::spawn_blocking(move || plan_items(&places))
        .await
        .map_err(|e| e.to_string())?;
    let wallets: Vec<String> = wallet_files(&datadir)
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect();
    let mut pending = None;
    let (balance, balance_note) = if wallets.is_empty() {
        (None, None)
    } else {
        let probe = detect::probe(&mgr.http, &s).await;
        match probe.state {
            detect::RpcState::Up => match detect::local_client(&mgr.http, &s).call("getwalletinfo", vec![]).await {
                Ok(v) => match holdings(&v) {
                    Some((total, waiting)) => {
                        pending = (waiting > 0.0).then_some(waiting);
                        (Some(total), None)
                    }
                    None => (None, Some("Your node didn't say what the balance is.".to_string())),
                },
                Err(e) => (None, Some(format!("Your node couldn't say what the balance is ({}).", e))),
            },
            detect::RpcState::Down => (None, Some("Your node isn't running, so the balance can't be shown.".into())),
            detect::RpcState::Warming | detect::RpcState::Busy => {
                (None, Some("Your node is still starting, so the balance can't be shown yet.".into()))
            }
            detect::RpcState::Locked => (None, Some(probe.message)),
        }
    };
    Ok(Plan {
        items,
        wallets,
        balance,
        pending,
        balance_note,
        backups: mgr.backups.lock().unwrap().clone(),
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
        backups: mgr.backups.lock().unwrap().clone(),
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
fn wallet_label(datadir: &Path, wallet: &Path) -> String {
    let rel = wallet.strip_prefix(datadir).unwrap_or(wallet).to_string_lossy().into_owned();
    file_safe(rel.strip_suffix(".dat").unwrap_or(&rel))
}

/// Copy a stopped node's wallets into `folder`: the first as <base>.dat, any others with their place
/// in the data folder added. Never overwrites a file.
fn copy_wallets(datadir: &Path, folder: &Path, base: &str) -> Result<Vec<PathBuf>, String> {
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
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| format!("Couldn't write {}: {}", dest.display(), e))?;
        std::io::copy(&mut from, &mut to)
            .and_then(|_| to.sync_all())
            .map_err(|e| format!("Couldn't write {}: {}", dest.display(), e))?;
        saved.push(dest);
    }
    Ok(saved)
}

/// "Back up wallet first": FreeBank-wallet-<local time>.dat in `folder`. A running node writes it
/// itself (RPC backupwallet), so the copy is whole; a stopped node's wallet files are copied. The
/// wallets in folders setup moved aside (no node runs there) are copied too, named after the folder.
pub async fn backup_wallet(mgr: &NodeManager, folder: &Path) -> Result<Vec<String>, String> {
    mgr.still_here()?;
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    let base = format!("FreeBank-wallet-{}", stamp());
    let node_wallets = wallet_files(&datadir).len();
    let moved: Vec<(String, PathBuf)> = asides(&datadir)
        .into_iter()
        .filter(|(_, path)| !wallet_files(path).is_empty())
        .collect();
    if node_wallets == 0 && moved.is_empty() {
        return Err(format!("There's no wallet in {}.", datadir.display()));
    }
    let mut saved = Vec::new();
    if node_wallets > 0 {
        let ours_alive = process::child_alive(mgr).await;
        let probe = detect::probe(&mgr.http, &s).await;
        match probe.state {
            // backupwallet saves the one wallet the node has loaded; the others would be left out.
            detect::RpcState::Up if node_wallets > 1 => {
                return Err(format!(
                    "Your node has {} wallets, and while it runs it can back up only one. Stop it on the Node tab, \
                     then back up again: FreeBank then copies every wallet.",
                    node_wallets
                ))
            }
            detect::RpcState::Up => {
                let dest = free_name(folder, &base);
                detect::local_client(&mgr.http, &s)
                    .call("backupwallet", vec![serde_json::json!(dest.to_string_lossy())])
                    .await
                    .map_err(|e| format!("Your node couldn't back up the wallet ({}).", e.trim_start_matches("RPC error: ")))?;
                if !dest.is_file() {
                    return Err(format!("Your node said it saved the backup, but {} isn't there.", dest.display()));
                }
                saved.push(dest);
            }
            detect::RpcState::Down if !ours_alive => {
                let (d, f, b) = (datadir.clone(), folder.to_path_buf(), base.clone());
                saved.extend(
                    tokio::task::spawn_blocking(move || copy_wallets(&d, &f, &b))
                        .await
                        .map_err(|e| e.to_string())??,
                );
            }
            detect::RpcState::Locked => return Err(format!("{} Stop that node, then back up again.", probe.message)),
            _ => return Err("Your node is still starting. Back up once it's running.".into()),
        }
    }
    for (name, path) in moved {
        // ".freebank.old-1727000000" -> "FreeBank-wallet-<time>-freebank-old-1727000000"
        let named = format!("{}-{}", base, file_safe(name.trim_start_matches('.')));
        let f = folder.to_path_buf();
        saved.extend(
            tokio::task::spawn_blocking(move || copy_wallets(&path, &f, &named))
                .await
                .map_err(|e| e.to_string())??,
        );
    }
    let saved = strings(&saved);
    mgr.backups.lock().unwrap().extend(saved.iter().cloned());
    Ok(saved)
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
        write(&datadir.join(DATADIR_MARK), b"");
        write(&datadir.join("wallet.dat"), &[1u8; 300]);
        write(&datadir.join("blocks/blk00000.dat"), &[2u8; 700]);
        Places {
            home,
            app_dir,
            screen_uses_app_dir: false,
            app_dir_shared: false,
            datadir: datadir.clone(),
            datadir_created: Some(datadir),
            caches: Vec::new(),
            keep: Vec::new(),
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
        p.datadir_created = None;
        let node = find(&plan_items(&p), "node").clone();
        assert!(!node.checked && node.allowed);
        assert!(node.note.contains("didn't create"), "{}", node.note);

        // Neither recorded nor marked: can't be ticked, and execute refuses it.
        std::fs::remove_file(p.datadir.join(DATADIR_MARK)).unwrap();
        let node = find(&plan_items(&p), "node").clone();
        assert!(!node.checked && !node.allowed);
        assert!(execute(&p, &ticks(&p, &["node"])).is_err());
        assert!(p.datadir.join("wallet.dat").exists());

        // Recorded but the mark is gone: another program may have made the folder again (BitWindow's
        // FreeBank uses the same default), so the record alone doesn't let it go.
        p.datadir_created = Some(p.datadir.clone());
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
    fn moved_aside_folders_listed_off() {
        let b = base("aside");
        let p = places(&b);
        let home = &p.home;
        write(&home.join(".freebank.old-1727000000/wallet.dat"), b"w");
        write(&home.join(".freebank.old-1727000001/debug.log"), b"l");
        write(&home.join(".freebank.old-abc/wallet.dat"), b"w");
        write(&home.join(".freebank.old-/wallet.dat"), b"w");
        write(&home.join(".freebank-other/wallet.dat"), b"w");

        let items = plan_items(&p);
        let asides: Vec<&Item> = items.iter().filter(|i| i.kind == Kind::Aside).collect();
        assert_eq!(
            asides.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["aside:.freebank.old-1727000000", "aside:.freebank.old-1727000001"]
        );
        assert!(asides.iter().all(|i| !i.checked && i.allowed));
        assert!(asides[0].note.contains("holds that node's wallet"));
        assert_eq!(asides[0].wallets, vec![home.join(".freebank.old-1727000000/wallet.dat").to_string_lossy()]);
        assert!(asides[1].note.contains("No wallet"));
        assert!(asides[1].wallets.is_empty());
        assert_eq!(find(&items, "node").wallets, vec![p.datadir.join("wallet.dat").to_string_lossy()]);
        assert!(find(&items, "app").wallets.is_empty());

        let done = execute(&p, &ticks(&p, &["aside:.freebank.old-1727000000"])).unwrap();
        assert_eq!(done.removed, vec![home.join(".freebank.old-1727000000")]);
        assert!(!home.join(".freebank.old-1727000000").exists());
        assert!(home.join(".freebank.old-1727000001").exists());
        assert!(home.join(".freebank.old-abc/wallet.dat").exists());
        assert!(p.datadir.join("wallet.dat").exists());
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
        write(&home.join(".freebank.old-1/wallet.dat"), b"w");
        let shown = ticks(&p, &["app", "node", "aside:.freebank.old-1"]);

        // Settings now point at another folder FreeBank once set up, with its own wallet and a
        // moved-aside folder of the same name beside it. The same ids now name those folders.
        let other = home.join("disk/.freebank");
        write(&other.join(DATADIR_MARK), b"");
        write(&other.join("wallet.dat"), b"other");
        write(&home.join("disk/.freebank.old-1/wallet.dat"), b"other aside");
        p.datadir = other.clone();
        for t in &shown[1..] {
            let err = execute(&p, std::slice::from_ref(t)).unwrap_err();
            assert!(err.contains("list has changed"), "{}: {}", t.id, err);
        }
        let err = execute(&p, &shown).unwrap_err();
        assert!(err.contains("list has changed"), "{}", err);
        assert!(other.join("wallet.dat").exists());
        assert!(home.join("disk/.freebank.old-1/wallet.dat").exists());
        assert!(home.join(".freebank/wallet.dat").exists());
        assert!(p.app_dir.join("settings.json").exists());

        // Ticks made from the list as it is now are taken.
        let now = ticks(&p, &["aside:.freebank.old-1"]);
        assert_eq!(now[0].path, home.join("disk/.freebank.old-1").to_string_lossy());
        execute(&p, &now).unwrap();
        assert!(!home.join("disk/.freebank.old-1").exists());
        assert!(home.join(".freebank.old-1/wallet.dat").exists());
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

        let done = execute(&p, &ticks(&p, &["node", "cache:cache"])).unwrap();
        assert!(std::fs::symlink_metadata(&p.datadir).is_err());
        assert!(real.join("wallet.dat").exists() && real.join(DATADIR_MARK).exists());
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
            p.datadir_created = Some(datadir.clone());
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
        p.datadir_created = Some(inner.clone());
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
        let saved = copy_wallets(&p.datadir, &docs, base_name).unwrap();
        assert_eq!(
            saved,
            vec![
                docs.join(format!("{}-2.dat", base_name)),
                docs.join(format!("{}-savings.dat", base_name)),
            ]
        );
        assert_eq!(std::fs::read(&saved[0]).unwrap(), vec![1u8; 300]);
        assert_eq!(std::fs::read(docs.join(format!("{}.dat", base_name))).unwrap(), b"an older backup");
        assert_eq!(wallet_label(&p.datadir, &p.datadir.join("wallets/house/wallet.dat")), "wallets-house-wallet");
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
            datadir_created: p.datadir_created.as_ref().map(|d| d.to_string_lossy().into_owned()),
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
        assert_eq!(*mgr.backups.lock().unwrap(), saved);

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

    #[tokio::test]
    async fn backups_take_moved_aside_wallets_too() {
        let b = base("asidebackup");
        let p = places(&b);
        let docs = p.home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        write(&p.home.join(".freebank.old-1727000000/wallets/wallet.dat"), b"old node's");
        write(&p.home.join(".freebank.old-1727000001/debug.log"), b"no wallet here");
        let mgr = manager(&p);

        // The node is stopped: its wallet is copied, and so is the moved-aside one, named after its folder.
        let saved = backup_wallet(&mgr, &docs).await.unwrap();
        assert_eq!(saved.len(), 2, "{:?}", saved);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), vec![1u8; 300]);
        assert!(saved[1].ends_with("-freebank-old-1727000000.dat"), "{}", saved[1]);
        assert_eq!(std::fs::read(&saved[1]).unwrap(), b"old node's");

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
    }
}
