//! Is a FreeBank node using a folder? freebankd (Core 0.16) holds an fcntl write lock on `.lock` in
//! its data folder and on `.walletlock` in its wallet folder for as long as it runs (`LockDirectory`,
//! through boost's file_lock), whatever port it answers on. The app asks the kernel whether it could
//! take that lock (F_GETLK). That never takes the lock, so nothing here can stop a node starting.

use std::path::{Path, PathBuf};

/// A folder a node is using, and the process that holds its lock (when the system says).
#[derive(Debug, Clone, PartialEq)]
pub struct InUse {
    pub folder: PathBuf,
    pub pid: Option<u32>,
}

impl InUse {
    /// "A FreeBank node (process 1234) is using /home/x/.freebank."
    pub fn say(&self) -> String {
        match self.pid {
            Some(pid) => format!("A FreeBank node (process {}) is using {}.", pid, self.folder.display()),
            None => format!("A FreeBank node is using {}.", self.folder.display()),
        }
    }
}

/// Who holds a lock on `file`: Ok(None) when nobody does or there is no such file, Ok(Some(pid))
/// when a process does (pid None when the system doesn't say which), Err when the file can't be
/// looked at.
#[cfg(unix)]
pub fn holder(file: &Path) -> Result<Option<Option<u32>>, String> {
    use std::os::unix::io::AsRawFd;
    let f = match std::fs::File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {}", file.display(), e)),
    };
    // SAFETY: flock is plain data; F_GETLK only writes into the struct we own.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = 0;
    fl.l_len = 0;
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) } == -1 {
        return Err(format!("{}: {}", file.display(), std::io::Error::last_os_error()));
    }
    if fl.l_type == libc::F_UNLCK as libc::c_short {
        return Ok(None);
    }
    Ok(Some((fl.l_pid > 0).then_some(fl.l_pid as u32)))
}

#[cfg(not(unix))]
pub fn holder(_file: &Path) -> Result<Option<Option<u32>>, String> {
    Ok(None)
}

/// The process holding `<datadir>/.lock`, if one does and the system says which.
pub fn datadir_holder(datadir: &Path) -> Option<u32> {
    holder(&datadir.join(".lock")).ok().flatten().flatten()
}

/// Is a node other than `ours` using `datadir`: its `.lock`, or `.walletlock` in any folder its
/// wallets may be in? A lock that can't be looked at counts as in use.
pub fn in_use(datadir: &Path, ours: &[u32]) -> Option<InUse> {
    let places = std::iter::once((datadir.to_path_buf(), ".lock"))
        .chain(super::wallet_dirs(datadir).into_iter().map(|d| (d, ".walletlock")));
    for (folder, name) in places {
        match holder(&folder.join(name)) {
            Ok(None) => {}
            Ok(Some(Some(pid))) if ours.contains(&pid) => {}
            Ok(Some(pid)) => return Some(InUse { folder, pid }),
            Err(_) => return Some(InUse { folder, pid: None }),
        }
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testnode;
    use super::*;

    #[test]
    fn a_running_node_is_seen_by_its_lock() {
        let d = testnode::temp("lock");
        let datadir = d.join("node");
        std::fs::create_dir_all(datadir.join("wallets")).unwrap();
        // Nothing yet: no lock files at all, then lock files nobody holds.
        assert_eq!(holder(&datadir.join(".lock")), Ok(None));
        assert_eq!(in_use(&datadir, &[]), None);
        std::fs::write(datadir.join(".lock"), b"").unwrap();
        std::fs::write(datadir.join("wallets/.walletlock"), b"").unwrap();
        assert_eq!(in_use(&datadir, &[]), None);

        // A node on some port holds both.
        let node = testnode::FakeNode::spawn(&datadir, testnode::free_port(), testnode::Opts::default());
        let pid = node.pid();
        assert_eq!(holder(&datadir.join(".lock")), Ok(Some(Some(pid))));
        assert_eq!(datadir_holder(&datadir), Some(pid));
        assert_eq!(in_use(&datadir, &[]), Some(InUse { folder: datadir.clone(), pid: Some(pid) }));
        assert!(in_use(&datadir, &[]).unwrap().say().contains(&format!("process {}", pid)));
        // The app's own node doesn't count.
        assert_eq!(in_use(&datadir, &[pid]), None);
        // Looking never takes the lock: the node still holds it.
        assert_eq!(datadir_holder(&datadir), Some(pid));
        drop(node);
        assert_eq!(in_use(&datadir, &[]), None);

        // Only the wallet folder's lock held (a node with this folder as its walletdir).
        let node = testnode::FakeNode::spawn(
            &d.join("other"),
            testnode::free_port(),
            testnode::Opts { walletlock: Some(datadir.join("wallets")), ..Default::default() },
        );
        assert_eq!(
            in_use(&datadir, &[]),
            Some(InUse { folder: datadir.join("wallets"), pid: Some(node.pid()) })
        );
        drop(node);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
