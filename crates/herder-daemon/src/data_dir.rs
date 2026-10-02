//! The daemon's data directory: its layout, the single-instance lock and the host id.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

const LOCK_FILE: &str = "daemon.lock";
const HOST_ID_FILE: &str = "host-id";
const SUBDIRS: [&str; 3] = ["db", "tls", "sessions"];

/// An opened data directory. Holds an exclusive lock on it until dropped.
#[derive(Debug)]
pub struct DataDir {
    root: PathBuf,
    host_id: Uuid,
    _lock: File,
}

impl DataDir {
    /// Creates the layout under `root` if needed, takes the single-instance lock and loads (or
    /// creates, once) the host id.
    pub fn open(root: &Path) -> Result<Self> {
        create_private_dir(root)?;
        let lock = lock(root)?;
        for sub in SUBDIRS {
            create_private_dir(&root.join(sub))?;
        }
        let host_id = host_id(root)?;
        Ok(Self {
            root: root.to_owned(),
            host_id,
            _lock: lock,
        })
    }

    /// The data directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This host's stable id.
    pub fn host_id(&self) -> Uuid {
        self.host_id
    }
}

/// Creates `dir` (and parents) readable by this user only; it will hold keys and transcripts.
fn create_private_dir(dir: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Takes an exclusive lock on `<root>/daemon.lock` and records our PID in it.
///
/// The OS releases the lock when the process exits, however it exits, so a stale file never
/// blocks a restart. std opens the file close-on-exec, so children we spawn never hold it once
/// they exec; one forked by another thread does share it until its exec, which is why dropping a
/// `DataDir` may not free the lock the same instant.
fn lock(root: &Path) -> Result<File> {
    let path = root.join(LOCK_FILE);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut pid = String::new();
            // Best effort: the PID only makes the message more useful.
            let _ = file.read_to_string(&mut pid);
            let holder = match pid.trim() {
                "" => String::new(),
                pid => format!(" (pid {pid})"),
            };
            bail!(
                "another herder daemon{holder} is already using the data dir {}",
                root.display()
            );
        }
        Err(TryLockError::Error(err)) => {
            return Err(err).with_context(|| format!("locking {}", path.display()));
        }
    }
    file.set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|()| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(file)
}

/// Reads `<root>/host-id`, generating and atomically writing a UUIDv7 the first time.
///
/// Callers hold the data-dir lock, so no other daemon races the write.
fn host_id(root: &Path) -> Result<Uuid> {
    let path = root.join(HOST_ID_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => {
            return text
                .trim()
                .parse()
                .with_context(|| format!("{} does not hold a valid host id", path.display()));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    }
    let id = Uuid::now_v7();
    let tmp = root.join(format!(".{HOST_ID_FILE}.tmp"));
    write_synced(&tmp, format!("{id}\n").as_bytes())
        .and_then(|()| fs::rename(&tmp, &path))
        .and_then(|()| File::open(root)?.sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(id)
}

/// Atomically writes `dir/name` readable by this user only; the caller syncs `dir` afterwards
/// when the rename must survive a crash.
pub(crate) fn write_private(dir: &Path, name: &str, contents: &[u8]) -> Result<()> {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp"));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, &path))
        .with_context(|| format!("writing {}", path.display()))
}

fn write_synced(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    /// Reopens `root` after an earlier `DataDir` on it was dropped. Other tests in this binary
    /// spawn processes, and a child sits on a copy of the lock fd between its fork and its exec,
    /// so the lock can outlive the drop by that long. Waiting for it is the deterministic check;
    /// only an fd that survives exec holds it past the deadline.
    fn reopen(root: &Path) -> DataDir {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match DataDir::open(root) {
                Ok(dir) => return dir,
                Err(err)
                    if err.to_string().contains("already using") && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("reopening {}: {err:#}", root.display()),
            }
        }
    }

    #[test]
    fn creates_layout_and_host_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("nested/herder");
        let dir = DataDir::open(&root).unwrap();
        assert_eq!(dir.root(), root);
        for sub in SUBDIRS {
            assert!(root.join(sub).is_dir(), "{sub} missing");
        }
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(dir.host_id().get_version_num(), 7);
        let written = fs::read_to_string(root.join(HOST_ID_FILE)).unwrap();
        assert_eq!(written.trim(), dir.host_id().to_string());
        let pid = fs::read_to_string(root.join(LOCK_FILE)).unwrap();
        assert_eq!(pid.trim(), std::process::id().to_string());
    }

    #[test]
    fn host_id_is_stable_across_restarts() {
        let tmp = tempfile::tempdir().unwrap();
        let first = DataDir::open(tmp.path()).unwrap().host_id();
        let second = reopen(tmp.path()).host_id();
        assert_eq!(first, second);
    }

    #[test]
    fn corrupt_host_id_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(HOST_ID_FILE), "garbage").unwrap();
        let err = DataDir::open(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("valid host id"), "{err:#}");
    }

    #[test]
    fn lock_prevents_a_second_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let first = DataDir::open(tmp.path()).unwrap();
        let err = DataDir::open(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("already using the data dir"), "{err}");
        assert!(
            err.contains(&format!("pid {}", std::process::id())),
            "{err}"
        );
        drop(first);
        reopen(tmp.path());
    }

    #[test]
    fn lock_fd_is_close_on_exec() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::open(tmp.path()).unwrap();
        let fdinfo =
            fs::read_to_string(format!("/proc/self/fdinfo/{}", dir._lock.as_raw_fd())).unwrap();
        let flags = fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("flags:"))
            .unwrap();
        let flags = i32::from_str_radix(flags.trim(), 8).unwrap();
        assert_ne!(flags & nix::libc::O_CLOEXEC, 0, "{fdinfo}");
    }

    #[test]
    fn spawned_child_does_not_hold_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::open(tmp.path()).unwrap();
        // `spawn` returns once the child has exec'd, so it holds the fd only if it leaked.
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        drop(dir);
        reopen(tmp.path());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
