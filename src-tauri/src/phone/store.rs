//! What the phone relay keeps on disk, all under `<app data>/phone/`, each file mode 0600:
//! - `desktop.key`: the desktop static key D (32-byte scalar, hex);
//! - `devices.json`: allowed phones (name, P_pub, added, last seen, daily limit, today's spend);
//! - `config.json`: the relay URL;
//! - `held.json`: sends waiting for the desktop, and the notices owed to phones for held sends a
//!   restart cancelled;
//! - `sends.log`: one JSON line per phone send (sent, held, declined, failed, expired, cancelled).
//!
//! The wallet passphrase for phone sends is never among them: it lives only in memory.

use super::crypto;
use p256::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The public relay; it also serves the phone page at https://app.ecxfreebank.com/. Settings can
/// point a desktop at another (ws:// only for local testing).
pub const DEFAULT_RELAY: &str = "wss://app.ecxfreebank.com/ws";
/// 0.1 ECX
pub const DEFAULT_LIMIT_SATS: u64 = 10_000_000;
pub const SATS: f64 = 100_000_000.0;
pub const MAX_ECX: f64 = 21_000_000.0;

pub fn day_of(unix: u64) -> u64 {
    unix / 86_400
}

/// ECX (a JSON number) to sats, refusing anything that isn't a sane positive amount.
pub fn to_sats(ecx: f64) -> Result<u64, String> {
    if !ecx.is_finite() || ecx < 0.0 || ecx > MAX_ECX {
        return Err("amount out of range".into());
    }
    Ok((ecx * SATS).round() as u64)
}

/// An ECX amount from the phone (a JSON number, up to 8 decimals, `1e-8` included) to sats,
/// exactly: parsed from the number's decimal text, not by float rounding. More than 8 decimals,
/// negative or over 21 million ECX is refused.
pub fn json_to_sats(v: &serde_json::Value) -> Result<u64, String> {
    let n = v.as_number().ok_or("amount must be a number")?;
    decimal_to_sats(&n.to_string())
}

pub fn decimal_to_sats(s: &str) -> Result<u64, String> {
    let bad = || "amount must be a number of ECX with at most 8 decimals".to_string();
    let s = s.trim();
    if s.starts_with('-') {
        return Err("amount out of range".into());
    }
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().map_err(|_| bad())?),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if int.is_empty() && frac.is_empty() || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    // digits × 10^(exp - frac.len()) ECX = digits × 10^(exp - frac.len() + 8) sats
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let shift = exp - frac.len() as i32 + 8;
    let sats: u128 = if digits.is_empty() {
        0
    } else if shift >= 0 {
        if digits.len() as i32 + shift > 18 {
            return Err("amount out of range".into());
        }
        digits.parse::<u128>().map_err(|_| bad())? * 10u128.pow(shift as u32)
    } else {
        let cut = (-shift) as usize;
        let (keep, drop) = if cut >= digits.len() { ("", digits) } else { digits.split_at(digits.len() - cut) };
        if drop.chars().any(|c| c != '0') {
            return Err(bad());
        }
        if keep.is_empty() { 0 } else { keep.parse::<u128>().map_err(|_| bad())? }
    };
    if sats > (MAX_ECX as u128) * 100_000_000 {
        return Err("amount out of range".into());
    }
    Ok(sats as u64)
}

pub fn to_ecx(sats: u64) -> f64 {
    sats as f64 / SATS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// b64u(SHA-256(P_pub))[0..16]: short, stable, safe to show.
    pub id: String,
    pub name: String,
    /// b64u uncompressed SEC1
    pub p_pub: String,
    pub added: u64,
    pub last_seen: Option<u64>,
    pub limit_sats: u64,
    /// Day (unix days, UTC) the spend below belongs to.
    #[serde(default)]
    pub spent_day: u64,
    /// Sent without asking the desktop on `spent_day`.
    #[serde(default)]
    pub spent_sats: u64,
}

impl Device {
    pub fn id_for(p_pub: &PublicKey) -> String {
        crypto::b64u(&crypto::sha256(&crypto::pub_bytes(p_pub)))[..16].to_string()
    }

    pub fn spent_on(&self, day: u64) -> u64 {
        if self.spent_day == day {
            self.spent_sats
        } else {
            0
        }
    }

    pub fn left_on(&self, day: u64) -> u64 {
        self.limit_sats.saturating_sub(self.spent_on(day))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Devices {
    pub devices: Vec<Device>,
}

impl Devices {
    pub fn by_pub(&self, p_pub: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.p_pub == p_pub)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    /// Take `sats` from the day's allowance if it fits. False means: hold it for the desktop.
    pub fn reserve(&mut self, id: &str, sats: u64, day: u64) -> bool {
        let Some(d) = self.get_mut(id) else { return false };
        if sats > d.left_on(day) {
            return false;
        }
        if d.spent_day != day {
            d.spent_day = day;
            d.spent_sats = 0;
        }
        d.spent_sats += sats;
        true
    }

    /// Give back a reservation whose send failed.
    pub fn release(&mut self, id: &str, sats: u64, day: u64) {
        if let Some(d) = self.get_mut(id) {
            if d.spent_day == day {
                d.spent_sats = d.spent_sats.saturating_sub(sats);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub relay_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { relay_url: DEFAULT_RELAY.into() }
    }
}

/// `held.json`.
#[derive(Default, Serialize, Deserialize)]
pub struct HeldFile {
    #[serde(default)]
    pub held: Vec<super::Held>,
    #[serde(default)]
    pub cancelled: Vec<super::Cancelled>,
}

/// The phone folder. Nothing is written once the app's own folder is gone (after "Obliterate").
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(app_dir: &Path) -> Self {
        Self { dir: app_dir.join("phone") }
    }

    fn ensure_dir(&self) -> Result<(), String> {
        let parent = self.dir.parent().ok_or("no app folder")?;
        if !parent.is_dir() {
            return Err("the app's folder is gone".into());
        }
        if !self.dir.is_dir() {
            std::fs::create_dir(&self.dir).map_err(|e| format!("create {}: {e}", self.dir.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        Ok(())
    }

    /// Write via a temp file and rename, mode 0600.
    fn write_private(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.ensure_dir()?;
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
    }

    fn read_json<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        std::fs::read(self.dir.join(name))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The desktop static key D: read it, or make it once.
    pub fn desktop_key(&self) -> Result<SecretKey, String> {
        let path = self.dir.join("desktop.key");
        if let Ok(s) = std::fs::read_to_string(&path) {
            let b = hex::decode(s.trim()).map_err(|_| format!("{} is damaged", path.display()))?;
            return SecretKey::from_slice(&b).map_err(|_| format!("{} is damaged", path.display()));
        }
        let k = crypto::random_secret();
        self.write_private("desktop.key", hex::encode(k.to_bytes()).as_bytes())?;
        Ok(k)
    }

    pub fn load_devices(&self) -> Devices {
        self.read_json("devices.json")
    }

    pub fn save_devices(&self, d: &Devices) -> Result<(), String> {
        self.write_private("devices.json", &serde_json::to_vec_pretty(d).unwrap())
    }

    pub fn load_config(&self) -> Config {
        self.read_json("config.json")
    }

    pub fn save_config(&self, c: &Config) -> Result<(), String> {
        self.write_private("config.json", &serde_json::to_vec_pretty(c).unwrap())
    }

    pub fn load_held(&self) -> HeldFile {
        self.read_json("held.json")
    }

    pub fn save_held(&self, h: &HeldFile) -> Result<(), String> {
        self.write_private("held.json", &serde_json::to_vec_pretty(h).unwrap())
    }

    /// Append one line to sends.log.
    pub fn log_send(&self, entry: &serde_json::Value) {
        if self.ensure_dir().is_err() {
            return;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        if let Ok(mut f) = opts.open(self.dir.join("sends.log")) {
            let _ = writeln!(f, "{}", entry);
        }
    }

    /// The last `n` lines of sends.log, newest first.
    pub fn recent_sends(&self, n: usize) -> Vec<serde_json::Value> {
        let s = std::fs::read_to_string(self.dir.join("sends.log")).unwrap_or_default();
        s.lines().rev().filter_map(|l| serde_json::from_str(l).ok()).take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(limit: u64) -> Devices {
        Devices {
            devices: vec![Device {
                id: "a".into(),
                name: "n".into(),
                p_pub: "p".into(),
                added: 0,
                last_seen: None,
                limit_sats: limit,
                spent_day: 0,
                spent_sats: 0,
            }],
        }
    }

    #[test]
    fn ledger_reserves_releases_and_rolls_over() {
        let mut d = dev(DEFAULT_LIMIT_SATS);
        assert!(d.reserve("a", 6_000_000, 100));
        assert!(!d.reserve("a", 5_000_000, 100), "over what is left");
        assert!(d.reserve("a", 4_000_000, 100), "exactly what is left");
        assert_eq!(d.devices[0].left_on(100), 0);
        d.release("a", 4_000_000, 100);
        assert_eq!(d.devices[0].left_on(100), 4_000_000);
        // A new day starts from the full limit.
        assert_eq!(d.devices[0].left_on(101), DEFAULT_LIMIT_SATS);
        assert!(d.reserve("a", DEFAULT_LIMIT_SATS, 101));
        // Releasing yesterday's reservation doesn't touch today's.
        d.release("a", 1, 100);
        assert_eq!(d.devices[0].left_on(101), 0);
        assert!(!d.reserve("zz", 1, 101), "unknown device");
    }

    #[test]
    fn zero_limit_holds_everything() {
        let mut d = dev(0);
        assert!(!d.reserve("a", 1, 5));
    }

    #[test]
    fn exact_amounts() {
        let j = |s: &str| json_to_sats(&serde_json::from_str(s).unwrap());
        assert_eq!(j("1e-8").unwrap(), 1);
        assert_eq!(j("0.00000001").unwrap(), 1);
        assert_eq!(j("0.1").unwrap(), 10_000_000);
        assert_eq!(j("0.05").unwrap(), 5_000_000);
        assert_eq!(j("1.23456789").unwrap(), 123_456_789);
        assert_eq!(j("12").unwrap(), 1_200_000_000);
        assert_eq!(j("2.5e-3").unwrap(), 250_000);
        assert_eq!(j("21000000").unwrap(), 2_100_000_000_000_000);
        assert_eq!(j("0").unwrap(), 0);
        assert!(j("1e-9").is_err());
        assert!(j("0.123456789").is_err());
        assert!(j("-1").is_err());
        assert!(j("21000000.00000001").is_err());
        assert!(j("1e30").is_err());
        assert!(j("\"0.1\"").is_err());
        assert!(decimal_to_sats(".").is_err());
        assert!(decimal_to_sats("1e").is_err());
    }

    #[test]
    fn amounts() {
        assert_eq!(to_sats(0.1).unwrap(), 10_000_000);
        assert_eq!(to_sats(0.00000001).unwrap(), 1);
        assert!(to_sats(f64::NAN).is_err());
        assert!(to_sats(-1.0).is_err());
        assert!(to_sats(22_000_000.0).is_err());
    }

    #[test]
    fn key_is_made_once_and_private() {
        let tmp = std::env::temp_dir().join(format!("fb-phone-store-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let s = Store::new(&tmp);
        let k1 = s.desktop_key().unwrap();
        let k2 = s.desktop_key().unwrap();
        assert_eq!(k1.to_bytes(), k2.to_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(s.dir.join("desktop.key")).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn nothing_written_without_the_app_folder() {
        let s = Store::new(Path::new("/nonexistent-fb-app-dir"));
        assert!(s.desktop_key().is_err());
    }
}
